use serde::Deserialize;
use std::net::SocketAddr;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub chat: Option<ChatSettings>,
    #[serde(default)]
    pub test_ingest: Option<TestIngestConfig>,
    #[serde(default = "tls_reload_default")]
    pub tls_reload_seconds: u64,
    #[serde(default)]
    pub loopback_test_ca: Option<std::path::PathBuf>,
    pub api_bind: SocketAddr,
    pub ingest_bind: SocketAddr,
    pub public_ingest_url: String,
    pub dock_base_url: String,
    pub max_sessions: usize,
    #[serde(default = "pending_connections_default")]
    pub max_pending_connections: usize,
    pub max_sessions_per_tenant: usize,
    pub database_max_queries: u32,
    pub request_timeout_seconds: u64,
    pub infisical: InfisicalConfig,
    pub tls: TlsConfig,
    pub media: MediaConfig,
    pub platforms: Vec<PlatformConfig>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatSettings {
    pub bot_base_url: String,
    pub allowed_origins: Vec<String>,
    pub max_users: usize,
    pub max_sockets_per_user: usize,
    pub idle_timeout_seconds: u64,
}
fn tls_reload_default() -> u64 {
    60
}
fn pending_connections_default() -> usize {
    8
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestIngestConfig {
    pub allowed_streamer_ids: Vec<u64>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    #[serde(default = "media_worker_threads_default")]
    pub worker_threads: usize,
    #[serde(default)]
    pub enhanced: EnhancedConfig,
    #[serde(default)]
    pub probe_dump_streamer_id: Option<u64>,
    pub ffmpeg: std::path::PathBuf,
    pub ffprobe: std::path::PathBuf,
    pub work_directory: std::path::PathBuf,
    pub max_event_bytes: usize,
    pub max_queued_bytes: usize,
    pub max_queued_events: usize,
    pub max_tracks: usize,
    pub live_audio_track: u8,
    pub vod_audio_track: Option<u8>,
}
fn media_worker_threads_default() -> usize {
    2
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    pub name: String,
    pub allowed_hosts: Vec<String>,
    pub allow_unencrypted: bool,
    pub video_codec: uplink_core::Codec,
    pub use_vod_audio: bool,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfisicalConfig {
    pub base_url: String,
    #[serde(default = "infisical_socket_default")]
    pub socket_path: std::path::PathBuf,
    #[serde(default)]
    pub socket_owner_uid: u32,
    pub project_id: String,
    pub environment: String,
    pub secret_path: String,
    pub credential_fd: u32,
}
fn infisical_socket_default() -> std::path::PathBuf {
    uplink_infisical_transport::DEFAULT_SOCKET.into()
}
#[derive(Clone, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum TlsConfig {
    Fds {
        certificate_fd: u32,
        private_key_fd: u32,
    },
    Infisical {
        certificate_secret: String,
        private_key_secret: String,
    },
}
impl Config {
    pub fn permits_tenant(&self, tenant: u64) -> bool {
        tenant > 0
            && self
                .test_ingest
                .as_ref()
                .is_none_or(|scope| scope.allowed_streamer_ids.contains(&tenant))
    }
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        if input.len() > 64 * 1024 {
            return Err("Konfigurationsdatei ist zu groß.");
        }
        let config: Self = toml::from_str(input).map_err(|_| "Konfiguration ist ungültig.")?;
        if let Some(chat) = &config.chat {
            let base =
                reqwest::Url::parse(&chat.bot_base_url).map_err(|_| "Botadresse ist ungültig.")?;
            if !matches!(base.scheme(), "http" | "https")
                || !matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                || base.path() != "/"
                || base.query().is_some()
                || base.fragment().is_some()
                || !base.username().is_empty()
                || base.password().is_some()
                || !(1..=1024).contains(&chat.max_users)
                || !(1..=32).contains(&chat.max_sockets_per_user)
                || !(30..=3600).contains(&chat.idle_timeout_seconds)
                || chat.allowed_origins.is_empty()
                || chat.allowed_origins.len() > 8
                || chat.allowed_origins.iter().any(|origin| {
                    reqwest::Url::parse(origin).map_or(true, |url| {
                        url.scheme() != "https" || url.origin().ascii_serialization() != *origin
                    })
                })
            {
                return Err("Chatkonfiguration verletzt Zugriffs- oder Ressourcengrenzen.");
            }
        }
        let public_ingest = reqwest::Url::parse(&config.public_ingest_url)
            .map_err(|_| "Öffentliche Eingangsadresse ist ungültig.")?;
        if public_ingest.scheme() != "rtmps"
            || public_ingest.host_str().is_none()
            || !public_ingest.username().is_empty()
            || public_ingest.password().is_some()
            || public_ingest.query().is_some()
            || public_ingest.fragment().is_some()
            || !matches!(public_ingest.path(), "/live" | "/live/")
        {
            return Err("Öffentliche Eingangsadresse darf keinen Zugang enthalten.");
        }
        let dock = reqwest::Url::parse(&config.dock_base_url)
            .map_err(|_| "Öffentliche Dockadresse ist ungültig.")?;
        if dock.scheme() != "https"
            || dock.host_str().is_none()
            || !dock.username().is_empty()
            || dock.password().is_some()
            || dock.query().is_some()
            || dock.fragment().is_some()
        {
            return Err(
                "Öffentliche Dockadresse muss eine gültige HTTPS-Adresse ohne Zugang sein.",
            );
        }
        if config
            .loopback_test_ca
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || !config.ingest_bind.ip().is_loopback())
        {
            return Err("Eigene Test-CA ist nur am lokalen Testeingang erlaubt.");
        }
        if config.test_ingest.as_ref().is_some_and(|scope| {
            scope.allowed_streamer_ids.len() != 1
                || scope
                    .allowed_streamer_ids
                    .iter()
                    .any(|id| *id == 0 || *id > i64::MAX as u64)
        }) {
            return Err("Testeingang benötigt genau eine ausdrückliche Nutzerfreigabe.");
        }
        if !config.api_bind.ip().is_loopback()
            || !(30..=86400).contains(&config.tls_reload_seconds)
            || config.max_sessions == 0
            || config.max_sessions > 1024
            || !(1..=128).contains(&config.max_pending_connections)
            || config.max_sessions_per_tenant == 0
            || config.max_sessions_per_tenant > config.max_sessions
            || config.database_max_queries == 0
            || config.database_max_queries > 64
            || config.request_timeout_seconds == 0
            || config.request_timeout_seconds > 60
            || config.infisical.base_url != uplink_infisical_transport::BASE_URL
            || !config.infisical.socket_path.is_absolute()
            || config.infisical.credential_fd < 3
        {
            return Err("Konfiguration verletzt Zugriffs- oder Ressourcengrenzen.");
        }
        match &config.tls {
            TlsConfig::Fds {
                certificate_fd,
                private_key_fd,
            } if *certificate_fd < 3
                || *private_key_fd < 3
                || certificate_fd == private_key_fd
                || *certificate_fd == config.infisical.credential_fd
                || *private_key_fd == config.infisical.credential_fd =>
            {
                return Err("TLS-FDs sind ungültig.");
            }
            TlsConfig::Infisical {
                certificate_secret,
                private_key_secret,
            } if certificate_secret == private_key_secret
                || [certificate_secret, private_key_secret].iter().any(|name| {
                    name.is_empty()
                        || name.len() > 128
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                }) =>
            {
                return Err("TLS-Secretnamen sind ungültig.");
            }
            _ => {}
        }
        if !config.media.ffmpeg.is_absolute()
            || !config.media.ffprobe.is_absolute()
            || !config.media.work_directory.is_absolute()
            || config.media.max_queued_events == 0
            || config.media.max_queued_events > 4096
            || config.media.max_tracks == 0
            || config.media.max_tracks > 32
            || config
                .media
                .probe_dump_streamer_id
                .is_some_and(|id| id == 0 || id > i64::MAX as u64)
            || config.media.vod_audio_track == Some(config.media.live_audio_track)
        {
            return Err("Mediengrenzen oder Audiozuordnung sind ungültig.");
        }
        let enhanced = &config.media.enhanced;
        let mut keys = std::collections::HashSet::new();
        if !(1..=16).contains(&enhanced.maximum_video_tracks)
            || !(1..=100_000).contains(&enhanced.maximum_aggregate_bitrate)
            || enhanced.capacity_units > 100_000
            || enhanced.legacy_session_units == 0
            || enhanced.profiles.len() > 128
            || (enhanced.capacity_units > 0
                && enhanced.legacy_session_units > enhanced.capacity_units)
            || enhanced.profiles.iter().any(|profile| {
                profile.key.is_empty()
                    || profile.key.len() > 4096
                    || profile.key.chars().any(char::is_control)
                    || !keys.insert(&profile.key)
                    || profile.units == 0
                    || profile.units > enhanced.capacity_units
            })
        {
            return Err("Enhanced-Broadcasting-Grenzen sind ungültig.");
        }
        config.ingest_limits()?;
        let mut platforms = std::collections::HashSet::new();
        for platform in &config.platforms {
            if !matches!(
                platform.name.as_str(),
                "twitch" | "kick" | "youtube" | "tiktok"
            ) || !platforms.insert(&platform.name)
                || platform.allowed_hosts.is_empty()
                || platform.allowed_hosts.len() > 16
                || platform.allowed_hosts.iter().any(|host| {
                    host.is_empty()
                        || host.len() > 253
                        || !host
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                })
                || (platform.use_vod_audio && config.media.vod_audio_track.is_none())
            {
                return Err("Plattformkonfiguration ist ungültig.");
            }
        }
        Ok(config)
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnhancedConfig {
    #[serde(default = "enhanced_tracks")]
    pub maximum_video_tracks: u32,
    #[serde(default = "enhanced_bitrate")]
    pub maximum_aggregate_bitrate: u64,
    #[serde(default)]
    pub capacity_units: u32,
    #[serde(default = "legacy_units")]
    pub legacy_session_units: u32,
    #[serde(default)]
    pub profiles: Vec<CapacityProfile>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityProfile {
    pub key: String,
    pub units: u32,
}
fn enhanced_tracks() -> u32 {
    8
}
fn enhanced_bitrate() -> u64 {
    20000
}
fn legacy_units() -> u32 {
    1
}
impl Default for EnhancedConfig {
    fn default() -> Self {
        Self {
            maximum_video_tracks: enhanced_tracks(),
            maximum_aggregate_bitrate: enhanced_bitrate(),
            capacity_units: 0,
            legacy_session_units: 1,
            profiles: Vec::new(),
        }
    }
}
