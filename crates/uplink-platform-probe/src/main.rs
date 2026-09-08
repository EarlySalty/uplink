//! Ausschließlich lokale Hardwaremessung oder eine autorisierte GoLive-Konfigurationsabfrage.
//! Kein Publisher, kein Listener, keine schreibende Datenbankanfrage.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, process::ExitCode, time::Duration};
use tokio::io::AsyncReadExt;
use uplink_media::{
    EngineConfig, MediaLimits, PublishSecret,
    platform::{
        hardware,
        twitch::{Canvas, GoLiveClient, GoLiveProbe, Preferences, Rational},
    },
};
use uplink_service::{
    config::{Config, InfisicalConfig, MediaConfig, TlsConfig},
    crypto::Secret,
    secrets,
    store::Store,
};
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, &'static str>;

struct Arguments {
    hardware_only: bool,
    credential_fd: Option<u32>,
    metadata: Option<PathBuf>,
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

fn arguments(values: impl IntoIterator<Item = OsString>) -> Result<Arguments> {
    const ERROR: &str = "Argumente der Plattformprobe sind ungültig.";
    let mut values = values.into_iter();
    let mut hardware_only = false;
    let mut fields = BTreeMap::new();
    while let Some(flag) = values.next() {
        let flag = flag.to_str().ok_or(ERROR)?;
        if flag == "--hardware-only" {
            if hardware_only {
                return Err(ERROR);
            }
            hardware_only = true;
            continue;
        }
        if !matches!(
            flag,
            "--credential-fd" | "--infisical-metadata" | "--ffmpeg" | "--ffprobe"
        ) {
            return Err(ERROR);
        }
        if fields
            .insert(flag.to_owned(), values.next().ok_or(ERROR)?)
            .is_some()
        {
            return Err(ERROR);
        }
    }
    let ffmpeg = PathBuf::from(fields.remove("--ffmpeg").ok_or(ERROR)?);
    let ffprobe = PathBuf::from(fields.remove("--ffprobe").ok_or(ERROR)?);
    if !ffmpeg.is_absolute() || !ffprobe.is_absolute() {
        return Err(ERROR);
    }
    let credential_fd = fields
        .remove("--credential-fd")
        .map(|value| {
            let value = value.to_str().ok_or(ERROR)?;
            if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(ERROR);
            }
            let fd = value.parse::<u32>().map_err(|_| ERROR)?;
            if !(3..=1024).contains(&fd) {
                return Err(ERROR);
            }
            Ok(fd)
        })
        .transpose()?;
    let metadata = fields.remove("--infisical-metadata").map(PathBuf::from);
    if hardware_only {
        if credential_fd.is_some() || metadata.is_some() {
            return Err(ERROR);
        }
    } else if credential_fd.is_none() || metadata.as_ref().is_none_or(|path| !path.is_absolute()) {
        return Err(ERROR);
    }
    Ok(Arguments {
        hardware_only,
        credential_fd,
        metadata,
        ffmpeg,
        ffprobe,
    })
}

struct Metadata {
    base_url: String,
    project_id: String,
    environment: String,
    secret_path: String,
}

fn metadata(input: &str) -> Result<Metadata> {
    const ERROR: &str = "Infisical-Metadaten sind ungültig.";
    if input.len() > 65_536 {
        return Err(ERROR);
    }
    let mut fields = BTreeMap::new();
    for line in input.lines() {
        let Some((name, value)) = line.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !matches!(
            name,
            "INFISICAL_PROJECT_ID"
                | "INFISICAL_ENV"
                | "INFISICAL_SECRET_PATH"
                | "INFISICAL_API_URL"
        ) {
            continue;
        }
        let value = value.trim();
        if fields.insert(name, value).is_some() {
            return Err(ERROR);
        }
    }
    let mut take = |name| fields.remove(name).map(str::to_owned).ok_or(ERROR);
    let base_url = take("INFISICAL_API_URL")?;
    let project_id = take("INFISICAL_PROJECT_ID")?;
    let environment = take("INFISICAL_ENV")?;
    let secret_path = take("INFISICAL_SECRET_PATH")?;
    // Die bisherige normale Metadatendatei bezeichnet weiterhin diese Instanz.
    // Der Transport verwendet ausschließlich deren geschützten Unixsocket.
    if base_url != "http://127.0.0.1:8080"
        || project_id.len() != 36
        || project_id.bytes().enumerate().any(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte != b'-'
            } else {
                !byte.is_ascii_hexdigit()
            }
        })
        || environment.is_empty()
        || environment.len() > 32
        || !environment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || !secret_path.starts_with('/')
        || secret_path.len() > 256
        || !secret_path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-'))
    {
        return Err(ERROR);
    }
    Ok(Metadata {
        base_url: "http://infisical.local".into(),
        project_id,
        environment,
        secret_path,
    })
}

fn broker_identity(body: &[u8], expected: i64) -> Result<bool> {
    const ERROR: &str = "Brokerantwort ist ungültig.";
    #[derive(Deserialize)]
    struct Identity<'a> {
        #[serde(borrow)]
        platform_user_id: &'a str,
        #[serde(borrow)]
        access_token: &'a str,
    }
    if body.len() > 65_536 || expected <= 0 {
        return Err(ERROR);
    }
    // Borrow both fields from the zeroizing response; no owned token copy.
    let identity: Identity<'_> = serde_json::from_slice(body).map_err(|_| ERROR)?;
    if identity.access_token.is_empty()
        || identity.access_token.len() > 8192
        || identity
            .access_token
            .bytes()
            .any(|byte| byte.is_ascii_control())
        || identity.platform_user_id.is_empty()
        || !identity
            .platform_user_id
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(ERROR);
    }
    let actual = identity
        .platform_user_id
        .parse::<i64>()
        .map_err(|_| ERROR)?;
    if actual <= 0 {
        return Err(ERROR);
    }
    Ok(actual == expected)
}

fn broker_http_status(status: u16) -> Result<bool> {
    match status {
        200 => Ok(true),
        // Der Bestandsbroker verlangt user:read:chat. Sein 404 ist daher kein
        // Nachweis über die separat gespeicherte und autorisierte Publishauth.
        404 => Ok(false),
        401 | 403 => Err("Plattformbroker hat den internen Zugang abgewiesen."),
        409 => Err("Die Twitch-Verbindung benötigt eine erneute Anmeldung."),
        _ => Err("Plattformbroker kann die Identität derzeit nicht bestätigen."),
    }
}

async fn check_broker(
    client: &reqwest::Client,
    token: &Secret,
    streamer: i64,
) -> Result<Option<bool>> {
    let mut header = reqwest::header::HeaderValue::from_bytes(token.expose())
        .map_err(|_| "Interner Brokerzugang ist ungültig.")?;
    header.set_sensitive(true);
    let mut response = client
        .get("http://127.0.0.1:8769/twitch/api/v2/internal/platform-token")
        .query(&[
            ("streamer", streamer.to_string()),
            ("platform", "twitch".into()),
        ])
        .header("X-Internal-Token", header)
        .send()
        .await
        .map_err(|_| "Plattformbroker ist nicht erreichbar.")?;
    if !broker_http_status(response.status().as_u16())? {
        return Ok(None);
    }
    if response.content_length().is_some_and(|size| size > 65_536) {
        return Err("Brokerantwort überschreitet die zulässige Größe.");
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Brokerantwort wurde unterbrochen.")?
    {
        if body.len().saturating_add(chunk.len()) > 65_536 {
            return Err("Brokerantwort überschreitet die zulässige Größe.");
        }
        body.extend_from_slice(&chunk);
    }
    broker_identity(&body, streamer).map(Some)
}

fn fetch_config(args: &Arguments, meta: Metadata) -> Result<Config> {
    let credential_fd = args.credential_fd.ok_or("Credential-FD fehlt.")?;
    // Adapt the existing fetch contract. No server is started, and its TLS,
    // ingest and dashboard fields are never consumed by this executable.
    Ok(Config {
        chat: None,
        test_ingest: None,
        tls_reload_seconds: 60,
        loopback_test_ca: None,
        api_bind: ([127, 0, 0, 1], 0).into(),
        ingest_bind: ([127, 0, 0, 1], 0).into(),
        public_ingest_url: "rtmps://localhost/live".into(),
        dock_base_url: "https://localhost".into(),
        max_sessions: 1,
        max_pending_connections: 1,
        max_sessions_per_tenant: 1,
        database_max_queries: 1,
        request_timeout_seconds: 5,
        infisical: InfisicalConfig {
            base_url: meta.base_url,
            socket_path: "/run/uplink-infisical/api.sock".into(),
            socket_owner_uid: 0,
            project_id: meta.project_id,
            environment: meta.environment,
            secret_path: meta.secret_path,
            credential_fd,
        },
        tls: TlsConfig::Fds {
            certificate_fd: credential_fd + 1,
            private_key_fd: credential_fd + 2,
        },
        media: MediaConfig {
            ffmpeg: args.ffmpeg.clone(),
            ffprobe: args.ffprobe.clone(),
            work_directory: "/tmp".into(),
            max_event_bytes: 1024 * 1024,
            max_queued_bytes: 2 * 1024 * 1024,
            max_queued_events: 64,
            max_tracks: 16,
            live_audio_track: 0,
            vod_audio_track: Some(1),
        },
        platforms: Vec::new(),
    })
}

fn preferences() -> Preferences {
    Preferences {
        // OBSBasicSettings.ui at OBS 6b3e5507: Kbps; passed unchanged by
        // BasicOutputHandler.cpp and GoLiveAPI_PostData.cpp. This is a request
        // ceiling, never an assertion that these profiles fit this server.
        maximum_aggregate_bitrate: 20_000,
        maximum_video_tracks: 8,
        vod_track_audio: true,
        audio_samples_per_sec: 48_000,
        audio_channels: 2,
        audio_max_buffering_ms: 960,
        audio_fixed_buffering: false,
        canvases: [(2560, 1440), (1080, 1920)]
            .into_iter()
            .map(|(width, height)| Canvas {
                width,
                height,
                canvas_width: width,
                canvas_height: height,
                framerate: Rational {
                    numerator: 60,
                    denominator: 1,
                },
            })
            .collect(),
    }
}

#[derive(Serialize)]
struct Summary {
    broker_identity_matches: Option<bool>,
    chat_identity_broker_status: &'static str,
    hardware: hardware::HardwareReport,
    requested: Preferences,
    configuration: GoLiveProbe,
}

fn emit(value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)
        .map_err(|_| "Prüfergebnis konnte nicht ausgegeben werden.")?;
    stdout
        .write_all(b"\n")
        .map_err(|_| "Prüfergebnis konnte nicht ausgegeben werden.")
}

async fn run(args: Arguments) -> Result<()> {
    let engine = EngineConfig {
        ffmpeg: args.ffmpeg.clone(),
        ffprobe: args.ffprobe.clone(),
        work_directory: "/tmp".into(),
        limits: MediaLimits {
            worker_threads: 2,
            ..MediaLimits::default()
        },
    };
    if args.hardware_only {
        let report = hardware::measure(&engine)
            .await
            .map_err(|_| "Lokale Hardware- oder Encodermessung ist fehlgeschlagen.")?;
        return emit(&report);
    }
    let file = tokio::fs::File::open(
        args.metadata
            .as_ref()
            .ok_or("Infisical-Metadaten fehlen.")?,
    )
    .await
    .map_err(|_| "Infisical-Metadaten sind nicht verfügbar.")?;
    let mut bytes = Zeroizing::new(Vec::new());
    tokio::time::timeout(
        Duration::from_secs(2),
        file.take(65_537).read_to_end(&mut bytes),
    )
    .await
    .map_err(|_| "Infisical-Metadaten wurden nicht rechtzeitig gelesen.")?
    .map_err(|_| "Infisical-Metadaten konnten nicht gelesen werden.")?;
    let meta =
        metadata(std::str::from_utf8(&bytes).map_err(|_| "Infisical-Metadaten sind ungültig.")?)?;
    let config = fetch_config(&args, meta)?;
    // fetch calls the existing read_fd/protect_fd before any FFmpeg process is
    // created. The inherited original credential must carry CLOEXEC, too.
    let secrets::ServiceSecrets {
        database,
        encryption,
        bot_internal,
        ..
    } = secrets::fetch(&config).await?;
    let store = Store::connect(&database, 1).await?;
    // No endpoint or account name is loaded. LIMIT 2 proves uniqueness, and
    // CASE bounds even a malformed stored encrypted blob before allocation.
    let rows = store.query(
        "SELECT d.streamer_id, CASE WHEN octet_length(d.stream_key_enc) BETWEEN 29 AND 4125 THEN d.stream_key_enc ELSE NULL END FROM relay.destinations d JOIN relay.users u ON u.streamer_id=d.streamer_id WHERE d.platform='twitch' AND d.enabled=true AND u.enabled=true ORDER BY d.streamer_id LIMIT 2",
        &[],
    ).await?;
    if rows.len() != 1 {
        return Err(
            "Die Probe benötigt genau ein aktives Twitch-Ziel mit freigeschaltetem Besitzer.",
        );
    }
    let streamer: i64 = rows[0]
        .try_get(0)
        .map_err(|_| "Gespeicherte Twitch-Identität ist ungültig.")?;
    if streamer <= 0 {
        return Err("Gespeicherte Twitch-Identität ist ungültig.");
    }
    let ciphertext: Vec<u8> = rows[0]
        .try_get::<_, Option<Vec<u8>>>(1)
        .map_err(|_| "Verschlüsselter Twitch-Zugang ist ungültig.")?
        .ok_or("Verschlüsselter Twitch-Zugang fehlt oder überschreitet die Grenze.")?;
    let key = encryption.open(&ciphertext, &format!("destination:{streamer}:twitch"))?;
    let authentication = PublishSecret::new(key.expose().to_vec())
        .map_err(|_| "Twitch-Publishzugang ist ungültig.")?;
    drop(key);
    drop(encryption);
    drop(store);
    drop(database);
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "Plattformbroker-Client konnte nicht vorbereitet werden.")?;
    let identity_matches = check_broker(&client, &bot_internal, streamer).await?;
    drop(bot_internal);
    if identity_matches == Some(false) {
        return Err("Twitch-Ziel und autorisierte Brokeridentität stimmen nicht überein.");
    }
    let hardware = hardware::measure(&engine)
        .await
        .map_err(|_| "Lokale Hardware- oder Encodermessung ist fehlgeschlagen.")?;
    let requested = preferences();
    let configuration = GoLiveClient::new()
        .map_err(|_| "Twitch-Konfigurationsclient ist nicht verfügbar.")?
        .probe(&authentication, &hardware, &requested)
        .await
        .map_err(|_| "Twitch-Konfiguration konnte nicht sicher geprüft werden.")?;
    emit(&Summary {
        broker_identity_matches: identity_matches,
        chat_identity_broker_status: if identity_matches.is_some() {
            "confirmed"
        } else {
            "not_confirmed_http_404"
        },
        hardware,
        requested,
        configuration,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let outcome = match arguments(std::env::args_os().skip(1)) {
        Ok(args) => run(args).await,
        Err(error) => Err(error),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn local_hardware_mode_never_accepts_a_credential_or_metadata_source() {
        let parsed = arguments(args(&[
            "--hardware-only",
            "--ffmpeg",
            "/test/ffmpeg",
            "--ffprobe",
            "/test/ffprobe",
        ]))
        .unwrap();
        assert!(parsed.hardware_only);
        assert!(parsed.credential_fd.is_none());
        assert!(parsed.metadata.is_none());
        assert_eq!(parsed.ffmpeg, PathBuf::from("/test/ffmpeg"));
        assert_eq!(parsed.ffprobe, PathBuf::from("/test/ffprobe"));
        assert!(
            arguments(args(&[
                "--hardware-only",
                "--credential-fd",
                "3",
                "--ffmpeg",
                "/test/ffmpeg",
                "--ffprobe",
                "/test/ffprobe"
            ]))
            .is_err()
        );
    }

    #[test]
    fn live_probe_requires_an_explicit_safe_fd_and_normal_metadata_file() {
        let parsed = arguments(args(&[
            "--credential-fd",
            "3",
            "--infisical-metadata",
            "/normal/infisical.conf",
            "--ffmpeg",
            "/test/ffmpeg",
            "--ffprobe",
            "/test/ffprobe",
        ]))
        .unwrap();
        assert!(!parsed.hardware_only);
        assert_eq!(parsed.credential_fd, Some(3));
        for values in [
            vec!["--credential-fd", "0"],
            vec!["--credential-fd", "2"],
            vec!["--credential-fd", "3", "--credential-fd", "3"],
            vec!["--token", "sensitive-sentinel"],
        ] {
            let error = arguments(args(&values)).err().unwrap();
            assert!(!error.contains("sensitive-sentinel"));
        }
    }

    const META: &str = "IGNORED_NORMAL_SETTING=unused\nINFISICAL_PROJECT_ID=00000000-0000-0000-0000-000000000001\nINFISICAL_ENV=prod\nINFISICAL_SECRET_PATH=/\nINFISICAL_API_URL=http://127.0.0.1:8080\n";

    #[test]
    fn metadata_reads_only_known_literal_fields_without_shell_evaluation() {
        let value = metadata(META).unwrap();
        assert_eq!(value.base_url, "http://infisical.local");
        assert_eq!(value.environment, "prod");
        assert_eq!(value.secret_path, "/");
        assert_eq!(value.project_id, "00000000-0000-0000-0000-000000000001");
        assert!(metadata(&META.replace("prod", "$(sensitive-sentinel)")).is_err());
        assert!(
            metadata(&META.replace(
                "http://127.0.0.1:8080",
                "https://sensitive-sentinel.invalid"
            ))
            .is_err()
        );
        assert!(metadata(&format!("{META}INFISICAL_ENV=prod\n")).is_err());
        assert!(metadata(&META.replace("INFISICAL_ENV=prod\n", "")).is_err());
    }

    #[test]
    fn broker_identity_comes_from_platform_id_without_exposing_tokens() {
        let body = br#"{"platform_user_id":"42","platform_login":"irrelevant","access_token":"sensitive-sentinel","scopes":[]}"#;
        assert_eq!(broker_identity(body, 42), Ok(true));
        assert_eq!(broker_identity(body, 43), Ok(false));
        for body in [
            &br#"{"platform_user_id":"42","access_token":""}"#[..],
            &br#"{"platform_login":"42","access_token":"sensitive-sentinel"}"#[..],
            &br#"{"platform_user_id":"sensitive-sentinel","access_token":"secret"}"#[..],
            &b"malformed sensitive-sentinel"[..],
        ] {
            let error = broker_identity(body, 42).unwrap_err();
            assert!(!error.contains("sensitive-sentinel"));
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn missing_chat_grant_never_becomes_a_confirmed_identity() {
        assert_eq!(broker_http_status(404), Ok(false));
        assert_eq!(broker_http_status(200), Ok(true));
        for status in [201, 204, 301, 401, 403, 409, 500, 503] {
            assert!(broker_http_status(status).is_err());
        }
        assert_eq!(
            broker_identity(
                br#"{"platform_user_id":"43","access_token":"synthetic"}"#,
                42
            ),
            Ok(false)
        );
        assert_eq!(
            serde_json::to_string(&Option::<bool>::None).unwrap(),
            "null"
        );
    }
}
