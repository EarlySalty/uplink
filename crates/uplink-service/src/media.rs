use crate::{api::ServiceState, runtime::SessionProcessor};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use uplink_ingest::{
    EventKind, MediaEvent, MediaKind, TrackIdentity, WireCodec, WireTrack, rustls,
};
use uplink_media::{
    DesiredOutput, DesiredVideo, EngineConfig, MediaEngine, PublishSecret, PublishTarget,
};

#[derive(Clone)]
pub struct Coordinator {
    state: Arc<ServiceState>,
    engine: Arc<MediaEngine>,
    tls: Arc<rustls::ClientConfig>,
    engine_config: Arc<EngineConfig>,
    hardware: Arc<
        tokio::sync::OnceCell<
            Result<uplink_media::platform::hardware::HardwareReport, uplink_media::MediaError>,
        >,
    >,
    broker: Option<Arc<crate::chat::BotBroker>>,
    cast: CastMixer,
}

#[derive(Clone, Default)]
struct CastMixer(Arc<CastMixerState>);

#[derive(Default)]
struct CastMixerState {
    programs: Mutex<HashMap<u64, Arc<CastProgram>>>,
    next_id: AtomicU64,
}

struct CastProgram {
    id: u64,
    tenant: u64,
    owner_source: u64,
    identity: TrackIdentity,
    sender: tokio::sync::mpsc::Sender<CastMessage>,
    timeline: Mutex<CastTimeline>,
    codecs: Mutex<HashMap<WireTrack, WireCodec>>,
}

enum CastMessage {
    Event(MediaEvent),
    Stop,
}

#[derive(Default)]
struct CastTimeline {
    source_id: Option<u64>,
    source_base_dts: u32,
    canonical_base_dts: u32,
    last_dts: Option<u32>,
}

impl CastMixer {
    fn current(&self, tenant: u64) -> Option<Arc<CastProgram>> {
        self.0
            .programs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&tenant)
            .cloned()
    }

    fn install(
        &self,
        tenant: u64,
        owner_source: u64,
        identity: TrackIdentity,
        sender: tokio::sync::mpsc::Sender<CastMessage>,
    ) -> (Arc<CastProgram>, bool) {
        let mut programs = self
            .0
            .programs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = programs.get(&tenant) {
            return (existing.clone(), false);
        }
        let id = self
            .0
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let program = Arc::new(CastProgram {
            id,
            tenant,
            owner_source,
            identity,
            sender,
            timeline: Mutex::new(CastTimeline::default()),
            codecs: Mutex::new(HashMap::new()),
        });
        programs.insert(tenant, program.clone());
        (program, true)
    }

    fn remove_if_current(&self, program: &CastProgram) {
        let mut programs = self
            .0
            .programs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if programs
            .get(&program.tenant)
            .is_some_and(|current| current.id == program.id)
        {
            programs.remove(&program.tenant);
        }
    }

    fn stop_source(&self, program: &CastProgram, source_id: u64) {
        let is_current = program
            .timeline
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .source_id
            == Some(source_id);
        // Der Outputtask hält die Reservation der Quelle, die ihn gestartet
        // hat. Trennt sich diese Owner-Quelle oder die aktuell ausgespielte
        // Quelle, wird der Programmpfad kontrolliert beendet. Eine weiterhin
        // ausgewählte aktive Quelle kann ihn am nächsten Keyframe neu öffnen;
        // dadurch bleibt keine Zombie-Reservation zurück.
        if program.owner_source != source_id && !is_current {
            return;
        }
        self.remove_if_current(program);
        let _ = program.sender.try_send(CastMessage::Stop);
    }
}

impl CastProgram {
    fn source_is_active(&self, source_id: u64) -> bool {
        self.timeline
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .source_id
            == Some(source_id)
    }

    fn validate_codecs(
        &self,
        headers: &HashMap<WireTrack, MediaEvent>,
    ) -> Result<(), &'static str> {
        let offered: HashMap<_, _> = headers
            .iter()
            .map(|(track, event)| (*track, event.codec))
            .collect();
        if !offered.keys().any(|track| track.kind == MediaKind::Video)
            || !offered.keys().any(|track| track.kind == MediaKind::Audio)
        {
            return Err(
                "Casting-Quelle hat noch kein vollständiges Audio-/Video-Profil geliefert.",
            );
        }
        let mut expected = self
            .codecs
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if expected.is_empty() {
            *expected = offered;
            return Ok(());
        }
        if *expected != offered {
            return Err(
                "Casting-Quelle verwendet andere Codec- oder Track-Rollen als das laufende Program. Alle POVs müssen dasselbe Eingangsprofil senden.",
            );
        }
        Ok(())
    }

    fn begin_source(
        &self,
        source_id: u64,
        headers: &HashMap<WireTrack, MediaEvent>,
        keyframe: &MediaEvent,
    ) -> Result<(), &'static str> {
        if !is_video_keyframe(keyframe) {
            return Err("Casting-Schnitt wartet auf den nächsten Video-Keyframe.");
        }
        self.validate_codecs(headers)?;
        let mut timeline = self
            .timeline
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let canonical_base = timeline
            .last_dts
            .map(|last| last.saturating_add(1))
            .unwrap_or(0);
        timeline.source_id = Some(source_id);
        timeline.source_base_dts = keyframe.dts_ms;
        timeline.canonical_base_dts = canonical_base;
        let mut ordered: Vec<_> = headers.values().collect();
        ordered.sort_by_key(|event| {
            (
                u8::from(event.identity.track.kind == MediaKind::Audio),
                event.identity.track.wire_id,
            )
        });
        for header in ordered {
            let identity = TrackIdentity {
                session: self.identity.session,
                generation: self.identity.generation,
                track: header.identity.track,
            };
            let routed = header.routed_as(identity, canonical_base, i64::from(canonical_base));
            self.sender
                .try_send(CastMessage::Event(routed))
                .map_err(|_| "Casting-Programmpuffer ist ausgelastet.")?;
        }
        let identity = TrackIdentity {
            session: self.identity.session,
            generation: self.identity.generation,
            track: keyframe.identity.track,
        };
        let pts_delta = keyframe.pts_ms - i64::from(keyframe.dts_ms);
        let routed = keyframe.routed_as(
            identity,
            canonical_base,
            i64::from(canonical_base).saturating_add(pts_delta),
        );
        self.sender
            .try_send(CastMessage::Event(routed))
            .map_err(|_| "Casting-Programmpuffer ist ausgelastet.")?;
        timeline.last_dts = Some(canonical_base);
        Ok(())
    }

    fn route(&self, source_id: u64, event: &MediaEvent) -> Result<bool, &'static str> {
        let mut timeline = self
            .timeline
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if timeline.source_id != Some(source_id) {
            return Ok(false);
        }
        if event.configuration_revision > 1 {
            return Err(
                "Casting-Quelle hat ihr Codecprofil im laufenden Program geändert. Quelle mit stabilem Profil neu verbinden.",
            );
        }
        let relative = match event.dts_ms.checked_sub(timeline.source_base_dts) {
            Some(value) => value,
            None => return Ok(false),
        };
        let dts = timeline
            .canonical_base_dts
            .checked_add(relative)
            .ok_or("Casting-Zeitlinie ist übergelaufen.")?;
        if timeline.last_dts.is_some_and(|last| dts < last) {
            return Ok(false);
        }
        let identity = TrackIdentity {
            session: self.identity.session,
            generation: self.identity.generation,
            track: event.identity.track,
        };
        let pts_delta = event.pts_ms - i64::from(event.dts_ms);
        let routed = event.routed_as(identity, dts, i64::from(dts).saturating_add(pts_delta));
        self.sender
            .try_send(CastMessage::Event(routed))
            .map_err(|_| "Casting-Programmpuffer ist ausgelastet.")?;
        timeline.last_dts = Some(dts);
        Ok(true)
    }
}

fn is_video_keyframe(event: &MediaEvent) -> bool {
    event.identity.track.kind == MediaKind::Video
        && event.event_kind == EventKind::Frame
        && event
            .wire_body()
            .first()
            .is_some_and(|first| first & 0x70 == 0x10)
}

impl Coordinator {
    pub fn new(state: Arc<ServiceState>) -> Result<Self, &'static str> {
        let config = &state.config.media;
        std::fs::create_dir_all(&config.work_directory)
            .map_err(|_| "Medienverzeichnis ist nicht verfügbar.")?;
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::symlink_metadata(&config.work_directory)
            .map_err(|_| "Medienverzeichnis ist nicht verfügbar.")?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Medienverzeichnis ist nicht geschützt.");
        }
        std::fs::set_permissions(
            &config.work_directory,
            std::fs::Permissions::from_mode(0o700),
        )
        .map_err(|_| "Medienverzeichnis ist nicht geschützt.")?;
        let engine_config = EngineConfig {
            ffmpeg: config.ffmpeg.clone(),
            ffprobe: config.ffprobe.clone(),
            work_directory: config.work_directory.clone(),
            limits: state.config.media_limits(),
        };
        let engine = MediaEngine::new(engine_config.clone())
            .map_err(|_| "Medienkonfiguration ist ungültig.")?;
        let mut roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(path) = &state.config.loopback_test_ca {
            if !state.config.ingest_bind.ip().is_loopback() {
                return Err("Eigene Test-CA ist nur am lokalen Testeingang erlaubt.");
            }
            use rustls::pki_types::{CertificateDer, pem::PemObject};
            use std::io::Read;
            let mut pem = Vec::new();
            std::fs::File::open(path)
                .map_err(|_| "Öffentliche Test-CA fehlt.")?
                .take(65537)
                .read_to_end(&mut pem)
                .map_err(|_| "Öffentliche Test-CA ist nicht lesbar.")?;
            if pem.len() > 65536 {
                return Err("Öffentliche Test-CA ist zu groß.");
            }
            let certificate = CertificateDer::from_pem_slice(&pem)
                .map_err(|_| "Öffentliche Test-CA ist ungültig.")?;
            roots
                .add(certificate)
                .map_err(|_| "Öffentliche Test-CA ist ungültig.")?;
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| "TLS-Ausgang ist ungültig.")?
        .with_root_certificates(roots)
        .with_no_client_auth();
        state.registry.configure_capacity(
            config.enhanced.capacity_units,
            config.enhanced.legacy_session_units,
        )?;
        let broker = state
            .config
            .chat
            .as_ref()
            .map(|chat| {
                crate::chat::BotBroker::new(
                    &chat.bot_base_url,
                    crate::crypto::Secret::new(state.secrets.bot_internal.expose().to_vec()),
                )
            })
            .transpose()?;
        Ok(Self {
            engine_config: Arc::new(engine_config),
            hardware: Arc::new(tokio::sync::OnceCell::new()),
            broker: broker.map(Arc::new),
            state,
            engine: Arc::new(engine),
            tls: Arc::new(tls),
            cast: CastMixer::default(),
        })
    }
    async fn twitch_publish_erlaubt(
        &self,
        tenant: u64,
        generation: i64,
    ) -> Result<(), &'static str> {
        let granted = self
            .broker
            .as_ref()
            .ok_or("Die Twitch-Kontoprüfung ist noch nicht eingerichtet.")?
            .publish_grant(tenant)
            .await
            .map_err(|_| {
                "Der Twitch-Zugang oder sein Kontoinhaber konnte nicht bestätigt werden. Twitch erneut verbinden."
            })?;
        pruefe_publish_generation(granted, generation)
    }
    async fn twitch_output(
        &self,
        tenant: u64,
        wunsch: &Zielwunsch,
        source: &uplink_media::SourceObservation,
    ) -> Result<uplink_media::ProgramOutput, TwitchOutputError> {
        let output = &wunsch.output;
        let wahl = if wunsch.hochkant.is_some() {
            self.gespeicherte_hochkant_wahl(
                i64::try_from(tenant).map_err(|_| "Nutzeridentität ist ungültig.")?,
            )
            .await?
        } else {
            None
        };
        let (ziel, layout) = match (wunsch.hochkant, wahl.as_ref()) {
            (Some(ziel), Some(wahl)) => (Some(ziel), Some(wahl)),
            (Some(_), None) => {
                return Err(
                    "Die Hochkantwahl ist eingeschaltet, aber ohne gespeicherte Bildgestaltung."
                        .into(),
                );
            }
            (None, _) => (None, None),
        };
        let hochkant = match (ziel, layout) {
            (Some((breite, hoehe)), Some(wahl)) => {
                let composition =
                    wahl.layout
                        .kompiliere(source.width, source.height, breite, hoehe)?;
                Some(crate::media_output::HochkantWahl {
                    ziel: (breite, hoehe),
                    composition,
                    revision: uplink_core::LayoutRevision {
                        id: wahl.layout_id,
                        revision: wahl.revision,
                    },
                })
            }
            _ => None,
        };
        let preferences = crate::media_output::preferences(
            source,
            output,
            &self.state.config.media.enhanced,
            ziel,
        )?;
        let hardware = self
            .hardware
            .get_or_init(|| uplink_media::platform::hardware::measure(&self.engine_config))
            .await
            .as_ref()
            .map_err(|_| "Die Softwareencoder konnten auf diesem Server nicht bestätigt werden.")?;
        let configuration = uplink_media::platform::twitch::GoLiveClient::new()
            .map_err(twitch_configuration_error)?
            .configure(
                &output.target.playpath,
                hardware,
                &preferences,
                &output.target.allowed_hosts,
            )
            .await
            .map_err(twitch_configuration_error)?;
        crate::media_output::twitch(
            configuration,
            output.live_audio_track,
            output.vod_audio_track,
            hochkant.as_ref(),
            source,
        )
        .map_err(TwitchOutputError::Blocked)
    }

    async fn gespeicherte_hochkant_wahl(
        &self,
        tenant: i64,
    ) -> Result<Option<GespeicherteWahl>, &'static str> {
        let rows = self
            .state
            .store
            .query(
                "SELECT layout_id,revision,layout FROM relay.hochkant_layouts WHERE streamer_id=$1 ORDER BY revision DESC LIMIT 1",
                &[&tenant],
            )
            .await
            .map_err(|_| "Die gespeicherte Hochkantwahl ist nicht lesbar.")?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let layout_id = u64::try_from(
            row.try_get::<_, i64>(0)
                .map_err(|_| "Die gespeicherte Hochkantwahl ist unvollständig.")?,
        )
        .map_err(|_| "Die gespeicherte Hochkantwahl ist unvollständig.")?;
        let revision = u64::try_from(
            row.try_get::<_, i64>(1)
                .map_err(|_| "Die gespeicherte Hochkantwahl ist unvollständig.")?,
        )
        .map_err(|_| "Die gespeicherte Hochkantwahl ist unvollständig.")?;
        if layout_id == 0 || revision == 0 {
            return Err("Die gespeicherte Hochkantwahl trägt keine gültige Revision.");
        }
        let roh: serde_json::Value = row
            .try_get(2)
            .map_err(|_| "Die gespeicherte Hochkantwahl ist unvollständig.")?;
        let layout: crate::hochkant::HochkantLayout = serde_json::from_value(roh).map_err(
            |_| "Die gespeicherte Hochkantwahl wird in dieser Version nicht verstanden.",
        )?;
        Ok(Some(GespeicherteWahl {
            layout_id,
            revision,
            layout,
        }))
    }

    fn zielwunsch(
        &self,
        row: &tokio_postgres::Row,
        tenant: i64,
        platform: String,
    ) -> Result<Zielwunsch, &'static str> {
        let policy = self
            .state
            .config
            .platforms
            .iter()
            .find(|p| p.name == platform)
            .ok_or("Für ein Ziel fehlt die geprüfte Konfiguration.")?;
        let endpoint: String = row.try_get(1).map_err(|_| "Zieladresse fehlt.")?;
        let endpoint = crate::destinations::runtime_endpoint(&platform, &endpoint, policy)?;
        let ciphertext: Vec<u8> = row.try_get(2).map_err(|_| "Zielzugang fehlt.")?;
        let secret = self
            .state
            .secrets
            .encryption
            .open(&ciphertext, &format!("destination:{tenant}:{platform}"))?;
        let positive = |index: usize| -> Result<u32, &'static str> {
            row.try_get::<_, Option<i32>>(index)
                .ok()
                .flatten()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or("Gewünschtes Ausgabeprofil ist noch unvollständig.")
        };
        let audio_mode: Option<String> = row.try_get(7).map_err(|_| "Audiowahl fehlt.")?;
        let use_vod_audio = match (platform.as_str(), audio_mode.as_deref()) {
            ("twitch", Some("live")) => false,
            ("twitch", Some("separate_vod")) => true,
            (_, None) => policy.use_vod_audio,
            _ => return Err("Gespeicherte Audiowahl ist ungültig."),
        };
        let vod_audio_track = if use_vod_audio {
            Some(
                self.state
                    .config
                    .media
                    .vod_audio_track
                    .filter(|track| *track != self.state.config.media.live_audio_track)
                    .ok_or("Für separaten VOD-Ton fehlt die eigene Audiozuordnung.")?,
            )
        } else {
            None
        };
        let generation = row
            .try_get::<_, i64>(8)
            .map_err(|_| "Zielgeneration fehlt.")?;
        let hochkant_enabled = row
            .try_get::<_, Option<bool>>(9)
            .map_err(|_| "Hochkantwahl fehlt.")?
            .unwrap_or(false);
        let hochkant = if hochkant_enabled {
            let width =
                positive(10).map_err(|_| "Die gespeicherte Hochkantbreite ist unvollständig.")?;
            let height =
                positive(11).map_err(|_| "Die gespeicherte Hochkanthöhe ist unvollständig.")?;
            Some((width, height))
        } else {
            None
        };
        let output_mode = match row
            .try_get::<_, String>(12)
            .map_err(|_| "Gespeicherter Ausgabemodus fehlt.")?
            .as_str()
        {
            "single" => crate::destinations::TwitchOutputMode::Single,
            "enhanced" if platform == "twitch" => crate::destinations::TwitchOutputMode::Enhanced,
            _ => return Err("Gespeicherter Ausgabemodus ist ungültig."),
        };
        Ok(Zielwunsch {
            generation,
            hochkant,
            output_mode,
            output: DesiredOutput {
                target: PublishTarget {
                    id: platform,
                    endpoint,
                    playpath: PublishSecret::new(secret.expose().to_vec())
                        .map_err(|_| "Zielzugang ist ungültig.")?,
                    tls: Some(self.tls.clone()),
                    allowed_hosts: policy.allowed_hosts.clone(),
                    allow_loopback: self.state.config.loopback_test_ca.is_some()
                        && self.state.config.ingest_bind.ip().is_loopback(),
                    allow_unencrypted: policy.allow_unencrypted,
                },
                video: DesiredVideo {
                    width: positive(3)?,
                    height: positive(4)?,
                    fps: uplink_core::FrameRate::new(positive(5)?, 1)
                        .map_err(|_| "Bildrate ist ungültig.")?,
                    bitrate_kbps: positive(6)?,
                    codec: policy.video_codec,
                },
                live_audio_track: self.state.config.media.live_audio_track,
                vod_audio_track,
                layout: None,
            },
        })
    }
}

struct Zielwunsch {
    output: DesiredOutput,
    generation: i64,
    hochkant: Option<(u32, u32)>,
    output_mode: crate::destinations::TwitchOutputMode,
}

#[derive(Debug, PartialEq, Eq)]
enum TwitchOutputError {
    Blocked(&'static str),
    Fallback(&'static str),
}
impl From<&'static str> for TwitchOutputError {
    fn from(reason: &'static str) -> Self {
        Self::Blocked(reason)
    }
}

fn twitch_configuration_error(
    error: uplink_media::platform::twitch::GoLiveError,
) -> TwitchOutputError {
    use uplink_media::platform::twitch::GoLiveError;
    match error {
        GoLiveError::Transport | GoLiveError::ServiceUnavailable => TwitchOutputError::Fallback(
            "Twitch stellt den Enhanced-Vertrag gerade nicht bereit. Es läuft ein Einzelstream.",
        ),
        GoLiveError::AccountRejected
        | GoLiveError::WarningNeedsDecision
        | GoLiveError::UnsupportedEncoder
        | GoLiveError::UnsupportedAudioMapping => TwitchOutputError::Fallback(
            "Twitch hat keine hier ausführbare Enhanced-Konfiguration freigegeben. Es läuft ein Einzelstream.",
        ),
        _ => TwitchOutputError::Blocked(error.message()),
    }
}

struct GespeicherteWahl {
    layout_id: u64,
    revision: u64,
    layout: crate::hochkant::HochkantLayout,
}

fn pruefe_publish_generation(granted: i64, expected: i64) -> Result<(), &'static str> {
    if expected < 1 || granted != expected {
        return Err(
            "Der gespeicherte Twitch-Zugang stammt nicht aus der aktuellen Twitch-Verbindung. Twitch im Dashboard neu verbinden.",
        );
    }
    Ok(())
}

fn lokales_testziel(config: &crate::config::Config, target: &PublishTarget) -> bool {
    config.loopback_test_ca.is_some()
        && config.ingest_bind.ip().is_loopback()
        && target.allow_loopback
        && reqwest::Url::parse(&target.endpoint).is_ok_and(|url| {
            url.scheme() == "rtmps"
                && url.host_str() == Some("localhost")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && target.allowed_hosts == ["localhost"]
        })
}

/// Nur der bekannte alte Twitch-Default wird auf den offiziellen sicheren
/// Default abgebildet (https://ingest.twitch.tv/ingests). Keine generische
/// Protokoll-/Hostumschreibung und keine Änderung des gespeicherten Wunsches.
pub(crate) fn secure_default(platform: &str, endpoint: String) -> String {
    if platform == "twitch"
        && matches!(
            endpoint.as_str(),
            "rtmp://live.twitch.tv/app"
                | "rtmp://live.twitch.tv/app/"
                | "rtmp://live.twitch.tv:1935/app"
                | "rtmp://live.twitch.tv:1935/app/"
        )
    {
        "rtmps://ingest.global-contribute.live-video.net:443/app".into()
    } else {
        endpoint
    }
}

fn cast_video_codec(codec: WireCodec) -> Option<&'static str> {
    match codec {
        WireCodec::Av1 => Some("av1"),
        WireCodec::H264 => Some("h264"),
        WireCodec::Hevc => Some("hevc"),
        WireCodec::Aac => None,
    }
}

fn observe_cast_source(event: &MediaEvent, reservation: &crate::registry::Reservation) {
    if event.identity.track.kind != MediaKind::Video
        || event.event_kind != EventKind::SequenceHeader
    {
        return;
    }
    let Some(codec) = cast_video_codec(event.codec) else {
        return;
    };
    reservation.observation(serde_json::json!({
        "codec":codec,
        "cast_input":true,
        "platform_encode":reservation.cast_role() == "program"
    }));
}

impl Coordinator {
    fn spawn_cast_program(
        &self,
        _program: Arc<CastProgram>,
        mut messages: tokio::sync::mpsc::Receiver<CastMessage>,
        reservation: Arc<crate::registry::Reservation>,
        tenant: i64,
    ) {
        let coordinator = self.clone();
        tokio::spawn(async move {
            let first = match messages.recv().await {
                Some(CastMessage::Event(event)) => event,
                Some(CastMessage::Stop) | None => return,
            };
            let (media_tx, media_rx) = tokio::sync::mpsc::channel(256);
            let bridge = tokio::spawn(async move {
                while let Some(message) = messages.recv().await {
                    match message {
                        CastMessage::Event(event) => {
                            if media_tx.send(event).await.is_err() {
                                break;
                            }
                        }
                        CastMessage::Stop => break,
                    }
                }
            });
            let result = coordinator
                .process_output(first, media_rx, reservation.clone(), tenant)
                .await;
            bridge.abort();
            let _ = bridge.await;
            if let Err(error) = result {
                reservation.fail(error);
            }
        });
    }

    async fn process_cast_source(
        &self,
        first: MediaEvent,
        mut events: tokio::sync::mpsc::Receiver<MediaEvent>,
        reservation: Arc<crate::registry::Reservation>,
        tenant: i64,
        source_id: u64,
    ) -> Result<(), &'static str> {
        let tenant_u64 = u64::try_from(tenant).map_err(|_| "Nutzeridentität ist ungültig.")?;
        let (selected_program, selected_preview, _) =
            crate::cast::selected_source(&self.state, tenant).await?;
        self.state
            .registry
            .set_cast_selection(tenant_u64, selected_program, selected_preview);
        self.state
            .registry
            .open_cast_preview_source(tenant_u64, source_id);

        let mut headers = HashMap::<WireTrack, MediaEvent>::new();
        let mut next = Some(first);
        let result = loop {
            let event = match next.take() {
                Some(event) => event,
                None => match events.recv().await {
                    Some(event) => event,
                    None => break Ok(()),
                },
            };
            observe_cast_source(&event, &reservation);
            self.state
                .registry
                .publish_cast_preview(tenant_u64, source_id, &event);
            if event.event_kind == EventKind::SequenceHeader {
                headers.insert(event.identity.track, event.clone());
            }

            // Quellen, die nicht auf Program liegen, werden nur bis hierhin
            // analysiert. Der komprimierte Frame verlässt danach den Scope und
            // gibt sein Ingest-Budget frei; es wird weder FFmpeg noch ein
            // Plattform-Pusher für Standby gestartet.
            if reservation.cast_role() != "program" {
                continue;
            }

            let mut program = self.cast.current(tenant_u64);
            if program.is_none() && is_video_keyframe(&event) {
                let (sender, receiver) = tokio::sync::mpsc::channel(512);
                let (installed, is_new) =
                    self.cast
                        .install(tenant_u64, source_id, event.identity, sender);
                if is_new {
                    self.spawn_cast_program(
                        installed.clone(),
                        receiver,
                        reservation.clone(),
                        tenant,
                    );
                }
                program = Some(installed);
            }
            let Some(program) = program else {
                continue;
            };

            if program.source_is_active(source_id) {
                if event.event_kind == EventKind::SequenceHeader {
                    // Laufende Quellen dürfen ihren Header nicht still gegen
                    // ein anderes Codecprofil tauschen. Revisionen > 1 werden
                    // beim nächsten Medienpaket explizit abgewiesen.
                    continue;
                }
                if let Err(error) = program.route(source_id, &event) {
                    break Err(error);
                }
                continue;
            }

            if is_video_keyframe(&event) {
                if let Err(error) = program.begin_source(source_id, &headers, &event) {
                    break Err(error);
                }
                if let Some(codec) = cast_video_codec(event.codec) {
                    reservation.observation(serde_json::json!({
                        "codec": codec,
                        "cast_input": true,
                        "platform_encode": true,
                        "cast_program_routed": true
                    }));
                }
            }
        };
        self.state
            .registry
            .close_cast_preview_source(tenant_u64, source_id);
        if let Some(program) = self.cast.current(tenant_u64) {
            self.cast.stop_source(&program, source_id);
        }
        result
    }

    async fn process_output(
        &self,
        first: MediaEvent,
        events: tokio::sync::mpsc::Receiver<MediaEvent>,
        reservation: Arc<crate::registry::Reservation>,
        tenant: i64,
    ) -> Result<(), &'static str> {
        reservation.reserve_legacy_capacity()?;
        let rows=self.state.store.query("SELECT d.platform,d.rtmp_url,d.stream_key_enc,d.width,d.height,d.fps,d.bitrate_kbps,d.twitch_audio_mode,COALESCE(f.generation,0),d.hochkant_enabled,d.hochkant_width,d.hochkant_height,d.twitch_output_mode FROM relay.destinations d LEFT JOIN relay.destination_fences f USING(streamer_id,platform) WHERE d.streamer_id=$1 AND d.enabled=true ORDER BY d.platform",&[&tenant]).await?;
        if rows.is_empty() {
            return Err("Kein Ausgabeziel ist aktiviert.");
        }
        let mut outputs = Vec::with_capacity(rows.len());
        for row in rows {
            let platform: String = row.try_get(0).map_err(|_| "Zieldaten sind ungültig.")?;
            match self.zielwunsch(&row, tenant, platform.clone()) {
                Ok(wunsch) => outputs.push(wunsch),
                Err(reason) => reservation.block_output(platform, reason),
            }
        }
        if outputs.is_empty() {
            return Err("Kein sicheres Ausgabeziel ist verfügbar; siehe Zielstatus.");
        }
        let mut diagnostic = uplink_media::PreparationDiagnostic::default();
        if probe_dump_allowed(&self.state.config.media, first.identity.session.tenant_id()) {
            diagnostic.request_probe_dump();
        }
        let mut events = events;
        let selected_audio = outputs
            .iter()
            .flat_map(|wunsch| {
                let output = &wunsch.output;
                std::iter::once(output.live_audio_track).chain(output.vod_audio_track)
            })
            .collect();
        let prepared = self
            .engine
            .prepare_source_diagnosed(first, &mut events, &selected_audio, &mut diagnostic)
            .await;
        if let Some(bytes) = diagnostic.take_probe_dump() {
            let directory = self.state.config.media.work_directory.clone();
            let status = crate::probe_dump::spawn(reservation.clone(), directory, bytes)
                .await
                .unwrap_or(crate::probe_dump::DumpStatus::WriteFailed);
            diagnostic.probe_dump_finished(status.code());
        }
        let prepared = prepared.map_err(|error| {
            preparation_failure(&reservation, error, serde_json::json!(diagnostic))
        })?;
        reservation.observation(
            serde_json::to_value(prepared.observation())
                .map_err(|_| "Eingangsmessung konnte nicht dargestellt werden.")?,
        );
        let mut programs = Vec::new();
        let mut desired = Vec::new();
        for wunsch in outputs {
            let output = &wunsch.output;
            let platform = output.target.id.clone();
            if platform != "twitch" || lokales_testziel(&self.state.config, &output.target) {
                desired.push(wunsch.output);
                continue;
            }
            reservation.requested_output_mode(&platform, wunsch.output_mode);
            if output.video.codec != uplink_core::Codec::H264 {
                reservation.block_output(platform, "Das gespeicherte Twitch-Einzelziel benötigt eine H.264-Ausgabe; der Ausgabevertrag muss korrigiert werden.");
                continue;
            }
            if wunsch.output_mode == crate::destinations::TwitchOutputMode::Single {
                if wunsch.hochkant.is_some() {
                    reservation.output_notice(platform, "Einzelstream sendet nur das Querformat. Für eine zusätzliche Hochkantfassung Enhanced wählen.");
                }
                desired.push(wunsch.output);
                continue;
            }
            let tenant_wert = u64::try_from(tenant).map_err(|_| "Nutzeridentität ist ungültig.")?;
            if let Err(reason) = self
                .twitch_publish_erlaubt(tenant_wert, wunsch.generation)
                .await
            {
                reservation.block_output(platform, reason);
                continue;
            }
            if self.state.config.media.enhanced.profiles.is_empty() {
                reservation.single_fallback(&platform, "Enhanced ist für diese Serverleistung noch nicht freigegeben. Es läuft ein Einzelstream ohne zusätzliche Qualitätsstufen oder Hochkantfassung.");
                desired.push(wunsch.output);
                continue;
            }
            let ergebnis = {
                self.twitch_output(tenant_wert, &wunsch, prepared.observation())
                    .await
            };
            match ergebnis {
                Ok(program) => {
                    if program
                        .video
                        .iter()
                        .filter(|video| video.canvas_index == 0)
                        .map(|video| &video.profile)
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        < 2
                    {
                        reservation.single_fallback(&platform, "Twitch hat keine zusätzlichen Querformat-Qualitätsstufen freigegeben. Es läuft ein Einzelstream.");
                        desired.push(wunsch.output);
                        continue;
                    }
                    if platform == "twitch" {
                        let key =
                            crate::media_output::capacity_key(prepared.observation(), &program);
                        let admitted = reserve_twitch_profile(
                            &self.state.config.media.enhanced,
                            &reservation,
                            &key,
                        );
                        if let Err(reason) = admitted {
                            reservation.media_diagnostic(
                                serde_json::json!({"phase":"capacity", "profile_key":key}),
                            );
                            reservation.single_fallback(&platform, reason);
                            desired.push(wunsch.output);
                            continue;
                        }
                        if let Some(video) =
                            program.video.iter().find(|video| video.canvas_index == 1)
                        {
                            let layout = video.layout.as_ref().expect("Canvas 1 trägt Layout");
                            reservation.freeze_layout(
                                "twitch",
                                layout.revision.id,
                                layout.revision.revision,
                            );
                        }
                    }
                    programs.push(program);
                }
                Err(TwitchOutputError::Blocked(reason)) => {
                    reservation.block_output(platform, reason)
                }
                Err(TwitchOutputError::Fallback(reason)) => {
                    reservation.single_fallback(&platform, reason);
                    desired.push(wunsch.output);
                }
            }
        }
        if programs.is_empty() && desired.is_empty() {
            return Err("Kein ausführbares Ausgabeziel ist verfügbar; siehe Zielstatus.");
        }
        let running = self
            .engine
            .start_prepared_mixed(prepared, desired, programs, events, &mut diagnostic)
            .map_err(|error| {
                preparation_failure(&reservation, error, serde_json::json!(diagnostic))
            })?;
        if let Some(observation) = running.source_observation() {
            reservation.observation(
                serde_json::to_value(observation)
                    .map_err(|_| "Eingangsmessung konnte nicht dargestellt werden.")?,
            );
        }
        let observer = running.observer();
        let completion = running.finish();
        tokio::pin!(completion);
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        let report = loop {
            tokio::select! {
                report=&mut completion=>break report,
                _=interval.tick()=>{reservation.media_status(serde_json::to_value(observer.snapshot()).map_err(|_|"Ausgangsstatus konnte nicht dargestellt werden.")?);}
            }
        };
        reservation.media_status(
            serde_json::to_value(&report.status)
                .map_err(|_| "Ausgangsstatus konnte nicht dargestellt werden.")?,
        );
        if let Some(error) = report.error {
            let mut diagnostic = serde_json::json!(diagnostic);
            diagnostic["phase"] = serde_json::json!("media_output");
            diagnostic["error"] = serde_json::json!(error);
            reservation.media_diagnostic(diagnostic);
            return Err("Medienausgabe wurde mit Fehler beendet.");
        }
        if report.status.outputs.is_empty()
            || report
                .status
                .outputs
                .iter()
                .all(|output| matches!(output.state, uplink_media::OutputState::Failed(_)))
        {
            return Err("Alle Plattformausgänge sind fehlgeschlagen; siehe Zielstatus.");
        }
        Ok(())
    }
}

impl SessionProcessor for Coordinator {
    async fn process(
        &self,
        first: MediaEvent,
        events: tokio::sync::mpsc::Receiver<MediaEvent>,
    ) -> Result<(), &'static str> {
        if self.state.config.test_ingest.is_some() {
            return crate::test_ingest::receive(first, events).await;
        }
        let reservation = first
            .authorization_retention()
            .and_then(|value| value.downcast::<crate::registry::Reservation>().ok())
            .ok_or("Sessionreservierung fehlt.")?;
        let tenant = i64::try_from(first.identity.session.tenant_id())
            .map_err(|_| "Nutzeridentität ist ungültig.")?;
        if let Some(source_id) = reservation.source_id() {
            return self
                .process_cast_source(first, events, reservation, tenant, source_id)
                .await;
        }
        self.process_output(first, events, reservation, tenant)
            .await
    }
}

fn reserve_twitch_profile(
    enhanced: &crate::config::EnhancedConfig,
    reservation: &crate::registry::Reservation,
    key: &str,
) -> Result<(), &'static str> {
    let units = enhanced
        .profiles
        .iter()
        .find(|profile| profile.key == key)
        .map(|profile| profile.units)
        .ok_or("Dieses Qualitätsprofil ist noch nicht durch eine Lastmessung freigegeben.")?;
    reservation.reserve_profile_capacity(units)
}

fn probe_dump_allowed(config: &crate::config::MediaConfig, authenticated_tenant: u64) -> bool {
    config.probe_dump_streamer_id == Some(authenticated_tenant)
}

fn preparation_failure(
    reservation: &crate::registry::Reservation,
    error: uplink_media::MediaError,
    diagnostic: serde_json::Value,
) -> &'static str {
    let mut diagnostic = diagnostic;
    diagnostic["error"] = serde_json::json!(error);
    reservation.media_diagnostic(diagnostic);
    "Medienprofil konnte nicht sicher vorbereitet werden."
}

#[cfg(test)]
mod tests {
    #[test]
    fn enhanced_fallback_preserves_authentication_and_endpoint_rejections() {
        use uplink_media::platform::twitch::GoLiveError;
        for error in [
            GoLiveError::Transport,
            GoLiveError::ServiceUnavailable,
            GoLiveError::AccountRejected,
            GoLiveError::UnsupportedEncoder,
        ] {
            assert!(matches!(
                super::twitch_configuration_error(error),
                super::TwitchOutputError::Fallback(_)
            ));
        }
        for error in [
            GoLiveError::HttpRejected,
            GoLiveError::EndpointRejected,
            GoLiveError::InvalidResponse,
            GoLiveError::ResponseTooLarge,
        ] {
            assert!(matches!(
                super::twitch_configuration_error(error),
                super::TwitchOutputError::Blocked(_)
            ));
        }
    }
    #[test]
    fn configured_twitch_profiles_remain_closed_without_capacity_or_matching_key() {
        for (capacity, requested_key, admitted) in [
            (0, "measured", false),
            (2, "missing", false),
            (2, "measured", true),
        ] {
            let registry = crate::registry::Registry::new(1, 1).unwrap();
            registry.configure_capacity(capacity, 1).unwrap();
            let reservation = registry.reserve(11).unwrap();
            let enhanced = crate::config::EnhancedConfig {
                capacity_units: capacity,
                profiles: vec![crate::config::CapacityProfile {
                    key: "measured".into(),
                    units: 1,
                }],
                ..Default::default()
            };
            let result = super::reserve_twitch_profile(&enhanced, &reservation, requested_key);
            assert_eq!(result.is_ok(), admitted);
            if !admitted {
                assert!(result.unwrap_err().contains("Lastmessung"));
            }
        }
    }

    #[test]
    fn dump_scope_requires_explicit_config_and_matching_authenticated_tenant() {
        let mut config =
            crate::config::Config::parse(include_str!("../../../config/uplink-beispiel.toml"))
                .unwrap();
        assert!(!super::probe_dump_allowed(&config.media, 538636411));
        config.media.probe_dump_streamer_id = Some(538636411);
        assert!(super::probe_dump_allowed(&config.media, 538636411));
        assert!(!super::probe_dump_allowed(&config.media, 1186925760));
        assert!(!super::probe_dump_allowed(&config.media, 0));
    }

    #[test]
    fn preparation_failure_keeps_concrete_media_error_in_internal_completion() {
        let registry = crate::registry::Registry::new(1, 1).unwrap();
        let mut reservation = registry.reserve(538636411).unwrap();
        let completion = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = completion.clone();
        reservation.on_completion(move |value| *captured.lock().unwrap() = Some(value));
        let user_message = super::preparation_failure(
            &reservation,
            uplink_media::MediaError::UnsupportedProfile,
            serde_json::json!({"phase":"graph"}),
        );
        assert_eq!(
            user_message,
            "Medienprofil konnte nicht sicher vorbereitet werden."
        );
        drop(reservation);
        let captured = completion.lock().unwrap();
        let diagnostic = &captured.as_ref().unwrap().profile["media_diagnostic"];
        assert_eq!(
            diagnostic["error"], "unsupported_profile",
            "Die konkrete MediaError-Variante muss den Coordinator überleben"
        );
        assert_eq!(diagnostic["phase"], "graph");
    }
    use super::secure_default;
    #[test]
    fn only_known_twitch_default_uses_the_official_tls_endpoint() {
        assert_eq!(
            secure_default("twitch", "rtmp://live.twitch.tv/app".into()),
            "rtmps://ingest.global-contribute.live-video.net:443/app"
        );
        for (platform, endpoint) in [
            ("kick", "rtmp://live.twitch.tv/app"),
            ("twitch", "rtmp://live-fra.twitch.tv/app"),
            ("twitch", "rtmp://live.twitch.tv:1234/app"),
        ] {
            assert_eq!(secure_default(platform, endpoint.into()), endpoint);
        }
    }

    #[test]
    fn publish_verlangt_aktuelle_positive_generation() {
        assert!(super::pruefe_publish_generation(3, 3).is_ok());
        for (granted, expected) in [(2, 3), (4, 3), (3, 0), (3, -2)] {
            let fehler = super::pruefe_publish_generation(granted, expected).unwrap_err();
            assert!(fehler.contains("Twitch-Verbindung"));
        }
    }

    #[test]
    fn lokaler_key_pfad_braucht_test_ca_und_begrenztes_loopback_ziel() {
        let mut config =
            crate::config::Config::parse(include_str!("../../../config/uplink-beispiel.toml"))
                .unwrap();
        config.ingest_bind = "127.0.0.1:0".parse().unwrap();
        config.chat = None;
        let mut target = uplink_media::PublishTarget {
            id: "twitch".into(),
            endpoint: "rtmps://localhost/live".into(),
            playpath: uplink_media::PublishSecret::new(b"synthetic".to_vec()).unwrap(),
            tls: None,
            allowed_hosts: vec!["localhost".into()],
            allow_loopback: true,
            allow_unencrypted: false,
        };
        assert!(!super::lokales_testziel(&config, &target));
        config.loopback_test_ca = Some("/tmp/public-test-ca.pem".into());
        assert!(super::lokales_testziel(&config, &target));
        target.endpoint = "rtmps://ingest.example/live".into();
        assert!(!super::lokales_testziel(&config, &target));
        target.endpoint = "rtmps://localhost/live".into();
        target.allowed_hosts.push("ingest.example".into());
        assert!(!super::lokales_testziel(&config, &target));
        target.allowed_hosts = vec!["localhost".into()];
        target.allow_loopback = false;
        assert!(!super::lokales_testziel(&config, &target));
        target.allow_loopback = true;
        config.ingest_bind = "0.0.0.0:443".parse().unwrap();
        assert!(!super::lokales_testziel(&config, &target));
    }
}
