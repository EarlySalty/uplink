use std::{path::PathBuf, sync::Arc};
use uplink_service::{api::ServiceState, config::Config, registry::Registry, store::Store};

#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), &'static str> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--config")) {
        return Err(
            "Aufruf: uplink-service --config <Datei> [--check-config | --migrate | --ingest-test]",
        );
    }
    let path = PathBuf::from(args.next().ok_or("Konfigurationsdatei fehlt.")?);
    let mode = args.next();
    if args.next().is_some()
        || mode.as_deref().is_some_and(|value| {
            value != std::ffi::OsStr::new("--check-config")
                && value != std::ffi::OsStr::new("--ingest-test")
                && value != std::ffi::OsStr::new("--migrate")
        })
    {
        return Err(
            "Aufruf: uplink-service --config <Datei> [--check-config | --migrate | --ingest-test]",
        );
    }
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| "Konfigurationsdatei ist nicht lesbar.")?;
    let mut input = String::new();
    file.take(64 * 1024 + 1)
        .read_to_string(&mut input)
        .await
        .map_err(|_| "Konfigurationsdatei ist ungültig.")?;
    let config = Config::parse(&input)?;
    if mode.as_deref() == Some(std::ffi::OsStr::new("--ingest-test"))
        && (config.test_ingest.is_none() || !config.platforms.is_empty())
    {
        return Err("Teststart benötigt eine einzelne Konto-Freigabe und keine Plattformausgänge.");
    }
    if mode.as_deref() == Some(std::ffi::OsStr::new("--check-config")) {
        println!(
            "Konfiguration ist gültig. Zugänge, TLS, Datenbank und Medien wurden dadurch nicht geprüft."
        );
        return Ok(());
    }
    if config.test_ingest.is_some()
        && mode.as_deref() != Some(std::ffi::OsStr::new("--ingest-test"))
    {
        return Err("Ein reiner Testeingang darf nicht als regulärer Uplink-Dienst starten.");
    }
    uplink_service::secrets::protect_configured_fds(&config)?;
    let reader = uplink_service::secrets::SecretReader::new(&config).await?;
    let secrets = Arc::new(reader.fetch().await?);
    if mode.as_deref() == Some(std::ffi::OsStr::new("--migrate")) {
        let store = Store::connect(&secrets.database, config.database_max_queries).await?;
        uplink_service::migrations::apply(&store).await?;
        println!("Uplink-Zielgeneration und Audiowahl sind migriert. Kein Listener gestartet.");
        return Ok(());
    }
    let resolver = if matches!(
        config.tls,
        uplink_service::config::TlsConfig::Infisical { .. }
    ) {
        let mut roots = uplink_ingest::rustls::RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        );
        if let Some(path) = &config.loopback_test_ca {
            let mut certificate = Vec::new();
            tokio::fs::File::open(path)
                .await
                .map_err(|_| "Lokale Test-CA fehlt.")?
                .take(64 * 1024 + 1)
                .read_to_end(&mut certificate)
                .await
                .map_err(|_| "Lokale Test-CA ist unlesbar.")?;
            if certificate.len() > 64 * 1024 {
                return Err("Lokale Test-CA ist zu groß.");
            }
            use uplink_ingest::rustls::pki_types::{CertificateDer, pem::PemObject};
            for cert in CertificateDer::pem_slice_iter(&certificate) {
                roots
                    .add(cert.map_err(|_| "Lokale Test-CA ist ungültig.")?)
                    .map_err(|_| "Lokale Test-CA ist ungültig.")?;
            }
        }
        let host = reqwest::Url::parse(&config.public_ingest_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .ok_or("Öffentlicher TLS-Hostname fehlt.")?;
        let resolver = uplink_service::tls_reload::ReloadingTls::new(host, roots)?;
        let (cert, key) = secrets.tls_material.as_ref().ok_or("TLS-Material fehlt.")?;
        resolver.install(cert, key)?;
        Some(resolver)
    } else {
        None
    };
    let tls = match &resolver {
        Some(resolver) => resolver.server_config()?,
        None => uplink_service::secrets::tls(&config, &secrets).await?,
    };
    let reload_seconds = config.tls_reload_seconds;
    let store = Arc::new(Store::connect(&secrets.database, config.database_max_queries).await?);
    let registry = Registry::new(config.max_sessions, config.max_sessions_per_tenant)?;
    let chat = config
        .chat
        .as_ref()
        .map(|chat| {
            let identities = Arc::new(uplink_service::chat::StoredDockIdentity(store.clone()));
            let broker = Arc::new(uplink_service::chat::BotBroker::new(
                &chat.bot_base_url,
                uplink_service::crypto::Secret::new(secrets.bot_internal.expose().to_vec()),
            )?);
            uplink_chat::ChatHub::new(
                uplink_chat::ChatConfig {
                    allowed_origins: chat.allowed_origins.clone(),
                    max_users: chat.max_users,
                    max_sockets_per_user: chat.max_sockets_per_user,
                    idle_timeout: std::time::Duration::from_secs(chat.idle_timeout_seconds),
                },
                identities,
                broker,
            )
        })
        .transpose()?;
    let state = Arc::new(ServiceState {
        chat,
        config,
        store,
        secrets,
        registry,
        tls: resolver.clone(),
    });
    let processor = Arc::new(uplink_service::media::Coordinator::new(state.clone())?);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| "Dienststoppsignal konnte nicht eingerichtet werden.")?;
    let (ready, bound) = tokio::sync::oneshot::channel();
    let service = uplink_service::runtime::serve_with_ready(
        state,
        tls,
        processor,
        async move {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        },
        Some(ready),
    );
    tokio::pin!(service);
    tokio::select! {
        result=&mut service=>return result,
        addresses=bound=>{let (api,ingest)=addresses.map_err(|_|"Dienst konnte nicht starten.")?;println!("Uplink gestartet: {api} {ingest}");}
    }
    let refresh = async {
        let Some(resolver) = resolver else {
            std::future::pending::<()>().await;
            return;
        };
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(reload_seconds));
        interval.tick().await;
        let mut failed = false;
        loop {
            interval.tick().await;
            let result = match reader.fetch().await {
                Ok(values) => match values.tls_material {
                    Some((cert, key)) => resolver.install(&cert, &key),
                    None => Err("TLS-Material fehlt."),
                },
                Err(error) => Err(error),
            };
            resolver.mark_refresh_failure(result.is_err());
            match (result.is_err(), failed) {
                (true, false) => eprintln!(
                    "TLS-Aktualisierung fehlgeschlagen; neue Verbindungen benötigen einen weiterhin gültigen Stand."
                ),
                (false, true) => eprintln!("TLS-Aktualisierung wieder verfügbar."),
                _ => {}
            }
            failed = result.is_err();
        }
    };
    tokio::select! { result = service => result, _ = refresh => Err("TLS-Aktualisierung wurde unerwartet beendet.") }
}
