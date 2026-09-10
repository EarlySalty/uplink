//! Read-only Idee-1-Abnahme für die vorher autorisierte Twitch-ID EarlySaltys.
//! Bestehendes Credential auf FD 5; keine Argumente, ENV-Konfiguration oder Dateien mit Secrets.
//! Nur nach unabhängigem Quellreview ausführen. Keine Publish-/Broker-/Schreibaufrufe.
use serde_json::{Value, json};
use std::{io::Write, time::Duration};
use tokio::io::AsyncReadExt;
use uplink_service::{
    config::{Config, TlsConfig},
    secrets::{SecretReader, protect_configured_fds},
};

const STREAMER_ID: u64 = 1_186_925_760;
const CONFIG_PATH: &str = "/home/nathanael/.config/uplink/uplink.toml";
const RESPONSE_LIMIT: usize = 512 * 1024;
type Result<T> = std::result::Result<T, &'static str>;

#[tokio::main]
async fn main() {
    let result = tokio::time::timeout(Duration::from_secs(20), run()).await;
    let result = match result {
        Ok(result) => result,
        Err(_) => Err("LIVE_STATUS_TIMEOUT"),
    };
    if let Err(code) = result {
        // Alle Fehlerpfade liefern ausschließlich feste Codes, nie Bibliotheksfehler.
        let _ = writeln!(std::io::stderr().lock(), "{code}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    if std::env::args_os().len() != 1 {
        return Err("LIVE_STATUS_ARGUMENTS");
    }
    let mut config_text = String::new();
    tokio::fs::File::open(CONFIG_PATH)
        .await
        .map_err(|_| "LIVE_STATUS_CONFIG")?
        .take(64 * 1024 + 1)
        .read_to_string(&mut config_text)
        .await
        .map_err(|_| "LIVE_STATUS_CONFIG")?;
    if config_text.len() > 64 * 1024 {
        return Err("LIVE_STATUS_CONFIG");
    }
    let config = Config::parse(&config_text).map_err(|_| "LIVE_STATUS_CONFIG")?;
    validate_config(&config)?;
    protect_configured_fds(&config).map_err(|_| "LIVE_STATUS_FD")?;
    let secrets = SecretReader::new(&config)
        .await
        .map_err(|_| "LIVE_STATUS_AUTH")?
        .fetch()
        .await
        .map_err(|_| "LIVE_STATUS_AUTH")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "LIVE_STATUS_HTTP")?;
    // Nur die bereits vorhandenen, autorisierten GETs dieses festen Kontos.
    let status = fetch(&client, &config, secrets.api.expose(), "status").await?;
    let destinations = fetch(&client, &config, secrets.api.expose(), "destinations").await?;
    let safe = sanitize(&status, &destinations)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &safe).map_err(|_| "LIVE_STATUS_OUTPUT")?;
    writeln!(stdout).map_err(|_| "LIVE_STATUS_OUTPUT")?;
    Ok(())
}

fn validate_config(config: &Config) -> Result<()> {
    if config.infisical.credential_fd != 5
        || !config.api_bind.ip().is_loopback()
        || config.api_bind.port() == 0
        || config.test_ingest.is_some()
        || !matches!(config.tls, TlsConfig::Infisical { .. })
    {
        return Err("LIVE_STATUS_SCOPE");
    }
    Ok(())
}

async fn fetch(
    client: &reqwest::Client,
    config: &Config,
    auth: &[u8],
    route: &str,
) -> Result<Value> {
    let path = match route {
        "status" => "status",
        "destinations" => "destinations",
        _ => return Err("LIVE_STATUS_SCOPE"),
    };
    let mut response = client
        .get(format!("http://{}/v1/me/{path}", config.api_bind))
        .query(&[("streamer_id", STREAMER_ID)])
        .header("X-Relay-Auth", auth)
        .send()
        .await
        .map_err(|_| "LIVE_STATUS_HTTP")?;
    if !response.status().is_success() {
        return Err("LIVE_STATUS_HTTP_STATUS");
    }
    let mut body = zeroize::Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "LIVE_STATUS_HTTP_BODY")?
    {
        if body.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err("LIVE_STATUS_HTTP_LIMIT");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "LIVE_STATUS_JSON")
}

fn uint(value: &Value) -> Value {
    value.as_u64().map_or(Value::Null, |number| json!(number))
}
fn number(value: &Value) -> Value {
    value
        .as_f64()
        .filter(|number| number.is_finite() && *number >= 0.0)
        .map_or(Value::Null, |number| json!(number))
}
fn boolean(value: &Value) -> Value {
    value
        .as_bool()
        .map_or(Value::Null, |boolean| json!(boolean))
}
fn codec(value: &Value) -> Value {
    match value.as_str() {
        Some("av1") => json!("av1"),
        Some("hevc") => json!("hevc"),
        Some("h264") => json!("h264"),
        _ => Value::Null,
    }
}
fn output_state(value: &Value) -> Value {
    match value.as_str() {
        Some("sending") => json!("sending"),
        Some("starting") => json!("starting"),
        Some("publishing") => json!("publishing"),
        Some("finished") => json!("finished"),
        Some("ended") => json!("ended"),
        Some("local_end_unconfirmed") => json!("local_end_unconfirmed"),
        Some("failed") => json!("failed"),
        Some("unknown") => json!("unknown"),
        _ if value.is_object() && value.get("failed").is_some() => json!("failed"),
        _ => Value::Null,
    }
}
fn profile(value: &Value) -> Value {
    if !value.is_object() {
        return Value::Null;
    }
    json!({"codec":codec(&value["codec"]),"width":uint(&value["width"]),
        "height":uint(&value["height"]),"fps":number(&value["fps"]),
        "bitrate_kbps":uint(&value["bitrate_kbps"])})
}

fn sanitize(status: &Value, destinations: &Value) -> Result<Value> {
    let sessions = status["sessions"].as_array().ok_or("LIVE_STATUS_SCHEMA")?;
    let targets = destinations["destinations"]
        .as_array()
        .ok_or("LIVE_STATUS_SCHEMA")?;
    let destination = targets.iter().find(|target| target["platform"] == "twitch");
    let session = sessions.first();
    let safe_session = session.map_or(Value::Null, |session| {
        let source = &session["source_observation"];
        let output = session["outputs"]["outputs"].as_array()
            .and_then(|outputs| outputs.iter().find(|output| output["id"] == "twitch"));
        json!({"id":uint(&session["id"]),"active":boolean(&session["active"]),
            "input_codec":codec(&session["input_codec"]),
            "input_bitrate_kbps":number(&session["input_bitrate_kbps"]),
            "received_bytes":uint(&session["received_bytes"]),
            "input_backpressure":boolean(&session["input_backpressure"]),
            "source":{"codec":codec(&source["codec"]),"width":uint(&source["width"]),
                "height":uint(&source["height"]),"fps_numerator":uint(&source["fps_numerator"]),
                "fps_denominator":uint(&source["fps_denominator"])},
            "ingest_end_reason":if session["ingest_end_reason"]=="ExplicitStop" {json!("ExplicitStop")} else {Value::Null},
            "output":output.map_or(Value::Null, |output|json!({"state":output_state(&output["state"]),
                "received_bytes":uint(&output["received_bytes"]),"received_events":uint(&output["received_events"])}))})
    });
    Ok(json!({"streamer_id":STREAMER_ID,"session":safe_session,
        "twitch":destination.map_or(Value::Null,|destination|json!({
            "enabled":boolean(&destination["enabled"]),"output_state":output_state(&destination["output_state"]),
            "input_codec":codec(&destination["input_codec"]),"input_bitrate_kbps":number(&destination["input_bitrate_kbps"]),
            "active_profile":profile(&destination["active_profile"])}))}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whitelist_preserves_evidence_but_never_arbitrary_strings_or_objects() {
        let status = json!({"sessions":[{"id":7,"active":true,"input_codec":"av1","input_bitrate_kbps":4999.5,
            "received_bytes":12345,"input_backpressure":false,"generation":"synthetic-secret",
            "error":"synthetic-secret","ingest_end_reason":"ExplicitStop",
            "source_observation":{"codec":"av1","width":2560,"height":1440,"fps_numerator":60,"fps_denominator":1,"extra":"synthetic-secret"},
            "outputs":{"outputs":[{"id":"twitch","state":"publishing","received_bytes":23456,"received_events":360,"url":"synthetic-secret"}]}}]});
        let targets = json!({"destinations":[{"platform":"twitch","enabled":true,"output_state":"sending",
            "rtmp_url":"synthetic-secret","reason":"synthetic-secret","input_codec":"av1","input_bitrate_kbps":4999.5,
            "active_profile":{"codec":"h264","width":1920,"height":1080,"fps":60,"bitrate_kbps":6000,"extra":"synthetic-secret"}}]});
        let safe = sanitize(&status, &targets).unwrap();
        assert!(!safe.to_string().contains("synthetic-secret"));
        assert_eq!(safe["streamer_id"], STREAMER_ID);
        assert_eq!(safe["session"]["id"], 7);
        assert_eq!(safe["session"]["source"]["codec"], "av1");
        assert_eq!(safe["session"]["output"]["received_bytes"], 23456);
        assert_eq!(safe["session"]["ingest_end_reason"], "ExplicitStop");
        assert_eq!(safe["twitch"]["active_profile"]["bitrate_kbps"], 6000);
    }
    #[test]
    fn wrong_types_unknown_enums_and_negative_numbers_are_never_forwarded() {
        for bad in [
            json!("synthetic-secret"),
            json!(-1),
            json!({"secret":"synthetic-secret"}),
            json!(["synthetic-secret"]),
            json!(true),
        ] {
            assert!(uint(&bad).is_null());
            assert!(number(&bad).is_null());
            assert!(codec(&bad).is_null());
            assert!(output_state(&bad).is_null());
        }
        assert_eq!(
            output_state(&json!({"failed":"synthetic-secret"})),
            "failed"
        );
        assert!(boolean(&json!("true")).is_null());
        let safe = sanitize(&json!({"sessions":[]}), &json!({"destinations":[]})).unwrap();
        assert!(safe["session"].is_null());
        assert!(safe["twitch"].is_null());
        assert!(sanitize(&json!({}), &json!({"destinations":[]})).is_err());
    }
    #[test]
    fn only_existing_fd5_and_loopback_production_configuration_are_admitted() {
        let mut config =
            Config::parse(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
        assert!(validate_config(&config).is_ok());
        config.api_bind = "0.0.0.0:8892".parse().unwrap();
        assert!(validate_config(&config).is_err());
        config.api_bind = "127.0.0.1:0".parse().unwrap();
        assert!(validate_config(&config).is_err());
        config.api_bind = "127.0.0.1:8892".parse().unwrap();
        config.infisical.credential_fd = 6;
        assert!(validate_config(&config).is_err());
    }
}
