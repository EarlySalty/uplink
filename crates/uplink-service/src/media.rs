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
    async fn twitch_output(
        &self,
        tenant: u64,
        output: DesiredOutput,
        source: &uplink_media::SourceObservation,
    ) -> Result<uplink_media::ProgramOutput, &'static str> {
        self.broker.as_ref().ok_or("Die Twitch-Kontoprüfung ist noch nicht eingerichtet.")?.publish_grant(tenant).await.map_err(|_| "Der Twitch-Zugang oder sein Kontoinhaber konnte nicht bestätigt werden. Twitch erneut verbinden.")?;
        let preferences =
            crate::media_output::preferences(source, &output, &self.state.config.media.enhanced)?;
        let hardware = self
            .hardware
            .get_or_init(|| uplink_media::platform::hardware::measure(&self.engine_config))
            .await
            .as_ref()
            .map_err(|_| "Die Softwareencoder konnten auf diesem Server nicht bestätigt werden.")?;
        let configuration = uplink_media::platform::twitch::GoLiveClient::new()
            .map_err(|error| error.message())?
            .configure(
                &output.target.playpath,
                hardware,
                &preferences,
                &output.target.allowed_hosts,
            )
            .await
            .map_err(|error| error.message())?;
        crate::media_output::twitch(
            configuration,
            output.live_audio_track,
            output.vod_audio_track,
        )
    }

    fn output(
        &self,
        row: &tokio_postgres::Row,
        tenant: i64,
        platform: String,
    ) -> Result<DesiredOutput, &'static str> {
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
        Ok(DesiredOutput {
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
        })
    }
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
        let rows=self.state.store.query("SELECT platform,rtmp_url,stream_key_enc,width,height,fps,bitrate_kbps,twitch_audio_mode FROM relay.destinations WHERE streamer_id=$1 AND enabled=true ORDER BY platform",&[&tenant]).await?;
        if rows.is_empty() {
            return Err("Kein Ausgabeziel ist aktiviert.");
        }
        let mut outputs = Vec::with_capacity(rows.len());
        for row in rows {
            let platform: String = row.try_get(0).map_err(|_| "Zieldaten sind ungültig.")?;
            match self.output(&row, tenant, platform.clone()) {
                Ok(output) => outputs.push(output),
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
            .flat_map(|output| {
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
        for output in outputs {
            let platform = output.target.id.clone();
            let result = if platform == "twitch" {
                self.twitch_output(tenant as u64, output, prepared.observation())
                    .await
            } else {
                crate::media_output::ordinary(output)
            };
            match result {
                Ok(program) => {
                    if platform == "twitch" {
                        let key =
                            crate::media_output::capacity_key(prepared.observation(), &program);
                        let units = self
                            .state
                            .config
                            .media
                            .enhanced
                            .profiles
                            .iter()
                            .find(|profile| profile.key == key)
                            .map(|profile| profile.units);
                        let admitted = units.ok_or("Dieses Qualitätsprofil ist noch nicht durch eine Lastmessung freigegeben.").and_then(|units| reservation.reserve_profile_capacity(units));
                        if let Err(reason) = admitted {
                            reservation.media_diagnostic(
                                serde_json::json!({"phase":"capacity", "profile_key":key}),
                            );
                            reservation.block_output(platform, reason);
                            continue;
                        }
                    }
                    programs.push(program);
                }
                Err(reason) => reservation.block_output(platform, reason),
            }
        }
        if programs.is_empty() {
            return Err("Kein ausführbares Ausgabeziel ist verfügbar; siehe Zielstatus.");
        }
        let running = self
            .engine
            .start_prepared_program(prepared, programs, events, &mut diagnostic)
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
}
