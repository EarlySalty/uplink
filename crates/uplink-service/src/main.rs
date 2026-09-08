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
        return Err("Aufruf: uplink-service --config <Datei> [--check-config]");
    }
    let path = PathBuf::from(args.next().ok_or("Konfigurationsdatei fehlt.")?);
    let mode = args.next();
    if args.next().is_some()
        || mode
            .as_deref()
            .is_some_and(|value| value != std::ffi::OsStr::new("--check-config"))
    {
        return Err("Aufruf: uplink-service --config <Datei> [--check-config]");
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
    if mode.is_some() {
        println!(
            "Konfiguration ist gültig. Zugänge, TLS, Datenbank und Medien wurden dadurch nicht geprüft."
        );
        return Ok(());
    }
    uplink_service::secrets::protect_configured_fds(&config)?;
    let secrets = Arc::new(uplink_service::secrets::fetch(&config).await?);
    let tls = uplink_service::secrets::tls(&config, &secrets).await?;
    let store = Arc::new(Store::connect(&secrets.database, config.database_max_queries).await?);
    let registry = Registry::new(config.max_sessions, config.max_sessions_per_tenant)?;
    let state = Arc::new(ServiceState {
        config,
        store,
        secrets,
        registry,
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
    service.await
}
