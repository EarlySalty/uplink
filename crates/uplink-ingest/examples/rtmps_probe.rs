//! Ausführbarer lokaler FFmpeg-RTMPS-Nachweis, kein öffentlicher Ingestdienst.
//!
//! Aus dem Repository: cargo run -p uplink-ingest --example rtmps_probe --
//! --ffmpeg /absoluter/pfad/zu/ffmpeg8

#[path = "../tests/support/tls.rs"]
mod tls;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{ExitCode, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    time::timeout,
};
use uplink_ingest::*;

type ProbeResult<T> = Result<T, String>;
const DEADLINE: Duration = Duration::from_secs(20);
const CAPTURE_LIMIT: u64 = 64 * 1024;
const FIXTURE_LIMIT: u64 = 2 * 1024 * 1024;
const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../experiments/scuffle-probe/fixtures"
);
// FFmpeg 8 fügt beim erneuten Muxen der AV1-Testquelle matrixCoefficients=0
// hinzu. Das ist eine gemessene Änderung der Metadaten, kein Farbraumnachweis.
const AV1_COPY_COLOR_INFO: &[u8] = concat!(
    "\x02\x00\x09colorInfo\x03",
    "\x00\x0bcolorConfig\x03",
    "\x00\x12matrixCoefficients",
    "\x00",                             // AMF Number
    "\x00\x00\x00\x00\x00\x00\x00\x00", // 0.0
    "\x00\x00\x09\x00\x00\x09"
)
.as_bytes();
const AAC_MONO_CHANNEL_CONFIG: &[u8] = &[1, 1, 0, 0, 0, 4];

#[derive(Default)]
struct ProbeAuth {
    calls: AtomicUsize,
}

impl Authorizer for ProbeAuth {
    async fn authorize(&self, app: &str, stream: &str) -> Result<AuthorizedSession, ()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if app == "live" && stream == "probe" {
            AuthorizedSession::new(1, 1).map_err(|_| ())
        } else {
            Err(())
        }
    }
}

/// Diese Datei enthält ausschließlich ein frisch erzeugtes öffentliches Zertifikat.
/// Der private TLS-Schlüssel bleibt im Prozess des Testservers.
struct PublicCertificateFile {
    path: PathBuf,
    present: bool,
}

impl PublicCertificateFile {
    fn create(public_pem: &str) -> ProbeResult<Self> {
        if !public_pem.starts_with("-----BEGIN CERTIFICATE-----\n")
            || !public_pem.trim_end().ends_with("-----END CERTIFICATE-----")
            || public_pem.contains("PRIVATE KEY")
        {
            return Err("Die Test-CA ist kein öffentliches PEM-Zertifikat".into());
        }
        for _ in 0..16 {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).map_err(|_| "Zufall für Test-CA nicht verfügbar")?;
            let suffix = format!("{:x}", Sha256::digest(random));
            let path = std::env::temp_dir().join(format!("uplink-rtmps-ca-{suffix}.pem"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let owned = Self {
                        path,
                        present: true,
                    };
                    file.write_all(public_pem.as_bytes())
                        .map_err(|_| "Öffentliche Test-CA konnte nicht geschrieben werden")?;
                    return Ok(owned);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("Öffentliche Test-CA konnte nicht angelegt werden".into()),
            }
        }
        Err("Kein freier Dateiname für die öffentliche Test-CA".into())
    }

    fn remove(mut self) -> ProbeResult<()> {
        std::fs::remove_file(&self.path)
            .map_err(|_| "Öffentliche Test-CA konnte nicht entfernt werden")?;
        self.present = false;
        Ok(())
    }
}

impl Drop for PublicCertificateFile {
    fn drop(&mut self) {
        if !self.present {
            return;
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => eprintln!("Die eigene öffentliche Test-CA konnte nicht aufgeräumt werden"),
        }
    }
}

#[derive(Deserialize)]
struct Manifest {
    fixture_specific_mapping: Vec<TrackMapping>,
    files: Vec<FixtureFile>,
}

#[derive(Deserialize)]
struct TrackMapping {
    ffprobe_index: usize,
    media_kind: String,
    flv_track_id: u8,
    packets: usize,
}

#[derive(Deserialize)]
struct FixtureFile {
    path: String,
    bytes: usize,
    sha256: String,
}

#[derive(Clone, Deserialize)]
struct ReferencePacket {
    stream_index: usize,
    pts: i64,
    dts: i64,
    size: String,
    data_hash: String,
}

#[derive(Deserialize)]
struct ReferenceStream {
    index: usize,
    extradata_hash: String,
}

#[derive(Deserialize)]
struct Reference {
    packets: Vec<ReferencePacket>,
    streams: Vec<ReferenceStream>,
}

struct ExpectedTrack {
    codec: WireCodec,
    header_hash: String,
    packets: Vec<ReferencePacket>,
    headers_seen: usize,
    packets_seen: usize,
    expected_metadata: Option<&'static [u8]>,
    metadata_seen: usize,
    expects_sequence_end: bool,
    sequence_ends_seen: usize,
}

impl ExpectedTrack {
    fn observe_metadata(&mut self, payload: &[u8]) -> ProbeResult<()> {
        if self.headers_seen != 1
            || self.expected_metadata != Some(payload)
            || self.metadata_seen != 0
            || self.sequence_ends_seen != 0
        {
            return Err("Fehlzugeordnete, zusätzliche oder abweichende Testmetadaten".into());
        }
        self.metadata_seen = 1;
        Ok(())
    }

    fn observe_sequence_end(&mut self, payload: &[u8]) -> ProbeResult<()> {
        if !payload.is_empty()
            || !self.expects_sequence_end
            || self.sequence_ends_seen != 0
            || self.headers_seen != 1
            || self.packets_seen != self.packets.len()
        {
            return Err("Zusätzliches, vorzeitiges oder abweichendes Test-SequenceEnd".into());
        }
        self.sequence_ends_seen = 1;
        Ok(())
    }
}

struct Measurement {
    events: usize,
    bytes: usize,
    packets: usize,
    headers: usize,
    metadata: usize,
    endings: usize,
    common_shift_ms: Option<i64>,
    generation: Option<ConnectionGeneration>,
    tracks: BTreeMap<WireTrack, ExpectedTrack>,
}

fn read_fixture(name: &str, manifest: &Manifest) -> ProbeResult<Vec<u8>> {
    let expected = manifest
        .files
        .iter()
        .find(|file| file.path == name)
        .ok_or("Datei fehlt im Referenzmanifest")?;
    let mut bytes = Vec::new();
    File::open(Path::new(FIXTURES).join(name))
        .map_err(|_| "Referenzdatei fehlt")?
        .take(FIXTURE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Referenzdatei konnte nicht gelesen werden")?;
    if bytes.len() as u64 > FIXTURE_LIMIT
        || bytes.len() != expected.bytes
        || format!("{:x}", Sha256::digest(&bytes)) != expected.sha256
    {
        return Err("Referenzdatei stimmt nicht mit ihrem Größen-/SHA-256-Manifest überein".into());
    }
    Ok(bytes)
}

impl Measurement {
    fn new(codec: WireCodec, name: &str, manifest: &Manifest) -> ProbeResult<Self> {
        // Auch die von FFmpeg später gelesene FLV-Datei wird vor dem Start geprüft.
        read_fixture(&format!("{name}.flv"), manifest)?;
        let reference: Reference =
            serde_json::from_slice(&read_fixture(&format!("{name}.ffprobe.json"), manifest)?)
                .map_err(|_| "Ungültiges FFprobe-Referenzmanifest")?;
        let mut tracks = BTreeMap::new();
        for mapping in &manifest.fixture_specific_mapping {
            let kind = match mapping.media_kind.as_str() {
                "video" => MediaKind::Video,
                "audio" => MediaKind::Audio,
                _ => return Err("Unbekannte Medienart im Referenzmanifest".into()),
            };
            let packets: Vec<_> = reference
                .packets
                .iter()
                .filter(|packet| packet.stream_index == mapping.ffprobe_index)
                .cloned()
                .collect();
            let stream = reference
                .streams
                .iter()
                .find(|stream| stream.index == mapping.ffprobe_index)
                .ok_or("Spurheader fehlt im Referenzmanifest")?;
            if packets.len() != mapping.packets || packets.is_empty() {
                return Err("Paketanzahl stimmt im Referenzmanifest nicht überein".into());
            }
            let key = WireTrack {
                kind,
                wire_id: mapping.flv_track_id,
            };
            // Nur der gemessene, versionierte FFmpeg-8-Testweg: Audio#1 sendet
            // einmal Native-Mono, AV1-Video#0 einmal ColorInfo. Genau H264-Video#0
            // sendet ein SequenceEnd. Andere Clients/Plattformen sind daraus
            // ausdrücklich nicht zu SequenceEnd oder diesen Metadaten verpflichtet.
            let expected_metadata = match (key.kind, key.wire_id, codec) {
                (MediaKind::Audio, 1, _) => Some(AAC_MONO_CHANNEL_CONFIG),
                (MediaKind::Video, 0, WireCodec::Av1) => Some(AV1_COPY_COLOR_INFO),
                _ => None,
            };
            if tracks
                .insert(
                    key,
                    ExpectedTrack {
                        codec: if kind == MediaKind::Video {
                            codec
                        } else {
                            WireCodec::Aac
                        },
                        header_hash: stream.extradata_hash.clone(),
                        packets,
                        headers_seen: 0,
                        packets_seen: 0,
                        expected_metadata,
                        metadata_seen: 0,
                        expects_sequence_end: key.kind == MediaKind::Video
                            && key.wire_id == 0
                            && codec == WireCodec::H264,
                        sequence_ends_seen: 0,
                    },
                )
                .is_some()
            {
                return Err("Doppelte Spuridentität im Referenzmanifest".into());
            }
        }
        if tracks.len() != reference.streams.len()
            || tracks
                .values()
                .map(|track| track.packets.len())
                .sum::<usize>()
                != reference.packets.len()
        {
            return Err("Referenzmanifest enthält nicht zugeordnete Spuren oder Pakete".into());
        }
        Ok(Self {
            events: 0,
            bytes: 0,
            packets: 0,
            headers: 0,
            metadata: 0,
            endings: 0,
            common_shift_ms: None,
            generation: None,
            tracks,
        })
    }

    fn observe(&mut self, event: &MediaEvent) -> ProbeResult<()> {
        if event.identity.session != AuthorizedSession::new(1, 1).map_err(|_| "Testidentität")? {
            return Err("Medien gehören nicht zur autorisierten Testsession".into());
        }
        if *self.generation.get_or_insert(event.identity.generation) != event.identity.generation {
            return Err("Sessiongeneration wechselt innerhalb einer Verbindung".into());
        }
        let track = self
            .tracks
            .get_mut(&event.identity.track)
            .ok_or("Unerwartete Medienspur")?;
        if event.codec != track.codec || event.configuration_revision != 1 {
            return Err("Codec oder Headerrevision weichen ab".into());
        }
        self.events += 1;
        self.bytes += event.wire_body().len();
        match event.event_kind {
            EventKind::SequenceHeader => {
                if hash(event.payload()) != track.header_hash || track.headers_seen != 0 {
                    return Err("SequenceHeader stimmt nicht exakt mit der Referenz überein".into());
                }
                track.headers_seen += 1;
                self.headers += 1;
            }
            EventKind::Frame => {
                if track.headers_seen != 1 {
                    return Err("Medienpaket vor dem geprüften SequenceHeader".into());
                }
                let expected = track
                    .packets
                    .get(track.packets_seen)
                    .ok_or("Zusätzliches Medienpaket")?;
                check_packet(
                    expected,
                    i64::from(event.dts_ms),
                    event.pts_ms,
                    event.payload().len(),
                    &hash(event.payload()),
                    &mut self.common_shift_ms,
                )?;
                track.packets_seen += 1;
                self.packets += 1;
            }
            EventKind::Metadata => {
                println!(
                    "  Gemessene Metadaten {:?}#{}: {} Bytes, Kopf {:02x?}",
                    event.identity.track.kind,
                    event.identity.track.wire_id,
                    event.payload().len(),
                    &event.payload()[..event.payload().len().min(64)]
                );
                track.observe_metadata(event.payload())?;
                self.metadata += 1;
            }
            EventKind::SequenceEnd => {
                track.observe_sequence_end(event.payload())?;
                self.endings += 1;
            }
        }
        Ok(())
    }

    fn complete(&self, report: &SessionReport) -> ProbeResult<()> {
        if self.tracks.values().any(|track| {
            track.headers_seen != 1
                || track.packets_seen != track.packets.len()
                || track.metadata_seen != usize::from(track.expected_metadata.is_some())
                || track.sequence_ends_seen != usize::from(track.expects_sequence_end)
        }) {
            return Err(
                "Spur, Header, Medienpaket, Metadaten oder SequenceEnd weichen vom Testweg ab"
                    .into(),
            );
        }
        if self.headers
            != self
                .tracks
                .values()
                .map(|track| track.headers_seen)
                .sum::<usize>()
            || self.packets
                != self
                    .tracks
                    .values()
                    .map(|track| track.packets_seen)
                    .sum::<usize>()
            || self.metadata
                != self
                    .tracks
                    .values()
                    .map(|track| track.metadata_seen)
                    .sum::<usize>()
            || self.endings
                != self
                    .tracks
                    .values()
                    .map(|track| track.sequence_ends_seen)
                    .sum::<usize>()
            || self.events != self.headers + self.packets + self.metadata + self.endings
        {
            return Err(
                "Gesamtzähler stimmen nicht mit den einzeln geprüften Spurereignissen überein"
                    .into(),
            );
        }
        if !matches!(
            report.reason,
            EndReason::ExplicitStop | EndReason::PeerClosed
        ) || Some(report.generation) != self.generation
            || report.track_count != self.tracks.len()
            || report.received_events != self.events as u64
            || report.received_bytes != self.bytes as u64
            || report.max_queued_bytes == 0
            || report.max_queued_bytes > IngestLimits::local_probe().max_queued_bytes
        {
            return Err(format!(
                "Sessionbericht weicht von den empfangenen Medien ab: {report:?}"
            ));
        }
        Ok(())
    }
}

fn hash(data: &[u8]) -> String {
    format!("SHA256:{:x}", Sha256::digest(data))
}

fn check_packet(
    reference: &ReferencePacket,
    dts: i64,
    pts: i64,
    size: usize,
    data_hash: &str,
    common_shift: &mut Option<i64>,
) -> ProbeResult<()> {
    if reference
        .size
        .parse::<usize>()
        .map_err(|_| "Ungültige Referenzpaketgröße")?
        != size
        || reference.data_hash != data_hash
    {
        return Err("Paketgröße oder SHA-256 stimmt nicht mit der Referenz überein".into());
    }
    let shift = dts
        .checked_sub(reference.dts)
        .ok_or("Zeitstempelüberlauf")?;
    if *common_shift.get_or_insert(shift) != shift || pts.checked_sub(reference.pts) != Some(shift)
    {
        return Err("DTS/PTS weichen über die gemeinsame Zeitverschiebung hinaus ab".into());
    }
    Ok(())
}

async fn read_output(reader: impl AsyncRead + Unpin) -> ProbeResult<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .take(CAPTURE_LIMIT + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|_| "FFmpeg-Ausgabe konnte nicht gelesen werden")?;
    if output.len() as u64 > CAPTURE_LIMIT {
        return Err("FFmpeg-Ausgabe überschreitet das Testlimit".into());
    }
    Ok(output)
}

async fn reap(child: &mut Child) -> ProbeResult<()> {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        // Ein Statusfehler oder ein Exit während start_kill darf wait nicht
        // überspringen. Entscheidend ist das anschließende bestätigte Reaping.
        let _ = child.start_kill();
    }
    timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| "FFmpeg-Prozess konnte nicht rechtzeitig eingesammelt werden")?
        .map_err(|_| "FFmpeg-Prozess konnte nicht eingesammelt werden")?;
    Ok(())
}

async fn ffmpeg_version(executable: &Path) -> ProbeResult<String> {
    let mut child = Command::new(executable)
        .args(["-nostdin", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "FFmpeg konnte nicht gestartet werden")?;
    let output = child.stdout.take().ok_or("FFmpeg-Standardausgabe fehlt");
    let result = match output {
        Ok(output) => timeout(Duration::from_secs(5), async {
            let (output, status) = tokio::try_join!(read_output(output), async {
                child
                    .wait()
                    .await
                    .map_err(|_| "FFmpeg-Prozessstatus fehlt".to_owned())
            })?;
            if !status.success() {
                return Err("FFmpeg-Versionsabfrage fehlgeschlagen".into());
            }
            let first = String::from_utf8(output)
                .map_err(|_| "Ungültige FFmpeg-Versionsausgabe")?
                .lines()
                .next()
                .ok_or("Leere FFmpeg-Versionsausgabe")?
                .to_owned();
            let version = first
                .strip_prefix("ffmpeg version ")
                .ok_or("Unbekannte FFmpeg-Version")?
                .split_whitespace()
                .next()
                .ok_or("FFmpeg-Version fehlt")?;
            if version.trim_start_matches('n').split('.').next() != Some("8") {
                return Err("Diese Probe erfordert ausdrücklich FFmpeg 8".into());
            }
            Ok(first)
        })
        .await
        .map_err(|_| "Frist der FFmpeg-Versionsabfrage überschritten".to_owned())
        .and_then(|r| r),
        Err(error) => Err(error.to_owned()),
    };
    reap(&mut child).await?;
    result
}

async fn exchange(
    server: &IngestServer<ProbeAuth>,
    measurement: &mut Option<Measurement>,
) -> ProbeResult<SessionReport> {
    let mut connection = server
        .accept()
        .await
        .map_err(|_| "Lokale TCP-Annahme fehlgeschlagen")?;
    while let Some(event) = connection.next().await {
        let measurement = measurement
            .as_mut()
            .ok_or("Medien trotz ungültiger TLS-Prüfung")?;
        measurement.observe(&event)?;
        // Nur Hashes und Zähler bleiben erhalten; MediaEvent gibt sein Budget frei.
    }
    Ok(connection.finish().await)
}

struct Attempt {
    report: SessionReport,
    measurement: Option<Measurement>,
    status: ExitStatus,
    stderr: String,
}

async fn send(
    server: &IngestServer<ProbeAuth>,
    executable: &Path,
    certificate: &PublicCertificateFile,
    hostname: &str,
    fixture: &str,
    mut measurement: Option<Measurement>,
) -> ProbeResult<Attempt> {
    let address = server
        .local_addr()
        .map_err(|_| "Lokale Testadresse nicht verfügbar")?;
    let mut child = Command::new(executable)
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-re",
            "-i",
        ])
        .arg(Path::new(FIXTURES).join(format!("{fixture}.flv")))
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-map",
            "0:a:1",
            "-c",
            "copy",
            "-f",
            "flv",
            "-flvflags",
            "no_duration_filesize",
            "-tls_verify",
            "1",
            "-ca_file",
        ])
        .arg(&certificate.path)
        .args(["-verifyhost", hostname])
        // Beim gemessenen FFmpeg/OpenSSL-Build wird ein numerischer URL-Host
        // nicht gegen den Zertifikatsnamen geprüft, selbst mit verifyhost.
        // Deshalb ausschließlich localhost; echter SAN-Mismatch ist Pflichttest.
        .arg(format!("rtmps://localhost:{}/live/probe", address.port()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "FFmpeg-Sender konnte nicht gestartet werden")?;
    let stderr = child.stderr.take().ok_or("FFmpeg-Fehlerausgabe fehlt");
    let result = match stderr {
        Ok(stderr) => timeout(DEADLINE, async {
            tokio::try_join!(
                exchange(server, &mut measurement),
                read_output(stderr),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|_| "FFmpeg-Prozessstatus fehlt".to_owned())
                }
            )
        })
        .await
        .map_err(|_| "Frist der RTMPS-Probe überschritten".to_owned())
        .and_then(|r| r),
        Err(error) => Err(error.to_owned()),
    };
    // Auch bei Parserfehler, TLS-Abbruch und Deadline wird der Fremdprozess beendet und gewartet.
    reap(&mut child).await?;
    let (report, stderr, status) = result?;
    Ok(Attempt {
        report,
        measurement,
        status,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

async fn probe(executable: &Path) -> ProbeResult<()> {
    println!("{}", ffmpeg_version(executable).await?);
    let manifest: Manifest = serde_json::from_str(include_str!(
        "../../../experiments/scuffle-probe/fixtures/manifest.json"
    ))
    .map_err(|_| "Ungültiges Referenzmanifest")?;
    let tls = tls::test_tls();
    let trusted = PublicCertificateFile::create(&tls.certificate_pem)?;
    drop(tls.client);
    let auth = Arc::new(ProbeAuth::default());
    let server =
        IngestServer::bind_loopback(0, tls.server, auth.clone(), IngestLimits::local_probe())
            .await
            .map_err(|_| "Lokaler TLS-Testserver konnte nicht gestartet werden")?;
    let mut previous_generation = None;
    for (name, codec) in [("av1", WireCodec::Av1), ("h264", WireCodec::H264)] {
        let before = auth.calls.load(Ordering::SeqCst);
        let attempt = send(
            &server,
            executable,
            &trusted,
            "localhost",
            name,
            Some(Measurement::new(codec, name, &manifest)?),
        )
        .await?;
        if !attempt.status.success() {
            return Err(format!(
                "FFmpeg-{name} scheitert: {}; {:?}; {}",
                attempt.status, attempt.report.reason, attempt.stderr
            ));
        }
        let measurement = attempt.measurement.ok_or("Medienmessung fehlt")?;
        measurement.complete(&attempt.report)?;
        if auth.calls.load(Ordering::SeqCst) != before + 1
            || previous_generation == Some(attempt.report.generation)
        {
            return Err("Autorisierung oder frische Reconnect-Generation fehlt".into());
        }
        previous_generation = Some(attempt.report.generation);
        println!(
            "{name}: {} geprüfte Pakete, {} Header, {} Spuren, gemeinsame DTS/PTS-Verschiebung {} ms; Metadaten {}, Enden {}, Ereignisse {}, Bytes {}, maximale Queue {} Bytes, Ende {:?}",
            measurement.packets,
            measurement.headers,
            measurement.tracks.len(),
            measurement.common_shift_ms.ok_or("Zeitmessung fehlt")?,
            measurement.metadata,
            measurement.endings,
            measurement.events,
            measurement.bytes,
            attempt.report.max_queued_bytes,
            attempt.report.reason
        );
        println!(
            "  Pakete je geprüfter Spur: {}",
            measurement
                .tracks
                .iter()
                .map(|(key, track)| format!(
                    "{:?}#{}={}",
                    key.kind, key.wire_id, track.packets_seen
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let unrelated_tls = tls::test_tls();
    let untrusted = PublicCertificateFile::create(&unrelated_tls.certificate_pem)?;
    drop(unrelated_tls);
    let wrong_name_tls = tls::test_tls_for_names(vec!["wrong.invalid".into()]);
    let wrong_name_certificate = PublicCertificateFile::create(&wrong_name_tls.certificate_pem)?;
    drop(wrong_name_tls.client);
    let wrong_name_auth = Arc::new(ProbeAuth::default());
    let wrong_name_server = IngestServer::bind_loopback(
        0,
        wrong_name_tls.server,
        wrong_name_auth.clone(),
        IngestLimits::local_probe(),
    )
    .await
    .map_err(|_| "TLS-Testserver mit falschem SAN konnte nicht gestartet werden")?;
    for (name, active_server, active_auth, certificate) in [
        ("Falsche CA", &server, &auth, &untrusted),
        (
            "Falscher Zertifikat-Hostname",
            &wrong_name_server,
            &wrong_name_auth,
            &wrong_name_certificate,
        ),
    ] {
        let before = active_auth.calls.load(Ordering::SeqCst);
        let attempt = send(
            active_server,
            executable,
            certificate,
            "localhost",
            "av1",
            None,
        )
        .await?;
        if attempt.status.success()
            || attempt.report.reason != EndReason::TlsRejected
            || attempt.report.received_events != 0
            || attempt.report.received_bytes != 0
            || attempt.report.track_count != 0
            || active_auth.calls.load(Ordering::SeqCst) != before
        {
            return Err(format!(
                "{name}: TLS-Negativnachweis fehlgeschlagen; {:?}",
                attempt.report
            ));
        }
        println!(
            "{name}: FFmpeg {}, TLS abgelehnt, 0 Publish-Autorisierungen, 0 Medienereignisse",
            attempt.status
        );
    }
    wrong_name_certificate.remove()?;
    untrusted.remove()?;
    trusted.remove()?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let flag = arguments.next();
    let executable = arguments.next().map(PathBuf::from);
    if flag.as_deref() != Some(std::ffi::OsStr::new("--ffmpeg"))
        || executable
            .as_ref()
            .is_none_or(|path| !path.is_absolute() || !path.is_file())
        || arguments.next().is_some()
    {
        eprintln!("Aufruf: rtmps_probe --ffmpeg /absoluter/pfad/zu/ffmpeg8");
        return ExitCode::from(2);
    }
    match probe(&executable.expect("validated explicit executable")).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("RTMPS-Probe fehlgeschlagen: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn completed_measurement(name: &str, codec: WireCodec) -> (Measurement, SessionReport) {
        let manifest: Manifest = serde_json::from_str(include_str!(
            "../../../experiments/scuffle-probe/fixtures/manifest.json"
        ))
        .unwrap();
        let mut measurement = Measurement::new(codec, name, &manifest).unwrap();
        // Die öffentliche API erzeugt die opake Verbindungsgeneration selbst.
        // Ein sofort geschlossener lokaler TCP-Client reicht für diese Identität;
        // dieser Helfer behauptet ausdrücklich keinen Medien-Livenachweis.
        let server = IngestServer::bind_loopback(
            0,
            tls::test_tls().server,
            Arc::new(ProbeAuth::default()),
            IngestLimits::local_probe(),
        )
        .await
        .unwrap();
        let client = tokio::net::TcpStream::connect(server.local_addr().unwrap())
            .await
            .unwrap();
        let connection = server.accept().await.unwrap();
        drop(client);
        let mut report = timeout(Duration::from_secs(2), connection.finish())
            .await
            .unwrap();
        for track in measurement.tracks.values_mut() {
            track.headers_seen = 1;
            track.packets_seen = track.packets.len();
            track.metadata_seen = usize::from(track.expected_metadata.is_some());
            track.sequence_ends_seen = usize::from(track.expects_sequence_end);
        }
        measurement.packets = 240;
        measurement.headers = 3;
        measurement.metadata = if codec == WireCodec::Av1 { 2 } else { 1 };
        measurement.endings = usize::from(codec == WireCodec::H264);
        measurement.events = 245;
        measurement.bytes = 1;
        measurement.common_shift_ms = Some(0);
        measurement.generation = Some(report.generation);
        report.reason = EndReason::ExplicitStop;
        report.received_events = measurement.events as u64;
        report.received_bytes = measurement.bytes as u64;
        report.max_queued_bytes = 1;
        report.track_count = measurement.tracks.len();
        measurement.complete(&report).unwrap();
        (measurement, report)
    }

    #[tokio::test]
    async fn omitted_metadata_fails_even_when_the_session_counters_agree() {
        for (name, codec) in [("av1", WireCodec::Av1), ("h264", WireCodec::H264)] {
            let (mut measurement, mut report) = completed_measurement(name, codec).await;
            measurement
                .tracks
                .get_mut(&WireTrack {
                    kind: MediaKind::Audio,
                    wire_id: 1,
                })
                .unwrap()
                .metadata_seen -= 1;
            measurement.metadata -= 1;
            measurement.events -= 1;
            report.received_events -= 1;
            assert!(
                measurement.complete(&report).is_err(),
                "{name}: fehlende Metadaten"
            );
        }
    }

    #[tokio::test]
    async fn duplicated_metadata_fails_even_when_the_session_counters_agree() {
        for (name, codec) in [("av1", WireCodec::Av1), ("h264", WireCodec::H264)] {
            let (mut measurement, mut report) = completed_measurement(name, codec).await;
            measurement
                .tracks
                .get_mut(&WireTrack {
                    kind: MediaKind::Audio,
                    wire_id: 1,
                })
                .unwrap()
                .metadata_seen += 1;
            measurement.metadata += 1;
            measurement.events += 1;
            report.received_events += 1;
            assert!(
                measurement.complete(&report).is_err(),
                "{name}: doppelte Metadaten"
            );
        }
    }

    #[tokio::test]
    async fn omitted_h264_sequence_end_fails_even_when_the_session_counters_agree() {
        let (mut measurement, mut report) = completed_measurement("h264", WireCodec::H264).await;
        measurement
            .tracks
            .get_mut(&WireTrack {
                kind: MediaKind::Video,
                wire_id: 0,
            })
            .unwrap()
            .sequence_ends_seen = 0;
        measurement.endings = 0;
        measurement.events -= 1;
        report.received_events -= 1;
        assert!(measurement.complete(&report).is_err());
    }

    #[tokio::test]
    async fn extra_sequence_end_fails_for_both_measured_codec_paths() {
        for (name, codec) in [("av1", WireCodec::Av1), ("h264", WireCodec::H264)] {
            let (mut measurement, mut report) = completed_measurement(name, codec).await;
            measurement
                .tracks
                .get_mut(&WireTrack {
                    kind: MediaKind::Video,
                    wire_id: 0,
                })
                .unwrap()
                .sequence_ends_seen += 1;
            measurement.endings += 1;
            measurement.events += 1;
            report.received_events += 1;
            assert!(
                measurement.complete(&report).is_err(),
                "{name}: zusätzliches Ende"
            );
        }
    }

    #[tokio::test]
    async fn metadata_on_the_wrong_track_cannot_replace_the_expected_event() {
        let (mut measurement, report) = completed_measurement("av1", WireCodec::Av1).await;
        measurement
            .tracks
            .get_mut(&WireTrack {
                kind: MediaKind::Audio,
                wire_id: 0,
            })
            .unwrap()
            .metadata_seen = 1;
        measurement
            .tracks
            .get_mut(&WireTrack {
                kind: MediaKind::Audio,
                wire_id: 1,
            })
            .unwrap()
            .metadata_seen = 0;
        assert_eq!(measurement.metadata, 2);
        assert!(measurement.complete(&report).is_err());
    }

    #[tokio::test]
    async fn per_track_observation_rejects_duplicates_and_unexpected_auxiliary_events() {
        for (name, codec) in [("av1", WireCodec::Av1), ("h264", WireCodec::H264)] {
            let (mut measurement, _) = completed_measurement(name, codec).await;
            for track in measurement.tracks.values_mut() {
                // Vollständig beobachtete Ereignisse dürfen nicht erneut gezählt
                // werden; nicht erwartete Formen bleiben selbst mit gültigen Bytes abgewiesen.
                assert!(
                    track
                        .observe_metadata(
                            track.expected_metadata.unwrap_or(AAC_MONO_CHANNEL_CONFIG)
                        )
                        .is_err()
                );
                assert!(track.observe_sequence_end(&[]).is_err());
                track.metadata_seen = 0;
                track.sequence_ends_seen = 0;
                if let Some(expected) = track.expected_metadata {
                    assert!(track.observe_metadata(&[0]).is_err());
                    track.observe_metadata(expected).unwrap();
                    assert!(track.observe_metadata(expected).is_err());
                } else {
                    assert!(track.observe_metadata(AAC_MONO_CHANNEL_CONFIG).is_err());
                }
                if track.expects_sequence_end {
                    track.packets_seen -= 1;
                    assert!(track.observe_sequence_end(&[]).is_err());
                    track.packets_seen += 1;
                    assert!(track.observe_sequence_end(&[0]).is_err());
                    track.observe_sequence_end(&[]).unwrap();
                    assert!(track.observe_sequence_end(&[]).is_err());
                } else {
                    assert!(track.observe_sequence_end(&[]).is_err());
                }
            }
        }
    }

    fn reference() -> ReferencePacket {
        ReferencePacket {
            stream_index: 0,
            dts: 50,
            pts: 35,
            size: "3".into(),
            data_hash: hash(&[1, 2, 3]),
        }
    }

    #[test]
    fn common_time_shift_preserves_signed_composition_and_rejects_drift() {
        let reference = reference();
        let mut shift = None;
        check_packet(&reference, 71, 56, 3, &reference.data_hash, &mut shift).unwrap();
        assert_eq!(shift, Some(21));
        assert!(check_packet(&reference, 72, 57, 3, &reference.data_hash, &mut shift).is_err());
        assert!(check_packet(&reference, 71, 57, 3, &reference.data_hash, &mut shift).is_err());
    }

    #[test]
    fn packet_hash_and_length_are_both_required() {
        let reference = reference();
        assert!(check_packet(&reference, 50, 65, 3, &hash(&[1, 2, 4]), &mut None).is_err());
        assert!(check_packet(&reference, 50, 65, 4, &reference.data_hash, &mut None).is_err());
    }

    #[test]
    fn both_pinned_fixture_manifests_are_complete() {
        let manifest: Manifest = serde_json::from_str(include_str!(
            "../../../experiments/scuffle-probe/fixtures/manifest.json"
        ))
        .unwrap();
        for (name, codec) in [("av1", WireCodec::Av1), ("h264", WireCodec::H264)] {
            let measurement = Measurement::new(codec, name, &manifest).unwrap();
            assert_eq!(measurement.tracks.len(), 3);
            assert_eq!(
                measurement
                    .tracks
                    .values()
                    .map(|track| track.packets.len())
                    .sum::<usize>(),
                240
            );
            assert!(
                measurement
                    .tracks
                    .values()
                    .all(|track| track.headers_seen == 0)
            );
        }
    }
}
