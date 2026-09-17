use crate::{api::ServiceState, runtime::SessionProcessor};
use std::sync::Arc;
use uplink_ingest::{MediaEvent, rustls};
use uplink_media::{
    DesiredOutput, DesiredVideo, EngineConfig, MediaEngine, PublishSecret, PublishTarget,
};

pub struct Coordinator {
    state: Arc<ServiceState>,
    engine: MediaEngine,
    tls: Arc<rustls::ClientConfig>,
    engine_config: EngineConfig,
    hardware: tokio::sync::OnceCell<
        Result<uplink_media::platform::hardware::HardwareReport, uplink_media::MediaError>,
    >,
    broker: Option<crate::chat::BotBroker>,
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
            engine_config,
            hardware: tokio::sync::OnceCell::new(),
            broker,
            state,
            engine,
            tls: Arc::new(tls),
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

    async fn native_2k_profile(
        &self,
        tenant: i64,
    ) -> Result<uplink_media::platform::twitch::Native2kClientProfile, &'static str> {
        let rows = self
            .state
            .store
            .query(
                "SELECT twitch_native_2k_hardware FROM relay.users WHERE streamer_id=$1 AND enabled=true",
                &[&tenant],
            )
            .await
            .map_err(|_| "Das 2K-Hardwareprofil konnte nicht gelesen werden.")?;
        let value: Option<serde_json::Value> = rows
            .first()
            .ok_or("Der Uplink-Zugang ist nicht freigeschaltet.")?
            .try_get(0)
            .map_err(|_| "Das 2K-Hardwareprofil ist beschädigt.")?;
        let value = value.ok_or(
            "Für Native Twitch-2K fehlen die Hardwaredaten des tatsächlich streamenden Quellrechners.",
        )?;
        let profile: uplink_media::platform::twitch::Native2kClientProfile =
            serde_json::from_value(value).map_err(|_| "Das 2K-Hardwareprofil ist beschädigt.")?;
        profile
            .validate()
            .map_err(|_| "Das 2K-Hardwareprofil ist ungültig.")?;
        Ok(profile)
    }

    async fn twitch_native_2k_output(
        &self,
        tenant: u64,
        wunsch: &Zielwunsch,
        source: &uplink_media::SourceObservation,
        input_mode: Native2kInputMode,
    ) -> Result<uplink_media::ProgramOutput, TwitchOutputError> {
        if wunsch.hochkant.is_some() {
            return Err(TwitchOutputError::Blocked(
                "Native 2K und Hochkant werden getrennt freigegeben; für diesen Pfad darf keine zweite Canvas aktiv sein.",
            ));
        }
        let erwarteter_codec = match input_mode {
            Native2kInputMode::HevcPassthrough => "hevc",
            Native2kInputMode::Av1Transcode => "av1",
        };
        if source.codec != erwarteter_codec
            || source.width != 2560
            || source.height != 1440
            || source.fps_numerator != 60
            || source.fps_denominator != 1
        {
            return Err(TwitchOutputError::Blocked(match input_mode {
                Native2kInputMode::HevcPassthrough => {
                    "Native Twitch-2K benötigt als Eingang exakt 2560×1440@60 HEVC; AV1/H.264 werden in diesem Modus nicht umkodiert."
                }
                Native2kInputMode::Av1Transcode => {
                    "Native Twitch-2K AV1 benötigt als Eingang exakt 2560×1440@60 AV1; der Modus ist ausschließlich für den gemessenen AV1→HEVC-Serverpfad."
                }
            }));
        }
        let hardware = self
            .hardware
            .get_or_init(|| uplink_media::platform::hardware::measure(&self.engine_config))
            .await
            .as_ref()
            .map_err(|_| {
                TwitchOutputError::Blocked("Die Serverencoder konnten nicht bestätigt werden.")
            })?;
        if !hardware.encoders.iter().any(|probe| {
            probe.initialized
                && probe.codec == uplink_core::Codec::H264
                && probe.encoder == "libx264"
        }) {
            return Err(TwitchOutputError::Blocked(
                "Für die niedrigeren Twitch-2K-Stufen ist libx264 auf diesem Server nicht bestätigt.",
            ));
        }
        if input_mode == Native2kInputMode::Av1Transcode
            && !hardware.encoders.iter().any(|probe| {
                probe.initialized
                    && probe.codec == uplink_core::Codec::Hevc
                    && probe.encoder == "libx265"
            })
        {
            return Err(TwitchOutputError::Blocked(
                "Für AV1→2K ist libx265 auf diesem Server nicht bestätigt.",
            ));
        }
        let tenant = i64::try_from(tenant).map_err(|_| "Nutzeridentität ist ungültig.")?;
        let client = self.native_2k_profile(tenant).await?;
        let mut preferences = crate::media_output::preferences(
            source,
            &wunsch.output,
            &self.state.config.media.enhanced,
            None,
        )?;
        if preferences.maximum_video_tracks < 5 {
            return Err(TwitchOutputError::Blocked(
                "Native 2K benötigt ein Enhanced-Limit von mindestens fünf Videospuren.",
            ));
        }
        // Der gemessene Querformat-Vertrag lag bei rund 20,5 Mbit/s Video plus
        // Audio. Unterhalb dieser Grenze soll Twitch nicht zu einer kleineren
        // Leiter gezwungen werden; der konkrete Vertrag bleibt Twitch-gesteuert.
        if preferences.maximum_aggregate_bitrate < 21_000 {
            return Err(TwitchOutputError::Blocked(
                "Das Enhanced-Bandbreitenlimit ist für Native 2K zu niedrig.",
            ));
        }
        preferences.canvases.truncate(1);
        let configuration = uplink_media::platform::twitch::GoLiveClient::new()
            .map_err(|error| TwitchOutputError::Blocked(error.message()))?
            .configure_native_2k(
                &wunsch.output.target.playpath,
                &client,
                &preferences,
                &wunsch.output.target.allowed_hosts,
            )
            .await
            .map_err(|error| TwitchOutputError::Blocked(error.message()))?;
        crate::media_output::twitch_native_2k(
            configuration,
            wunsch.output.live_audio_track,
            wunsch.output.vod_audio_track,
            source,
            &client,
            input_mode == Native2kInputMode::Av1Transcode,
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
        // Twitch hat keinen nutzerwaehlbaren Audio-Modus mehr. Der Eingang
        // liefert Live auf Wire-Track 0 und den getrennten VOD-Mix auf Track 1;
        // Uplink prueft beide und routet sie deterministisch. Die alte
        // twitch_audio_mode-Spalte bleibt nur fuer rollende Upgrades im SELECT,
        // beeinflusst den Mediengraph aber nicht mehr.
        let use_vod_audio = platform == "twitch" || policy.use_vod_audio;
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
            "native_2k" if platform == "twitch" => crate::destinations::TwitchOutputMode::Native2k,
            "native_2k_av1" if platform == "twitch" => {
                crate::destinations::TwitchOutputMode::Native2kAv1
            }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Native2kInputMode {
    HevcPassthrough,
    Av1Transcode,
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

impl SessionProcessor for Coordinator {
    async fn process(
        &self,
        first: MediaEvent,
        events: tokio::sync::mpsc::Receiver<MediaEvent>,
        source_termination: tokio::sync::watch::Receiver<uplink_media::SourceTermination>,
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
            let native_2k = match wunsch.output_mode {
                crate::destinations::TwitchOutputMode::Native2k => Some((
                    self.state.config.media.enhanced.native_2k_units,
                    Native2kInputMode::HevcPassthrough,
                    "Native Twitch-2K ist auf diesem Server noch nicht durch ein Kapazitätsbudget freigegeben.",
                    "Native 2K: 1440p60 HEVC wird unverändert durchgereicht; nur von Twitch angeforderte niedrigere H.264-Stufen werden serverseitig erzeugt.",
                )),
                crate::destinations::TwitchOutputMode::Native2kAv1 => Some((
                    self.state.config.media.enhanced.native_2k_av1_units,
                    Native2kInputMode::Av1Transcode,
                    "Native Twitch-2K AV1 ist bis zu einer bestandenen AV1→HEVC-Lastmessung gesperrt.",
                    "Native 2K AV1: 1440p60 AV1 wird serverseitig zu Twitch-HEVC gewandelt; die niedrigeren H.264-Stufen werden im selben Mediengraph erzeugt.",
                )),
                _ => None,
            };
            if let Some((units, input_mode, blocked, notice)) = native_2k {
                let av1_test = input_mode == Native2kInputMode::Av1Transcode
                    && self
                        .state
                        .config
                        .media
                        .enhanced
                        .native_2k_av1_test_permits(tenant_wert);
                if units == 0 && !av1_test {
                    reservation.block_output(platform, blocked);
                    continue;
                }
                match self
                    .twitch_native_2k_output(
                        tenant_wert,
                        &wunsch,
                        prepared.observation(),
                        input_mode,
                    )
                    .await
                {
                    Ok(program) => {
                        let capacity = if av1_test {
                            reservation.reserve_exclusive_test_capacity()
                        } else {
                            reservation.reserve_profile_capacity(units)
                        };
                        if let Err(reason) = capacity {
                            reservation.block_output(platform, reason);
                            continue;
                        }
                        reservation.output_notice(platform.clone(), if av1_test {
                            "Experimenteller AV1-2K-Lasttest: exklusives Uplink-Budget, Quellrechner-Hardware an Twitch, HEVC/H.264-Encode auf dem Server. Keine Produktionsfreigabe."
                        } else {
                            notice
                        });
                        programs.push(program);
                    }
                    Err(
                        TwitchOutputError::Blocked(reason) | TwitchOutputError::Fallback(reason),
                    ) => reservation.block_output(platform, reason),
                }
                continue;
            }
            if self.state.config.media.enhanced.profiles.is_empty()
                && self.state.config.media.enhanced.av1_1080_enhanced_units == 0
            {
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
                        let av1_1080 = av1_1080_enhanced_envelope(
                            &self.state.config.media.enhanced,
                            prepared.observation(),
                            &program,
                        );
                        let admitted = if av1_1080 {
                            reservation.reserve_profile_capacity(
                                self.state.config.media.enhanced.av1_1080_enhanced_units,
                            )
                        } else {
                            reserve_twitch_profile(
                                &self.state.config.media.enhanced,
                                &reservation,
                                &key,
                            )
                        };
                        if let Err(reason) = admitted {
                            reservation.media_diagnostic(
                                serde_json::json!({"phase":"capacity", "profile_key":key}),
                            );
                            reservation.single_fallback(&platform, reason);
                            desired.push(wunsch.output);
                            continue;
                        }
                        if av1_1080 {
                            reservation.output_notice(
                                platform.clone(),
                                "Upload-Sparmodus: ein 1080p60-AV1-Masterstream wird serverseitig in die von Twitch angeforderte H.264-Enhanced-Leiter umgesetzt.",
                            );
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
            .start_prepared_mixed_with_termination(
                prepared,
                desired,
                programs,
                events,
                source_termination,
                &mut diagnostic,
            )
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

fn av1_1080_enhanced_envelope(
    enhanced: &crate::config::EnhancedConfig,
    source: &uplink_media::SourceObservation,
    output: &uplink_media::ProgramOutput,
) -> bool {
    if enhanced.av1_1080_enhanced_units == 0
        || source.codec != "av1"
        || source.width != 1920
        || source.height != 1080
        || source.fps_numerator != 60
        || source.fps_denominator != 1
        || !(2..=5).contains(&output.video.len())
    {
        return false;
    }
    let mut aggregate = 0u64;
    let mut top = false;
    for video in &output.video {
        let profile = &video.profile;
        if video.canvas_index != 0
            || video.layout.is_some()
            || profile.codec != uplink_core::Codec::H264
            || profile.width > 1920
            || profile.height > 1080
            || u64::from(profile.fps.numerator()) > 60 * u64::from(profile.fps.denominator())
            || profile.rate.target_kbps > 7_500
            || profile.rate.max_kbps != profile.rate.target_kbps
            || video.bframes != 0
        {
            return false;
        }
        aggregate = aggregate.saturating_add(u64::from(profile.rate.target_kbps));
        top |= profile.width == 1920
            && profile.height == 1080
            && profile.fps.numerator() == 60
            && profile.fps.denominator() == 1;
    }
    top && aggregate <= 20_000
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
    fn measured_av1_1080_envelope_is_narrow_and_fail_closed() {
        use uplink_core::{
            Chroma, Codec, Color, ColorPrimaries, ColorRange, FrameRate, Gop, Matrix, RateControl,
            RateMode, Transfer, VideoProfile,
        };
        use uplink_media::{
            ProgramOutput, ProgramVideo, PublishSecret, PublishTarget, SourceObservation,
        };

        let source = SourceObservation {
            video_wire_track: 0,
            codec: "av1".into(),
            width: 1920,
            height: 1080,
            fps_numerator: 60,
            fps_denominator: 1,
            pixel_format: "yuv420p".into(),
            color_primaries: Some("bt709".into()),
            color_transfer: Some("bt709".into()),
            color_matrix: Some("bt709".into()),
            color_range: Some("tv".into()),
            rate_control: None,
            gop_frames: None,
            audio: Vec::new(),
            sampled_events: 1,
            sampled_bytes: 1,
            sampled_duration_ms: 1000,
        };
        let video = |wire_track, width, height, fps, bitrate| ProgramVideo {
            wire_track,
            canvas_index: 0,
            profile: VideoProfile {
                width,
                height,
                fps: FrameRate::new(fps, 1).unwrap(),
                codec: Codec::H264,
                codec_profile: "high".into(),
                level: "4.2".into(),
                bit_depth: 8,
                chroma: Chroma::Yuv420,
                color: Color {
                    primaries: ColorPrimaries::Bt709,
                    transfer: Transfer::Bt709,
                    matrix: Matrix::Bt709,
                    range: ColorRange::Limited,
                },
                rate: RateControl {
                    mode: RateMode::Cbr,
                    target_kbps: bitrate,
                    max_kbps: bitrate,
                    buffer_kbits: bitrate * 2,
                },
                gop: Gop {
                    keyframe_interval_frames: fps * 2,
                    closed: true,
                },
            },
            bframes: 0,
            layout: None,
        };
        let target = || PublishTarget {
            id: "twitch".into(),
            endpoint: "rtmps://test.example/app".into(),
            playpath: PublishSecret::new(b"synthetic-key".to_vec()).unwrap(),
            tls: None,
            allowed_hosts: vec!["test.example".into()],
            allow_loopback: false,
            allow_unencrypted: false,
        };
        let mut output = ProgramOutput {
            target: target(),
            video: vec![
                video(0, 1920, 1080, 60, 7_500),
                video(1, 1280, 720, 60, 4_500),
                video(2, 854, 480, 30, 2_500),
                video(3, 640, 360, 30, 1_200),
                video(4, 426, 240, 30, 500),
            ],
            audio: Vec::new(),
        };
        let enhanced = crate::config::EnhancedConfig {
            capacity_units: 100,
            av1_1080_enhanced_units: 100,
            ..Default::default()
        };
        assert!(super::av1_1080_enhanced_envelope(
            &enhanced, &source, &output
        ));

        output.video.push(video(5, 320, 180, 30, 200));
        assert!(!super::av1_1080_enhanced_envelope(
            &enhanced, &source, &output
        ));
        output.video.pop();
        output.video[0].profile.width = 2560;
        assert!(!super::av1_1080_enhanced_envelope(
            &enhanced, &source, &output
        ));
        output.video[0].profile.width = 1920;
        let mut wrong_source = source.clone();
        wrong_source.codec = "hevc".into();
        assert!(!super::av1_1080_enhanced_envelope(
            &enhanced,
            &wrong_source,
            &output
        ));
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
