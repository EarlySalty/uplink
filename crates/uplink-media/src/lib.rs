//! Sessiongebundene Medienverarbeitung. Plattformrechte werden dadurch nicht vergeben.

mod engine;
pub mod flv;
mod graph;
mod prepare;
pub mod pusher;
pub mod queue;
mod sockets;

use serde::Serialize;
use std::{fmt, path::PathBuf, sync::Arc, time::Duration};
use uplink_core::{Codec, FrameRate, LayoutRevision, PlanInput};
use uplink_ingest::{MediaEvent, TrackIdentity, WireTrack};
use zeroize::Zeroizing;

pub use engine::{MediaEngine, MediaObserver, RunningMedia};

pub type Result<T> = std::result::Result<T, MediaError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaError {
    InvalidConfiguration,
    InvalidPlan,
    MissingTrack,
    WrongSession,
    UnsupportedProfile,
    InvalidMedia,
    ResourceLimit,
    Backpressure,
    StartTimeout,
    ProcessFailed,
    ProcessCleanupFailed,
    Io,
    Cancelled,
    EndpointRejected,
    TlsRejected,
    PublishRejected,
    ProtocolRejected,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "Ungültige Medienkonfiguration",
            Self::InvalidPlan => "Ausgabeplan ist nicht freigegeben",
            Self::MissingTrack => "Eine benötigte Medienspur fehlt",
            Self::WrongSession => "Medien gehören nicht zur laufenden Session",
            Self::UnsupportedProfile => "Dieses Medienprofil ist hier nicht freigegeben",
            Self::InvalidMedia => "Ungültige Mediendaten empfangen",
            Self::ResourceLimit => "Mediengrenze erreicht",
            Self::Backpressure => "Ausgang kann die Medien nicht rechtzeitig übernehmen",
            Self::StartTimeout => "Medienstart hat die zulässige Frist überschritten",
            Self::ProcessFailed => "Medienverarbeitung wurde mit einem Fehler beendet",
            Self::ProcessCleanupFailed => "Medienprozess konnte nicht sicher beendet werden",
            Self::Io => "Medienverbindung wurde unterbrochen",
            Self::Cancelled => "Medienverarbeitung wurde beendet",
            Self::EndpointRejected => "Plattformadresse ist nicht freigegeben",
            Self::TlsRejected => "Zertifikat der Plattform wurde nicht bestätigt",
            Self::PublishRejected => "Plattform hat den Streamstart abgelehnt",
            Self::ProtocolRejected => "Plattformantwort entspricht nicht dem erwarteten Protokoll",
        })
    }
}
impl std::error::Error for MediaError {}

/// Zugänge nur aus dem autoritativen Broker. Kein Serialize/Clone, kein Klartext-Debug.
pub struct PublishSecret(Zeroizing<Vec<u8>>);
impl PublishSecret {
    pub fn new(bytes: Vec<u8>) -> Result<Self> {
        let bytes = Zeroizing::new(bytes);
        if bytes.is_empty()
            || bytes.len() > 4096
            || bytes.iter().any(|b| *b == 0 || *b == b'\n' || *b == b'\r')
        {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(Self(bytes))
    }
    pub(crate) fn expose_for_pipe(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for PublishSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublishSecret([geschützt])")
    }
}

/// Provider können Zugangsteile bereits im Endpoint-Pfad tragen. Deshalb wird
/// auch der Endpoint vollständig redigiert. Hosts stammen aus der Betreiberkonfiguration.
pub struct PublishTarget {
    pub id: String,
    pub endpoint: String,
    pub playpath: PublishSecret,
    /// Geprüfter Rootstore aus Dienstkonfiguration, keine eigene Secretablage.
    pub tls: Option<Arc<uplink_ingest::rustls::ClientConfig>>,
    pub allowed_hosts: Vec<String>,
    pub allow_loopback: bool,
    pub allow_unencrypted: bool,
}
impl fmt::Debug for PublishTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublishTarget")
            .field("id", &self.id)
            .field("endpoint", &"[geschützt]")
            .field("playpath", &self.playpath)
            .field("tls_configured", &self.tls.is_some())
            .field("allow_loopback", &self.allow_loopback)
            .field("allow_unencrypted", &self.allow_unencrypted)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct MediaLimits {
    pub max_tag_bytes: usize,
    pub queue_bytes: usize,
    pub queue_events: usize,
    pub startup_timeout: Duration,
    pub write_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub max_outputs: usize,
    pub max_encode_groups: usize,
    pub worker_threads: usize,
}
impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            max_tag_bytes: 2 * 1024 * 1024,
            queue_bytes: 8 * 1024 * 1024,
            queue_events: 512,
            startup_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(5),
            max_outputs: 12,
            max_encode_groups: 8,
            worker_threads: 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Eigenständig installierter, geprüfter FFmpeg-8-Build; kein Default zum alten Repository.
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Privater Laufzeitordner, Modus 0700, vom Dienst vorbereitet.
    pub work_directory: PathBuf,
    pub limits: MediaLimits,
}

#[derive(Debug, Clone)]
pub struct TrackBinding {
    pub logical_id: u32,
    pub wire: WireTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Composition {
    Crop(Crop),
    Stacked {
        gameplay: Crop,
        camera: Crop,
        camera_height: u32,
    },
    PictureInPicture {
        gameplay: Crop,
        camera: Crop,
        camera_box: Crop,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutSpec {
    pub revision: LayoutRevision,
    pub composition: Composition,
}

pub struct TargetRoute {
    pub output_id: String,
    pub target: PublishTarget,
}

pub struct SessionSpec {
    /// Vom autorisierten Eingang geliefert; Medien anderer Generation werden abgewiesen.
    pub identity: TrackIdentity,
    /// Freigegebene Quelldeklaration und Wunschprofile. Keine erfundene Messbestätigung.
    pub input: PlanInput,
    pub tracks: Vec<TrackBinding>,
    pub routes: Vec<TargetRoute>,
    pub layouts: Vec<LayoutSpec>,
}

/// Ein Wunsch ist kein Beleg für die Eingangsquelle oder ein Plattformrecht.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DesiredVideo {
    pub width: u32,
    pub height: u32,
    pub fps: FrameRate,
    pub bitrate_kbps: u32,
    pub codec: Codec,
}

pub struct DesiredOutput {
    pub target: PublishTarget,
    pub video: DesiredVideo,
    /// Autorisierte explizite Eingangsrollen, keine Zuordnung anhand der Reihenfolge.
    pub live_audio_track: u8,
    pub vod_audio_track: Option<u8>,
    pub layout: Option<LayoutSpec>,
}

pub struct DesiredSessionSpec {
    /// Darf eine Audiospur sein. Video wird aus dem wirklichen Vorlauf erkannt.
    pub first: MediaEvent,
    pub outputs: Vec<DesiredOutput>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioObservation {
    pub wire_track: u8,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u8,
}

/// Gemessener begrenzter Vorlauf. None bedeutet unbekannt, niemals freigegebenes CBR.
#[derive(Debug, Clone, Serialize)]
pub struct SourceObservation {
    pub video_wire_track: u8,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub pixel_format: String,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_matrix: Option<String>,
    pub color_range: Option<String>,
    pub rate_control: Option<String>,
    pub gop_frames: Option<u32>,
    pub audio: Vec<AudioObservation>,
    pub sampled_events: usize,
    pub sampled_bytes: usize,
    pub sampled_duration_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputStatus {
    pub id: String,
    pub state: OutputState,
    pub received_bytes: u64,
    pub received_events: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Starting,
    Publishing,
    Ended,
    Failed(MediaError),
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MediaStatus {
    pub encode_groups: usize,
    pub video_decoders: usize,
    pub outputs: Vec<OutputStatus>,
}

#[derive(Debug, Serialize)]
pub struct MediaReport {
    pub status: MediaStatus,
    pub error: Option<MediaError>,
}

#[cfg(test)]
mod secret_tests {
    use super::*;
    fn target() -> PublishTarget {
        PublishTarget {
            id: "synthetic".into(),
            endpoint: "rtmps://localhost/live/private-path-token?reject=1".into(),
            playpath: PublishSecret::new(b"private-publish-token".to_vec()).unwrap(),
            tls: None,
            allowed_hosts: vec!["localhost".into()],
            allow_loopback: false,
            allow_unencrypted: false,
        }
    }
    #[test]
    fn endpoint_credentials_are_hidden_in_direct_and_nested_debug() {
        let target = target();
        for debug in [
            format!("{target:?}"),
            format!("{:?}", Some(&target)),
            format!("{:?}", Ok::<_, MediaError>(&target)),
        ] {
            assert!(!debug.contains("private-path-token"));
            assert!(!debug.contains("private-publish-token"));
            assert!(!debug.contains("rtmps://"));
        }
    }
    #[tokio::test]
    async fn rejected_endpoint_error_never_repeats_path_credentials() {
        let result = crate::pusher::RunningPusher::start(target(), MediaLimits::default()).await;
        let error = match result {
            Ok(_) => panic!("query endpoint must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error, MediaError::EndpointRejected);
        for text in [format!("{error:?}"), format!("{error}")] {
            assert!(!text.contains("private-path-token"));
            assert!(!text.contains("private-publish-token"));
        }
    }
}
