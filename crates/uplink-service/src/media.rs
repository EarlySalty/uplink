use crate::{api::ServiceState, runtime::SessionProcessor};
use std::sync::Arc;
use uplink_ingest::{MediaEvent, rustls};
use uplink_media::{
    DesiredOutput, DesiredSessionSpec, DesiredVideo, EngineConfig, MediaEngine, MediaLimits,
    PublishSecret, PublishTarget,
};

pub struct Coordinator {
    state: Arc<ServiceState>,
    engine: MediaEngine,
    tls: Arc<rustls::ClientConfig>,
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
        let engine = MediaEngine::new(EngineConfig {
            ffmpeg: config.ffmpeg.clone(),
            ffprobe: config.ffprobe.clone(),
            work_directory: config.work_directory.clone(),
            limits: MediaLimits::default(),
        })
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
        Ok(Self {
            state,
            engine,
            tls: Arc::new(tls),
        })
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
        crate::destinations::public_endpoint(&endpoint)?;
        let endpoint = secure_default(&platform, endpoint);
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
            vod_audio_track: if policy.use_vod_audio {
                self.state.config.media.vod_audio_track
            } else {
                None
            },
            layout: None,
        })
    }
}

/// Nur der bekannte alte Twitch-Default wird auf den offiziellen sicheren
/// Default abgebildet (https://ingest.twitch.tv/ingests). Keine generische
/// Protokoll-/Hostumschreibung und keine Änderung des gespeicherten Wunsches.
fn secure_default(platform: &str, endpoint: String) -> String {
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
        let rows=self.state.store.query("SELECT platform,rtmp_url,stream_key_enc,width,height,fps,bitrate_kbps FROM relay.destinations WHERE streamer_id=$1 AND enabled=true ORDER BY platform",&[&tenant]).await?;
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
        let running = self
            .engine
            .prepare_and_start(DesiredSessionSpec { first, outputs }, events)
            .await
            .map_err(|_| "Medienprofil konnte nicht sicher vorbereitet werden.")?;
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
        if report.error.is_some() {
            return Err("Medienausgabe wurde mit Fehler beendet.");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
