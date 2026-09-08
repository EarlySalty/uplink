use serde::Deserialize;
use std::{cmp::Ordering, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    EncodeCapacity,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "Ungültige Konfiguration: {reason}"),
            Self::EncodeCapacity => {
                f.write_str("Der Plan benötigt mehr Video-Encodes als freigegeben.")
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "RawFrameRate")]
pub struct FrameRate {
    numerator: u32,
    denominator: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrameRate {
    numerator: u32,
    denominator: u32,
}

impl TryFrom<RawFrameRate> for FrameRate {
    type Error = Error;
    fn try_from(value: RawFrameRate) -> Result<Self, Error> {
        Self::new(value.numerator, value.denominator)
    }
}

impl FrameRate {
    pub fn new(numerator: u32, denominator: u32) -> Result<Self, Error> {
        if numerator == 0 || denominator == 0 {
            return Err(Error::Invalid("Bildrate muss positiv sein."));
        }
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        Ok(Self {
            numerator: numerator / a,
            denominator: denominator / a,
        })
    }
    pub fn numerator(self) -> u32 {
        self.numerator
    }
    pub fn denominator(self) -> u32 {
        self.denominator
    }
}

impl PartialOrd for FrameRate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FrameRate {
    fn cmp(&self, other: &Self) -> Ordering {
        (u64::from(self.numerator) * u64::from(other.denominator))
            .cmp(&(u64::from(other.numerator) * u64::from(self.denominator)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "RawScope")]
pub struct SessionScope {
    tenant_id: u64,
    session_id: u64,
    generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScope {
    tenant_id: u64,
    session_id: u64,
    generation: u64,
}

impl TryFrom<RawScope> for SessionScope {
    type Error = Error;
    fn try_from(v: RawScope) -> Result<Self, Error> {
        Self::new(v.tenant_id, v.session_id, v.generation)
    }
}
impl SessionScope {
    /// Identitäten kommen künftig aus Authentifizierung und Sessionverwaltung.
    /// Diese Typprüfung ersetzt keine Berechtigungsprüfung.
    pub fn new(tenant_id: u64, session_id: u64, generation: u64) -> Result<Self, Error> {
        if tenant_id == 0 || session_id == 0 || generation == 0 {
            return Err(Error::Invalid(
                "Nutzer, Session und Generation müssen gesetzt sein.",
            ));
        }
        Ok(Self {
            tenant_id,
            session_id,
            generation,
        })
    }
}

macro_rules! enum_model {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
enum_model!(Codec { Av1, H264, Hevc });
enum_model!(Chroma {
    Yuv420,
    Yuv422,
    Yuv444
});
enum_model!(ColorPrimaries { Bt709, Bt2020 });
enum_model!(Transfer {
    Bt709,
    Srgb,
    Pq,
    Hlg
});
enum_model!(Matrix { Bt709, Bt2020Ncl });
enum_model!(ColorRange { Limited, Full });
enum_model!(RateMode { Cbr, HqCbr });
enum_model!(AudioCodec { Aac, Opus });
enum_model!(AudioRole { Live, Vod });
enum_model!(Platform {
    Twitch,
    Kick,
    Youtube,
    Tiktok
});

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Color {
    pub primaries: ColorPrimaries,
    pub transfer: Transfer,
    pub matrix: Matrix,
    pub range: ColorRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateControl {
    pub mode: RateMode,
    pub target_kbps: u32,
    pub max_kbps: u32,
    pub buffer_kbits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gop {
    pub keyframe_interval_frames: u32,
    pub closed: bool,
}

/// Deklarierte Profileigenschaften. Nur `plan` prüft sie gegen Limits/Fähigkeiten.
/// Ein Wert dieses Typs ist kein Medien- oder Plattformnachweis.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    pub codec: Codec,
    pub codec_profile: String,
    pub level: String,
    pub bit_depth: u8,
    pub chroma: Chroma,
    pub color: Color,
    pub rate: RateControl,
    pub gop: Gop,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioProfile {
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u8,
    pub bitrate_kbps: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioTrack {
    pub track_id: u32,
    pub role: AudioRole,
    pub profile: AudioProfile,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub scope: SessionScope,
    pub video_track: u32,
    pub video: VideoProfile,
    pub audio: Vec<AudioTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutRevision {
    pub id: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioRequest {
    pub role: AudioRole,
    pub profile: AudioProfile,
}

/// Explizite Szenariodaten. Ein späterer Adapter muss die tatsächlichen
/// Kontofähigkeiten und die Gültigkeitsdauer seiner Nachweise liefern.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetCapabilities {
    pub video: Vec<VideoProfile>,
    pub audio: Vec<AudioProfile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCapabilities {
    pub decodable_video: Vec<VideoProfile>,
    pub encodable_video: Vec<VideoProfile>,
    pub decodable_audio: Vec<AudioProfile>,
    pub encodable_audio: Vec<AudioProfile>,
    pub compositable_layouts: Vec<LayoutRevision>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRequest {
    pub id: String,
    pub platform: Platform,
    pub video: VideoProfile,
    pub layout: Option<LayoutRevision>,
    pub encoder_preset_revision: String,
    pub live_audio: AudioRequest,
    pub vod_audio: Option<AudioRequest>,
    pub capabilities: TargetCapabilities,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_fps: FrameRate,
    pub max_video_kbps: u32,
    pub max_audio_kbps: u32,
    pub max_audio_tracks: usize,
    pub max_outputs: usize,
    pub max_unique_encodes: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    pub limits: Limits,
    pub source: Option<Source>,
    pub worker: WorkerCapabilities,
    pub outputs: Vec<OutputRequest>,
}

pub(crate) fn label_valid(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl VideoProfile {
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
    pub(crate) fn validate(&self, limits: &Limits) -> Result<(), Error> {
        if self.width == 0
            || self.height == 0
            || self.width > limits.max_width
            || self.height > limits.max_height
            || self.pixels() > limits.max_pixels
            || self.fps > limits.max_fps
        {
            return Err(Error::Invalid(
                "Auflösung oder Bildrate überschreitet die Grenzen.",
            ));
        }
        if !label_valid(&self.codec_profile)
            || !label_valid(&self.level)
            || !matches!(self.bit_depth, 8 | 10 | 12)
            || self.gop.keyframe_interval_frames == 0
        {
            return Err(Error::Invalid(
                "Video-Profil, Bit-Tiefe oder GOP ist ungültig.",
            ));
        }
        if self.rate.target_kbps == 0
            || self.rate.max_kbps != self.rate.target_kbps
            || self.rate.max_kbps > limits.max_video_kbps
            || self.rate.buffer_kbits == 0
        {
            return Err(Error::Invalid(
                "CBR braucht ein positives, begrenztes Bitratenbudget und einen Puffer.",
            ));
        }
        Ok(())
    }
}

impl AudioProfile {
    pub(crate) fn validate(&self, limits: &Limits) -> Result<(), Error> {
        if self.sample_rate == 0
            || self.sample_rate > 192000
            || self.channels == 0
            || self.channels > 8
            || self.bitrate_kbps == 0
            || self.bitrate_kbps > limits.max_audio_kbps
        {
            return Err(Error::Invalid("Audioformat überschreitet die Grenzen."));
        }
        Ok(())
    }
}
