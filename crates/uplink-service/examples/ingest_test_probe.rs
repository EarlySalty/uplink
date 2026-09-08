//! Explicit, authorized input-only test. All real credentials stay in Rust RAM.
use std::{path::PathBuf, sync::Arc, time::Duration};
use uplink_media::{
    MediaLimits, PublishSecret, PublishTarget, flv::FlvReader, pusher::RunningPusher,
};
use uplink_service::{
    config::Config,
    secrets::{SecretReader, protect_configured_fds},
    store::Store,
};

#[tokio::main]
async fn main() {
    if let Err(error) = tokio::time::timeout(Duration::from_secs(45), run())
        .await
        .map_err(|_| "Testfrist überschritten.")
        .and_then(|result| result)
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), &'static str> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (mode, path, output) = match args.as_slice() {
        [mode, path] if mode == "--check" => ("check", PathBuf::from(path), None),
        [mode, path, output] if mode == "--prepare" => {
            ("prepare", PathBuf::from(path), Some(PathBuf::from(output)))
        }
        _ => {
            return Err(
                "Aufruf: ingest_test_probe --prepare <Vorlage> <neue Config> | --check <Config>",
            );
        }
    };
    use tokio::io::AsyncReadExt;
    let mut template = String::new();
    tokio::fs::File::open(path)
        .await
        .map_err(|_| "Testkonfiguration fehlt.")?
        .take(64 * 1024 + 1)
        .read_to_string(&mut template)
        .await
        .map_err(|_| "Testkonfiguration ist unlesbar.")?;
    if template.len() > 64 * 1024
        || (mode == "prepare" && template.matches("TEST_STREAMER_ID").count() != 1)
    {
        return Err("Testvorlage ist ungültig.");
    }
    let config = Config::parse(&template.replace("TEST_STREAMER_ID", "1"))?;
    let scope = config
        .test_ingest
        .as_ref()
        .ok_or("Nur isolierter Testmodus ist erlaubt.")?;
    if !config.platforms.is_empty() {
        return Err("Testkonfiguration darf keine Ausgänge enthalten.");
    }
    protect_configured_fds(&config)?;
    let secrets = SecretReader::new(&config).await?.fetch().await?;
    let store = Store::connect(&secrets.database, config.database_max_queries).await?;
    if let Some(output) = output {
        let rows = store.query("SELECT d.streamer_id FROM relay.destinations d JOIN relay.users u ON u.streamer_id=d.streamer_id WHERE d.platform='twitch' AND d.enabled=true AND u.enabled=true ORDER BY d.streamer_id LIMIT 2",&[]).await?;
        if rows.len() != 1 {
            return Err("Testkonto ist nicht eindeutig freigegeben.");
        }
        let id: i64 = rows[0]
            .try_get(0)
            .map_err(|_| "Testidentität ist ungültig.")?;
        if id <= 0 {
            return Err("Testidentität ist ungültig.");
        }
        let rendered = template.replace("TEST_STREAMER_ID", &id.to_string());
        Config::parse(&rendered)?;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)
            .map_err(|_| "Neue Testkonfiguration konnte nicht angelegt werden.")?;
        std::io::Write::write_all(&mut file, rendered.as_bytes())
            .map_err(|_| "Testkonfiguration konnte nicht geschrieben werden.")?;
        file.sync_all()
            .map_err(|_| "Testkonfiguration konnte nicht bestätigt werden.")?;
        println!("Testkonfiguration für genau ein autorisiertes Konto angelegt.");
        return Ok(());
    }
    let id =
        i64::try_from(scope.allowed_streamer_ids[0]).map_err(|_| "Testidentität ist ungültig.")?;
    let rows = store
        .query(
            "SELECT ingest_key_enc FROM relay.users WHERE streamer_id=$1 AND enabled=true",
            &[&id],
        )
        .await?;
    let row = rows.first().ok_or("Testkonto ist nicht freigegeben.")?;
    let ciphertext: Vec<u8> = row.try_get(0).map_err(|_| "Testzugang fehlt.")?;
    let key = secrets
        .encryption
        .open(&ciphertext, &format!("ingest_key:{id}"))?;
    let host = reqwest::Url::parse(&config.public_ingest_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .ok_or("Testadresse ist ungültig.")?;
    let roots = uplink_ingest::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    let tls = Arc::new(
        uplink_ingest::rustls::ClientConfig::builder_with_provider(Arc::new(
            uplink_ingest::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS-Konfiguration ist ungültig.")?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    );
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "Statusclient fehlt.")?;
    let mut previous_generation = None;
    for (label, data) in [
        (
            "H.264",
            include_bytes!("../../../experiments/scuffle-probe/fixtures/h264.flv").as_slice(),
        ),
        (
            "AV1",
            include_bytes!("../../../experiments/scuffle-probe/fixtures/av1.flv").as_slice(),
        ),
    ] {
        let target = PublishTarget {
            id: "uplink-ingest-test".into(),
            endpoint: config.public_ingest_url.clone(),
            playpath: PublishSecret::new(key.expose().to_vec())
                .map_err(|_| "Testzugang ist ungültig.")?,
            tls: Some(tls.clone()),
            allowed_hosts: vec![host.clone()],
            allow_loopback: false,
            allow_unencrypted: false,
        };
        let pusher = RunningPusher::start(target, MediaLimits::default())
            .await
            .map_err(|_| "Autorisierter RTMPS-Teststart fehlgeschlagen.")?;
        let mut reader = FlvReader::new(data, 2 * 1024 * 1024);
        let mut expected_events = 0_u64;
        let mut expected_bytes = 0_u64;
        while let Some(tag) = reader
            .next()
            .await
            .map_err(|_| "Künstliches Testmedium ist ungültig.")?
        {
            if !matches!(tag.kind(), 8 | 9) {
                continue;
            }
            expected_events += 1;
            expected_bytes += tag.body().len() as u64;
            pusher
                .try_send(Arc::new(tag))
                .map_err(|_| "Testsendebudget ausgeschöpft.")?;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        pusher
            .finish()
            .await
            .map_err(|_| "RTMPS-Test konnte nicht sauber schließen.")?;
        let mut confirmed = false;
        for _ in 0..50 {
            let mut response = http
                .get(format!("http://{}/v1/me/status", config.api_bind))
                .query(&[("streamer_id", id.to_string())])
                .header("X-Relay-Auth", secrets.api.expose())
                .send()
                .await
                .map_err(|_| "Teststatus ist nicht erreichbar.")?;
            if !response.status().is_success() {
                return Err("Teststatus wurde nicht autorisiert.");
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "Teststatus ist ungültig.")?
            {
                if body.len().saturating_add(chunk.len()) > 64 * 1024 {
                    return Err("Teststatus überschreitet das Lesebudget.");
                }
                body.extend_from_slice(&chunk);
            }
            let value: serde_json::Value =
                serde_json::from_slice(&body).map_err(|_| "Teststatus ist ungültig.")?;
            if let Some(status) = value["sessions"].as_array().and_then(|rows| rows.first())
                && status["ingest_end_reason"] == "ExplicitStop"
                && status["received_events"] == expected_events
                && status["received_bytes"] == expected_bytes
                && status["error"].is_null()
                && status["source_observation"]["mode"] == "ingest_test"
                && status["outputs"]["publishing"] == false
                && status["source_observation"]["generation"].is_string()
                && previous_generation.as_ref() != Some(&status["source_observation"]["generation"])
            {
                previous_generation = Some(status["source_observation"]["generation"].clone());
                println!(
                    "{label}: {expected_events} Medienereignisse und {expected_bytes} Bytes über verifiziertes RTMPS bestätigt; keine Plattformausgabe."
                );
                confirmed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !confirmed {
            return Err("Empfangsstatus stimmt nicht mit dem Testmedium überein.");
        }
    }
    Ok(())
}
