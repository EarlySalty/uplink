//! Nur autorisierte Bestandsmetadaten für die exakte Ausgangshost-Freigabe.
//! Keine Entschlüsselung von Plattformkeys, keine Veröffentlichung, keine DB-Schreibzugriffe.
use serde_json::json;
use std::{path::Path, process::ExitCode};
use uplink_service::{config::Config, secrets::SecretReader, store::Store};

async fn run() -> Result<(), &'static str> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--config")) {
        return Err("Normale Konfiguration muss ausdrücklich angegeben werden.");
    }
    let path = arguments.next().ok_or("Konfigurationspfad fehlt.")?;
    if arguments.next().is_some() || !Path::new(&path).is_absolute() {
        return Err("Argumente sind ungültig.");
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "Konfiguration fehlt.")?;
    if !metadata.is_file() || metadata.len() > 65536 {
        return Err("Konfiguration ist ungültig.");
    }
    let contents = std::fs::read_to_string(path).map_err(|_| "Konfiguration ist nicht lesbar.")?;
    let config = Config::parse(&contents)?;
    uplink_service::secrets::protect_configured_fds(&config)?;
    let secrets = SecretReader::new(&config).await?.fetch().await?;
    let store = Store::connect(&secrets.database, config.database_max_queries).await?;
    let rows = store.query("SELECT d.platform,d.rtmp_url FROM relay.destinations d JOIN relay.users u ON u.streamer_id=d.streamer_id WHERE u.enabled=true ORDER BY d.platform LIMIT 129", &[]).await?;
    if rows.len() > 128 {
        return Err("Zu viele Ziele für diesen begrenzten Bestandsnachweis.");
    }
    let mut targets = Vec::new();
    for row in rows {
        let platform: String = row
            .try_get(0)
            .map_err(|_| "Plattformdaten sind ungültig.")?;
        if !matches!(platform.as_str(), "twitch" | "kick" | "youtube" | "tiktok") {
            return Err("Unbekannte Plattform im Bestand.");
        }
        let endpoint = zeroize::Zeroizing::new(
            row.try_get::<_, String>(1)
                .map_err(|_| "Zielmetadaten sind ungültig.")?,
        );
        let url = reqwest::Url::parse(&endpoint).map_err(|_| "Zielmetadaten sind ungültig.")?;
        if !matches!(url.scheme(), "rtmp" | "rtmps") {
            return Err("Zieltransport ist ungültig.");
        }
        targets.push(json!({"platform": platform, "scheme": url.scheme(), "host": url.host_str().ok_or("Zielhost fehlt.")?, "port": url.port().unwrap_or(if url.scheme()=="rtmps" {443} else {1935})}));
    }
    println!("{}", json!({"targets":targets}));
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
