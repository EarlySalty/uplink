//! Enger GoLive-Vertrag nach OBS 6b3e550729f125b6c5b3767df88c08f5aef9d264.
//! Kein OBS-Identitätsersatz, keine Tokenspeicherung und keine öffentliche Veröffentlichung.
use crate::{PublishSecret, PublishTarget};
use serde::{Deserialize, Serialize};
use std::{fmt, time::Duration};
use uplink_core::Codec;
use zeroize::Zeroizing;

const ENDPOINT: &str = "https://ingest.twitch.tv/api/v3/GetClientConfiguration";
const SCHEMA: &str = "2025-01-25";
const MAX_RESPONSE: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoLiveError {
    InvalidRequest,
    Transport,
    HttpRejected,
    ResponseTooLarge,
    InvalidResponse,
    AccountRejected,
    WarningNeedsDecision,
    EndpointRejected,
    UnsupportedEncoder,
    UnsupportedAudioMapping,
}
impl GoLiveError {
    /// Feste Diagnose ohne Anbietertext, Zugang oder Antwortkörper.
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "Twitch-Anfrage passt nicht zu den gemessenen Fähigkeiten",
            Self::Transport => "Twitch-Konfiguration ist vorübergehend nicht erreichbar",
            Self::HttpRejected => "Twitch hat die Konfigurationsanfrage abgelehnt",
            Self::ResponseTooLarge => "Twitch-Konfiguration überschreitet die zulässige Größe",
            Self::InvalidResponse => "Twitch-Konfiguration ist unvollständig oder widersprüchlich",
            Self::AccountRejected => "Twitch hat diese Ausgabekonfiguration nicht freigegeben",
            Self::WarningNeedsDecision => {
                "Twitch meldet eine Einschränkung; die Ausgabe bleibt angehalten"
            }
            Self::EndpointRejected => "Die von Twitch gelieferte Adresse ist nicht freigegeben",
            Self::UnsupportedEncoder => {
                "Der von Twitch geforderte Encoder ist hier nicht nachgewiesen"
            }
            Self::UnsupportedAudioMapping => {
                "Die von Twitch geforderte Audiozuordnung ist nicht nachgewiesen"
            }
        }
    }
}
impl fmt::Display for GoLiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for GoLiveError {}
type Result<T> = std::result::Result<T, GoLiveError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoLiveEncoder {
    ObsX264,
    JimNvenc,
    ObsNvencH264Tex,
    JimHevcNvenc,
    ObsNvencHevcTex,
    JimAv1Nvenc,
    ObsNvencAv1Tex,
    H264TextureAmf,
    H265TextureAmf,
    Av1TextureAmf,
    ObsQsv11V2,
    ObsQsv11Hevc,
    ObsQsv11Av1,
    FfmpegVaapi,
    FfmpegHevcVaapi,
    FfmpegAv1Vaapi,
}
impl GoLiveEncoder {
    pub fn audited(name: &str) -> Option<Self> {
        Some(match name {
            "obs_x264" => Self::ObsX264,
            "jim_nvenc" => Self::JimNvenc,
            "obs_nvenc_h264_tex" => Self::ObsNvencH264Tex,
            "jim_hevc_nvenc" => Self::JimHevcNvenc,
            "obs_nvenc_hevc_tex" => Self::ObsNvencHevcTex,
            "jim_av1_nvenc" => Self::JimAv1Nvenc,
            "obs_nvenc_av1_tex" => Self::ObsNvencAv1Tex,
            "h264_texture_amf" => Self::H264TextureAmf,
            "h265_texture_amf" => Self::H265TextureAmf,
            "av1_texture_amf" => Self::Av1TextureAmf,
            "obs_qsv11_v2" => Self::ObsQsv11V2,
            "obs_qsv11_hevc" => Self::ObsQsv11Hevc,
            "obs_qsv11_av1" => Self::ObsQsv11Av1,
            "ffmpeg_vaapi" => Self::FfmpegVaapi,
            "ffmpeg_hevc_vaapi" => Self::FfmpegHevcVaapi,
            "ffmpeg_av1_vaapi" => Self::FfmpegAv1Vaapi,
            _ => return None,
        })
    }
    pub fn audited_name(self) -> &'static str {
        match self {
            Self::ObsX264 => "obs_x264",
            Self::JimNvenc => "jim_nvenc",
            Self::ObsNvencH264Tex => "obs_nvenc_h264_tex",
            Self::JimHevcNvenc => "jim_hevc_nvenc",
            Self::ObsNvencHevcTex => "obs_nvenc_hevc_tex",
            Self::JimAv1Nvenc => "jim_av1_nvenc",
            Self::ObsNvencAv1Tex => "obs_nvenc_av1_tex",
            Self::H264TextureAmf => "h264_texture_amf",
            Self::H265TextureAmf => "h265_texture_amf",
            Self::Av1TextureAmf => "av1_texture_amf",
            Self::ObsQsv11V2 => "obs_qsv11_v2",
            Self::ObsQsv11Hevc => "obs_qsv11_hevc",
            Self::ObsQsv11Av1 => "obs_qsv11_av1",
            Self::FfmpegVaapi => "ffmpeg_vaapi",
            Self::FfmpegHevcVaapi => "ffmpeg_hevc_vaapi",
            Self::FfmpegAv1Vaapi => "ffmpeg_av1_vaapi",
        }
    }
    pub fn codec(self) -> Codec {
        match self {
            Self::ObsX264
            | Self::JimNvenc
            | Self::ObsNvencH264Tex
            | Self::H264TextureAmf
            | Self::ObsQsv11V2
            | Self::FfmpegVaapi => Codec::H264,
            Self::JimHevcNvenc
            | Self::ObsNvencHevcTex
            | Self::H265TextureAmf
            | Self::ObsQsv11Hevc
            | Self::FfmpegHevcVaapi => Codec::Hevc,
            Self::JimAv1Nvenc
            | Self::ObsNvencAv1Tex
            | Self::Av1TextureAmf
            | Self::ObsQsv11Av1
            | Self::FfmpegAv1Vaapi => Codec::Av1,
        }
    }
    pub fn executable_encoder(self) -> Option<&'static str> {
        match self {
            Self::ObsX264 => Some("libx264"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rational {
    pub numerator: u32,
    pub denominator: u32,
}
#[derive(Debug, Clone, Serialize)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub canvas_width: u32,
    pub canvas_height: u32,
    pub framerate: Rational,
}
#[derive(Debug, Clone, Serialize)]
pub struct Preferences {
    /// kbit/s, wie OBS-SpinBox „Kbps“ und BasicOutputHandler im gepinnten Quellstand.
    pub maximum_aggregate_bitrate: u64,
    pub maximum_video_tracks: u32,
    pub vod_track_audio: bool,
    pub audio_samples_per_sec: u32,
    pub audio_channels: u32,
    pub audio_max_buffering_ms: u32,
    pub audio_fixed_buffering: bool,
    pub canvases: Vec<Canvas>,
}

/// Nur aus tatsächlichen Messungen erzeugte Fähigkeiten; der Dienst setzt keine GPU ein.
#[derive(Serialize)]
pub struct RequestCapabilities {
    cpu: Cpu,
    memory: Memory,
    system: System,
    /// Der OBS-Vertrag serialisiert unbekannte optionale Fähigkeiten als null.
    gpu: Option<()>,
    gaming_features: Option<()>,
}
impl RequestCapabilities {
    pub fn from_measurement(report: &super::hardware::HardwareReport) -> Result<Self> {
        if report.cpu.logical_cores == 0
            || report.cpu.physical_cores == 0
            || report.cpu.physical_cores > report.cpu.logical_cores
            || report.memory.total_bytes == 0
            || report.memory.free_bytes > report.memory.total_bytes
            || report.gpu_verified
        {
            return Err(GoLiveError::InvalidRequest);
        }
        Ok(Self {
            cpu: Cpu {
                physical_cores: report.cpu.physical_cores,
                logical_cores: report.cpu.logical_cores,
                name: report.cpu.name.clone(),
                speed: report.cpu.speed_mhz,
            },
            memory: Memory {
                total: report.memory.total_bytes,
                free: report.memory.free_bytes,
            },
            system: System {
                name: report.system.name.clone(),
                version: report.system.version.clone(),
                release: report.system.release.clone(),
                revision: report.system.revision.clone(),
                bits: report.system.bits,
                arm: report.system.arm,
            },
            gpu: None,
            gaming_features: None,
        })
    }
}
#[derive(Serialize)]
struct Cpu {
    physical_cores: u32,
    logical_cores: u32,
    name: Option<String>,
    speed: Option<u32>,
}
#[derive(Serialize)]
struct Memory {
    total: u64,
    free: u64,
}
#[derive(Serialize)]
struct System {
    name: String,
    version: String,
    release: String,
    revision: String,
    bits: u32,
    arm: bool,
}

/// Response enthält Zugänge. Debug zeigt bewusst nur geprüfte Medienzahlen.
pub struct TwitchConfiguration {
    pub target: PublishTarget,
    pub video: Vec<VideoConfiguration>,
    pub audio: Vec<AudioConfiguration>,
    pub encoders: Vec<GoLiveEncoder>,
}
impl fmt::Debug for TwitchConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TwitchConfiguration")
            .field("target", &self.target)
            .field("video_tracks", &self.video.len())
            .field("audio_tracks", &self.audio.len())
            .field("encoders", &self.encoders)
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfiguration {
    pub wire_track: u8,
    pub canvas_index: usize,
    pub width: u32,
    pub height: u32,
    pub framerate: Rational,
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub keyframe_seconds: u32,
    pub profile: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRole {
    Live,
    Vod,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfiguration {
    pub wire_track: u8,
    pub role: AudioRole,
    pub channels: u8,
    pub bitrate_kbps: u32,
}

pub struct GoLiveClient {
    http: reqwest::Client,
}

#[derive(Debug, Serialize)]
pub struct GoLiveProbe {
    pub http_status: u16,
    pub configuration_status: &'static str,
    pub video: Vec<VideoProbe>,
    pub live_audio_tracks: usize,
    pub vod_audio_tracks: usize,
    pub rtmps_endpoint_available: bool,
    pub arbitrary_audio_ids: bool,
    /// Feste Suchbegriffe aus dem Statushinweis, keine ungeprüfte Ursachenbehauptung.
    pub status_terms: Vec<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct VideoProbe {
    /// Only audited literal identifiers are emitted; arbitrary response text is never logged.
    pub encoder: &'static str,
    pub width: u32,
    pub height: u32,
    pub canvas_index: usize,
}

impl GoLiveClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .https_only(true)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|_| GoLiveError::Transport)?,
        })
    }
    /// Der Aufruf prüft nur die Konfiguration des autorisierten Kontos. Er startet keinen Stream.
    pub async fn configure(
        &self,
        authentication: &PublishSecret,
        hardware: &super::hardware::HardwareReport,
        preferences: &Preferences,
        allowed_ingest_hosts: &[String],
    ) -> Result<TwitchConfiguration> {
        let capabilities = RequestCapabilities::from_measurement(hardware)?;
        let codecs = configurable_codecs(&hardware.encoders);
        let body = request_body(authentication, &capabilities, preferences, &codecs)?;
        let response = self
            .http
            .post(ENDPOINT)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from_owner(body))
            .send()
            .await
            .map_err(|_| GoLiveError::Transport)?;
        let bytes = read_response(response).await?;
        parse_configuration(
            &bytes,
            preferences,
            authentication,
            allowed_ingest_hosts,
            &codecs,
        )
    }

    /// Reine autorisierte Fähigkeitsabfrage. Auch nicht ausführbare Konfigurationen
    /// werden sicher zusammengefasst; daraus wird kein PublishTarget erzeugt.
    pub async fn probe(
        &self,
        authentication: &PublishSecret,
        hardware: &super::hardware::HardwareReport,
        preferences: &Preferences,
    ) -> Result<GoLiveProbe> {
        let capabilities = RequestCapabilities::from_measurement(hardware)?;
        let codecs: Vec<_> = hardware
            .encoders
            .iter()
            .filter(|p| p.initialized)
            .map(|p| p.codec)
            .collect();
        let body = request_body(authentication, &capabilities, preferences, &codecs)?;
        let response = self
            .http
            .post(ENDPOINT)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from_owner(body))
            .send()
            .await
            .map_err(|_| GoLiveError::Transport)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Ok(GoLiveProbe {
                http_status: status,
                configuration_status: "http_rejected",
                video: Vec::new(),
                live_audio_tracks: 0,
                vod_audio_tracks: 0,
                rtmps_endpoint_available: false,
                arbitrary_audio_ids: false,
                status_terms: Vec::new(),
            });
        }
        let bytes = read_response(response).await?;
        inspect_configuration(&bytes, status)
    }
}

#[derive(Deserialize)]
struct RawInspection<'a> {
    #[serde(borrow)]
    meta: RawMeta<'a>,
    #[serde(borrow)]
    status: RawStatus<'a>,
    #[serde(borrow)]
    encoder_configurations: Vec<RawVideoInspection<'a>>,
    #[serde(borrow)]
    ingest_endpoints: Vec<RawEndpoint<'a>>,
    #[serde(borrow)]
    audio_configurations: RawAudios<'a>,
}
#[derive(Deserialize)]
struct RawVideoInspection<'a> {
    #[serde(rename = "type")]
    encoder: &'a str,
    width: u32,
    height: u32,
    canvas_index: usize,
}
fn inspect_configuration(bytes: &[u8], http_status: u16) -> Result<GoLiveProbe> {
    if bytes.len() > MAX_RESPONSE {
        return Err(GoLiveError::ResponseTooLarge);
    }
    let raw: RawInspection<'_> =
        serde_json::from_slice(bytes).map_err(|_| GoLiveError::InvalidResponse)?;
    if raw.meta.service != "IVS"
        || raw.meta.schema_version != SCHEMA
        || raw.encoder_configurations.len() > 16
        || raw.ingest_endpoints.len() > 16
        || raw.audio_configurations.live.len() > 16
        || raw
            .audio_configurations
            .vod
            .as_ref()
            .is_some_and(|tracks| tracks.len() > 16)
    {
        return Err(GoLiveError::InvalidResponse);
    }
    let status = match raw.status.result {
        "success" => "success",
        "warning" => "warning",
        "error" => "error",
        _ => "unknown",
    };
    let video = raw
        .encoder_configurations
        .iter()
        .map(|v| VideoProbe {
            encoder: GoLiveEncoder::audited(v.encoder)
                .map_or("unknown", GoLiveEncoder::audited_name),
            width: v.width,
            height: v.height,
            canvas_index: v.canvas_index,
        })
        .collect();
    let vod = raw.audio_configurations.vod.as_deref().unwrap_or_default();
    let arbitrary_audio_ids = raw
        .audio_configurations
        .live
        .iter()
        .chain(vod)
        .enumerate()
        .any(|(index, a)| a.track_id != index as u32);
    Ok(GoLiveProbe {
        http_status,
        configuration_status: status,
        video,
        live_audio_tracks: raw.audio_configurations.live.len(),
        vod_audio_tracks: vod.len(),
        rtmps_endpoint_available: raw.ingest_endpoints.iter().any(|e| e.protocol == "RTMPS"),
        arbitrary_audio_ids,
        status_terms: status_terms(raw.status.html_en_us),
    })
}

fn status_terms(message: Option<&str>) -> Vec<&'static str> {
    let message = Zeroizing::new(message.unwrap_or_default().to_ascii_lowercase());
    [
        "client",
        "version",
        "application",
        "gpu",
        "graphics",
        "hardware",
        "unsupported",
        "not supported",
        "encoder",
        "authentication",
        "stream key",
        "account",
        "memory",
        "1440",
        "canvas",
        "operating system",
        "invalid",
        "missing",
        "required",
        "resolution",
        "affiliate",
        "partner",
        "cpu",
        "bitrate",
        "audio",
        "minimum",
        "composition",
        "index",
        "capabilities",
        "information",
        "at least one",
        "client gpu",
    ]
    .into_iter()
    .filter(|term| message.contains(term))
    .collect()
}

#[derive(Serialize)]
struct PostData<'a> {
    service: &'static str,
    schema_version: &'static str,
    authentication: &'a str,
    client: ClientDescription,
    capabilities: &'a RequestCapabilities,
    preferences: &'a Preferences,
}
#[derive(Serialize)]
struct ClientDescription {
    name: &'static str,
    version: &'static str,
    supported_codecs: Vec<&'static str>,
}
fn configurable_codecs(probes: &[super::hardware::EncoderProbe]) -> Vec<Codec> {
    // configure must request only the concrete obs_x264 -> libx264 mapping.
    // The read-only probe deliberately keeps measuring all available codecs.
    if probes
        .iter()
        .any(|probe| probe.initialized && probe.codec == Codec::H264 && probe.encoder == "libx264")
    {
        vec![Codec::H264]
    } else {
        Vec::new()
    }
}

fn request_body(
    authentication: &PublishSecret,
    capabilities: &RequestCapabilities,
    preferences: &Preferences,
    codecs: &[Codec],
) -> Result<Zeroizing<Vec<u8>>> {
    validate_preferences(preferences)?;
    if codecs.is_empty() {
        return Err(GoLiveError::InvalidRequest);
    }
    let authentication = std::str::from_utf8(authentication.expose_for_pipe())
        .map_err(|_| GoLiveError::InvalidRequest)?;
    let client = ClientDescription {
        name: "uplink",
        version: env!("CARGO_PKG_VERSION"),
        supported_codecs: codecs
            .iter()
            .map(|codec| match codec {
                Codec::H264 => "h264",
                Codec::Hevc => "h265",
                Codec::Av1 => "av1",
            })
            .collect(),
    };
    let request = PostData {
        service: "IVS",
        schema_version: SCHEMA,
        authentication,
        client,
        capabilities,
        preferences,
    };
    // Serialize directly into a zeroizing owner; no temporary plaintext String.
    let mut bytes = Zeroizing::new(Vec::new());
    serde_json::to_writer(&mut *bytes, &request).map_err(|_| GoLiveError::InvalidRequest)?;
    if bytes.len() > MAX_RESPONSE {
        return Err(GoLiveError::InvalidRequest);
    }
    Ok(bytes)
}
fn validate_preferences(p: &Preferences) -> Result<()> {
    if p.maximum_video_tracks == 0
        || p.maximum_video_tracks > 16
        || p.maximum_aggregate_bitrate == 0
        || p.maximum_aggregate_bitrate > 100_000
        || p.audio_samples_per_sec != 48000
        || !(1..=2).contains(&p.audio_channels)
        || p.audio_max_buffering_ms > 2000
        || p.canvases.is_empty()
        || p.canvases.len() > 2
    {
        return Err(GoLiveError::InvalidRequest);
    }
    for c in &p.canvases {
        if c.width < 2
            || c.height < 2
            || c.width > 4096
            || c.height > 4096
            || c.canvas_width < c.width
            || c.canvas_height < c.height
            || c.canvas_width > 4096
            || c.canvas_height > 4096
            || c.framerate.denominator == 0
            || c.framerate.numerator == 0
            || u64::from(c.framerate.numerator) > 120 * u64::from(c.framerate.denominator)
        {
            return Err(GoLiveError::InvalidRequest);
        }
    }
    Ok(())
}
async fn read_response(mut response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>> {
    if !response.status().is_success() {
        return Err(GoLiveError::HttpRejected);
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE as u64)
    {
        return Err(GoLiveError::ResponseTooLarge);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|_| GoLiveError::Transport)? {
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|len| len > MAX_RESPONSE)
        {
            return Err(GoLiveError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[derive(Deserialize)]
struct RawConfiguration<'a> {
    #[serde(borrow)]
    meta: RawMeta<'a>,
    #[serde(borrow)]
    status: RawStatus<'a>,
    #[serde(borrow)]
    ingest_endpoints: Vec<RawEndpoint<'a>>,
    #[serde(borrow)]
    encoder_configurations: Vec<RawVideo<'a>>,
    #[serde(borrow)]
    audio_configurations: RawAudios<'a>,
}
#[derive(Deserialize)]
struct RawMeta<'a> {
    service: &'a str,
    schema_version: &'a str,
    config_id: &'a str,
}
#[derive(Deserialize)]
struct RawStatus<'a> {
    result: &'a str,
    #[serde(default, borrow)]
    html_en_us: Option<&'a str>,
}
#[derive(Deserialize)]
struct RawEndpoint<'a> {
    protocol: &'a str,
    url_template: &'a str,
    authentication: Option<&'a str>,
}
#[derive(Deserialize)]
struct RawVideo<'a> {
    #[serde(rename = "type")]
    encoder: &'a str,
    width: u32,
    height: u32,
    framerate: Option<Rational>,
    canvas_index: usize,
    colorspace: Option<&'a str>,
    range: Option<&'a str>,
    format: Option<&'a str>,
    #[serde(borrow)]
    settings: RawVideoSettings<'a>,
}
#[derive(Deserialize)]
struct RawVideoSettings<'a> {
    rate_control: &'a str,
    bitrate: u32,
    keyint_sec: u32,
    profile: &'a str,
    bf: Option<u32>,
    preset: Option<&'a str>,
    tune: Option<&'a str>,
    x264opts: Option<&'a str>,
}
#[derive(Deserialize)]
struct RawAudios<'a> {
    #[serde(borrow)]
    live: Vec<RawAudio<'a>>,
    #[serde(borrow)]
    vod: Option<Vec<RawAudio<'a>>>,
}
#[derive(Deserialize)]
struct RawAudio<'a> {
    codec: &'a str,
    track_id: u32,
    channels: u8,
    settings: RawAudioSettings,
}
#[derive(Deserialize)]
struct RawAudioSettings {
    bitrate: u32,
}

fn parse_configuration(
    bytes: &[u8],
    preferences: &Preferences,
    authentication: &PublishSecret,
    hosts: &[String],
    codecs: &[Codec],
) -> Result<TwitchConfiguration> {
    validate_preferences(preferences)?;
    if bytes.len() > MAX_RESPONSE {
        return Err(GoLiveError::ResponseTooLarge);
    }
    let raw: RawConfiguration<'_> =
        serde_json::from_slice(bytes).map_err(|_| GoLiveError::InvalidResponse)?;
    if raw.meta.service != "IVS"
        || raw.meta.schema_version != SCHEMA
        || raw.meta.config_id.is_empty()
        || raw.meta.config_id.len() > 1024
    {
        return Err(GoLiveError::InvalidResponse);
    }
    match raw.status.result {
        "success" => {}
        "warning" => return Err(GoLiveError::WarningNeedsDecision),
        "error" => return Err(GoLiveError::AccountRejected),
        _ => return Err(GoLiveError::InvalidResponse),
    }
    if raw.encoder_configurations.is_empty()
        || raw.encoder_configurations.len() > preferences.maximum_video_tracks as usize
        || raw.ingest_endpoints.is_empty()
        || raw.ingest_endpoints.len() > 16
    {
        return Err(GoLiveError::InvalidResponse);
    }
    let mut video = Vec::new();
    let mut encoders = Vec::new();
    let mut bitrate = 0u64;
    for (index, v) in raw.encoder_configurations.iter().enumerate() {
        let canvas = preferences
            .canvases
            .get(v.canvas_index)
            .ok_or(GoLiveError::InvalidResponse)?;
        let fps = v.framerate.unwrap_or(canvas.framerate);
        if v.width < 2
            || v.height < 2
            || !v.width.is_multiple_of(2)
            || !v.height.is_multiple_of(2)
            || v.width > canvas.canvas_width
            || v.height > canvas.canvas_height
            || fps.denominator == 0
            || fps.numerator == 0
            || u64::from(fps.numerator) * u64::from(canvas.framerate.denominator)
                > u64::from(canvas.framerate.numerator) * u64::from(fps.denominator)
        {
            return Err(GoLiveError::InvalidResponse);
        }
        let audited = GoLiveEncoder::audited(v.encoder).ok_or(GoLiveError::UnsupportedEncoder)?;
        if audited.executable_encoder().is_none()
            || !codecs.contains(&audited.codec())
            || v.settings.rate_control != "CBR"
            || v.settings.bitrate == 0
            || v.settings.bitrate > 20_000
            || !(1..=4).contains(&v.settings.keyint_sec)
            || !["baseline", "main", "high"].contains(&v.settings.profile)
            || v.settings.bf.is_some_and(|bf| bf != 0)
            || v.settings.preset.is_some_and(|s| s != "veryfast")
            || v.settings.tune.is_some_and(|s| s != "zerolatency")
            || v.settings.x264opts.is_some_and(|s| !s.is_empty())
            || v.colorspace.is_some_and(|s| s != "VIDEO_CS_709")
            || v.range.is_some_and(|s| s != "VIDEO_RANGE_PARTIAL")
            || v.format
                .is_some_and(|s| !matches!(s, "VIDEO_FORMAT_NV12" | "VIDEO_FORMAT_I420"))
        {
            return Err(GoLiveError::UnsupportedEncoder);
        }
        bitrate += u64::from(v.settings.bitrate);
        encoders.push(audited);
        video.push(VideoConfiguration {
            wire_track: index as u8,
            canvas_index: v.canvas_index,
            width: v.width,
            height: v.height,
            framerate: fps,
            codec: audited.codec(),
            bitrate_kbps: v.settings.bitrate,
            keyframe_seconds: v.settings.keyint_sec,
            profile: v.settings.profile.into(),
        });
    }
    let vod = raw.audio_configurations.vod.as_deref().unwrap_or_default();
    if raw.audio_configurations.live.is_empty()
        || raw.audio_configurations.live.len() + vod.len() > 16
        || (preferences.vod_track_audio && vod.is_empty())
        || (!preferences.vod_track_audio && !vod.is_empty())
    {
        return Err(GoLiveError::UnsupportedAudioMapping);
    }
    let mut audio = Vec::new();
    for (role, list) in [
        (AudioRole::Live, raw.audio_configurations.live.as_slice()),
        (AudioRole::Vod, vod),
    ] {
        for a in list {
            // OBS emits live then VOD at sequential indices. Until separately proven,
            // a contradictory server track_id is rejected instead of guessing its role.
            if a.track_id != audio.len() as u32
                || a.codec != "aac"
                || u32::from(a.channels) != preferences.audio_channels
                || a.settings.bitrate < 32
                || a.settings.bitrate > 320
            {
                return Err(GoLiveError::UnsupportedAudioMapping);
            }
            bitrate += u64::from(a.settings.bitrate);
            audio.push(AudioConfiguration {
                wire_track: audio.len() as u8,
                role,
                channels: a.channels,
                bitrate_kbps: a.settings.bitrate,
            });
        }
    }
    if bitrate > preferences.maximum_aggregate_bitrate {
        return Err(GoLiveError::InvalidResponse);
    }
    let endpoint = raw
        .ingest_endpoints
        .iter()
        .find(|e| e.protocol == "RTMPS")
        .ok_or(GoLiveError::EndpointRejected)?;
    let (template, query) = endpoint
        .url_template
        .split_once('?')
        .unwrap_or((endpoint.url_template, ""));
    let server = template
        .strip_suffix("/{stream_key}")
        .ok_or(GoLiveError::EndpointRejected)?;
    let url = url::Url::parse(server).map_err(|_| GoLiveError::EndpointRejected)?;
    if url.scheme() != "rtmps"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path().contains('{')
        || url.path().contains('}')
        || url
            .host_str()
            .is_none_or(|host| !hosts.iter().any(|allowed| allowed == host))
    {
        return Err(GoLiveError::EndpointRejected);
    }
    let original = std::str::from_utf8(authentication.expose_for_pipe())
        .map_err(|_| GoLiveError::InvalidRequest)?;
    let active = endpoint
        .authentication
        .filter(|s| !s.is_empty())
        .unwrap_or(original);
    let (key, active_query) = active.split_once('?').unwrap_or((active, ""));
    let mut query_builder = url::form_urlencoded::Serializer::new(String::new());
    let original_query = original.split_once('?').map_or("", |(_, q)| q);
    let mut seen = std::collections::BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes())
        .chain(url::form_urlencoded::parse(original_query.as_bytes()))
        .chain(url::form_urlencoded::parse(active_query.as_bytes()))
    {
        if key == "clientConfigId" {
            return Err(GoLiveError::InvalidResponse);
        }
        if let Some(previous) = seen.insert(key.clone(), value.clone()) {
            if previous != value {
                return Err(GoLiveError::InvalidResponse);
            }
            continue;
        }
        query_builder.append_pair(&key, &value);
    }
    query_builder.append_pair("clientConfigId", raw.meta.config_id);
    let query = Zeroizing::new(query_builder.finish());
    let mut playpath = Zeroizing::new(Vec::new());
    playpath.extend_from_slice(key.as_bytes());
    playpath.push(b'?');
    playpath.extend_from_slice(query.as_bytes());
    let secret = PublishSecret::new(std::mem::take(&mut *playpath))
        .map_err(|_| GoLiveError::InvalidResponse)?;
    Ok(TwitchConfiguration {
        target: PublishTarget {
            id: "twitch".into(),
            endpoint: server.into(),
            playpath: secret,
            tls: None,
            allowed_hosts: hosts.to_vec(),
            allow_loopback: false,
            allow_unencrypted: false,
        },
        video,
        audio,
        encoders,
    })
}

#[cfg(test)]
mod tests;
