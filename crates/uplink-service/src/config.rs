use serde::Deserialize;
use std::net::SocketAddr;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub api_bind: SocketAddr,
    pub ingest_bind: SocketAddr,
    pub public_ingest_url: String,
    pub dock_base_url: String,
    pub max_sessions: usize,
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
pub struct MediaConfig {
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
    pub project_id: String,
    pub environment: String,
    pub secret_path: String,
    pub credential_fd: u32,
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
    pub fn parse(input: &str) -> Result<Self, &'static str> {
        if input.len() > 64 * 1024 {
            return Err("Konfigurationsdatei ist zu groß.");
        }
        let config: Self = toml::from_str(input).map_err(|_| "Konfiguration ist ungültig.")?;
        let infisical = reqwest::Url::parse(&config.infisical.base_url)
            .map_err(|_| "Infisical-Adresse ist ungültig.")?;
        let allowed_infisical = infisical.scheme() == "https"
            || (infisical.scheme() == "http"
                && matches!(
                    infisical.host_str(),
                    Some("127.0.0.1" | "localhost" | "[::1]")
                ));
        if !config.api_bind.ip().is_loopback()
            || config.max_sessions == 0
            || config.max_sessions > 1024
            || config.max_sessions_per_tenant == 0
            || config.max_sessions_per_tenant > config.max_sessions
            || config.database_max_queries == 0
            || config.database_max_queries > 64
            || config.request_timeout_seconds == 0
            || config.request_timeout_seconds > 60
            || !allowed_infisical
            || !infisical.username().is_empty()
            || infisical.password().is_some()
            || config.infisical.credential_fd < 3
            || !config.public_ingest_url.starts_with("rtmps://")
            || !config.dock_base_url.starts_with("https://")
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
            || config.media.max_event_bytes == 0
            || config.media.max_event_bytes > 16 * 1024 * 1024
            || config.media.max_queued_bytes < config.media.max_event_bytes
            || config
                .media
                .max_queued_bytes
                .saturating_mul(config.max_sessions)
                > 1024 * 1024 * 1024
            || config.media.max_queued_events == 0
            || config.media.max_queued_events > 4096
            || config.media.max_tracks == 0
            || config.media.max_tracks > 32
            || config.media.vod_audio_track == Some(config.media.live_audio_track)
        {
            return Err("Mediengrenzen oder Audiozuordnung sind ungültig.");
        }
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
