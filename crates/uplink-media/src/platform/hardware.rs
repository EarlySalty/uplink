//! Begrenzte Linux- und Software-Encoderprüfung. Keine Kapazitätsfreigabe.
use crate::{EngineConfig, MediaError, Result, flv::FlvReader, prepare::ProbeChild};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Semaphore, time::timeout};
use uplink_core::Codec;

#[derive(Debug, Clone, Serialize)]
pub struct HardwareReport {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub system: SystemInfo,
    pub encoders: Vec<EncoderProbe>,
    pub gpu_verified: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuInfo {
    /// Eindeutige Socket-/Core-Paare innerhalb der Affinität. Null: unbekannt.
    pub physical_cores: u32,
    pub logical_cores: u32,
    pub name: Option<String>,
    /// Mittelwert der gerade gemeldeten CPU-MHz, keine garantierte Taktrate.
    pub speed_mhz: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryInfo {
    /// Sichtbare /proc/meminfo-Werte; kein cgroup- oder Sessionbudget.
    pub total_bytes: u64,
    /// MemFree, ohne als reclaimable geschätzte Caches.
    pub free_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub name: String,
    pub version: String,
    pub release: String,
    pub revision: String,
    /// Adressbreite des tatsächlich laufenden Rust-Prozesses.
    pub bits: u32,
    pub arm: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EncoderProbe {
    #[serde(serialize_with = "serialize_codec")]
    pub codec: Codec,
    pub encoder: String,
    /// Nur erfolgreiche Initialisierung und synthetische FLV-Ausgabe.
    pub initialized: bool,
}

fn serialize_codec<S: serde::Serializer>(
    codec: &Codec,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.serialize_str(match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    })
}

const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_CPUS: usize = 65_536;
const FRAMES: usize = 8;
static MEASUREMENT: Semaphore = Semaphore::const_new(1);

/// Prüft drei Software-Encoder nacheinander. Ein Erfolg belegt ausschließlich
/// acht synthetische 320×180-Bilder mit FLV-Konfiguration und Bildpaketen.
/// Weder Echtzeitreserve noch GPU, HDR oder Plattformfreigabe werden abgeleitet.
pub async fn measure(config: &EngineConfig) -> Result<HardwareReport> {
    if !cfg!(target_os = "linux")
        || !config.ffmpeg.is_absolute()
        || !config.ffmpeg.is_file()
        || config.limits.worker_threads == 0
        || config.limits.startup_timeout.is_zero()
        || config.limits.shutdown_timeout.is_zero()
    {
        return Err(MediaError::InvalidConfiguration);
    }
    let _reservation = MEASUREMENT
        .try_acquire()
        .map_err(|_| MediaError::ResourceLimit)?;
    let mut report = host_report().await?;
    let available = std::thread::available_parallelism()
        .map_err(|_| MediaError::Io)?
        .get();
    let threads = config
        .limits
        .worker_threads
        .min(available)
        .min(report.cpu.logical_cores as usize)
        .min(2);
    for (codec, encoder) in [
        (Codec::H264, "libx264"),
        (Codec::Hevc, "libx265"),
        (Codec::Av1, "libsvtav1"),
    ] {
        let initialized = match probe_encoder(config, codec, encoder, threads).await {
            Ok(()) => true,
            Err(MediaError::ProcessCleanupFailed) => return Err(MediaError::ProcessCleanupFailed),
            // The public result explicitly marks this encoder as unverified.
            // Never silently advertise a codec after a failed probe.
            Err(_) => false,
        };
        report.encoders.push(EncoderProbe {
            codec,
            encoder: encoder.into(),
            initialized,
        });
    }
    Ok(report)
}

async fn host_report() -> Result<HardwareReport> {
    let (cpuinfo, status, memory, name, release, version) =
        timeout(Duration::from_secs(2), async {
            tokio::try_join!(
                read_public_file("/proc/cpuinfo", 4 * MAX_OUTPUT),
                read_public_file("/proc/self/status", 65_536),
                read_public_file("/proc/meminfo", 65_536),
                read_public_file("/proc/sys/kernel/ostype", 512),
                read_public_file("/proc/sys/kernel/osrelease", 512),
                read_public_file("/proc/sys/kernel/version", 512),
            )
        })
        .await
        .map_err(|_| MediaError::StartTimeout)??;
    let name = kernel_text(&name)?;
    let release = kernel_text(&release)?;
    let version = kernel_text(&version)?;
    if name != "Linux" {
        return Err(MediaError::UnsupportedProfile);
    }
    let revision = version
        .split_whitespace()
        .next()
        .ok_or(MediaError::InvalidMedia)?
        .to_owned();
    Ok(HardwareReport {
        cpu: parse_cpu(&cpuinfo, &parse_affinity(&status)?)?,
        memory: parse_memory(&memory)?,
        system: SystemInfo {
            name,
            version,
            release,
            revision,
            bits: usize::BITS,
            arm: cfg!(any(target_arch = "arm", target_arch = "aarch64")),
        },
        encoders: Vec::new(),
        gpu_verified: false,
    })
}

async fn read_public_file(path: &'static str, limit: usize) -> Result<String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| MediaError::Io)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| MediaError::Io)?;
    if bytes.len() > limit {
        return Err(MediaError::ResourceLimit);
    }
    String::from_utf8(bytes).map_err(|_| MediaError::InvalidMedia)
}

fn kernel_text(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(MediaError::InvalidMedia);
    }
    Ok(value.to_owned())
}

fn parse_affinity(status: &str) -> Result<BTreeSet<u32>> {
    let mut masks = status
        .lines()
        .filter_map(|line| line.strip_prefix("Cpus_allowed_list:"));
    let mask = masks.next().ok_or(MediaError::InvalidMedia)?.trim();
    if masks.next().is_some() || mask.is_empty() || mask.len() > 65_536 {
        return Err(MediaError::InvalidMedia);
    }
    let mut cpus = BTreeSet::new();
    for range in mask.split(',') {
        let (first, last) = match range.split_once('-') {
            Some((first, last)) => (number(first)?, number(last)?),
            None => {
                let cpu = number(range)?;
                (cpu, cpu)
            }
        };
        if first > last || last > 1_048_575 || (last - first) as usize >= MAX_CPUS - cpus.len() {
            return Err(MediaError::InvalidMedia);
        }
        for cpu in first..=last {
            if !cpus.insert(cpu) {
                return Err(MediaError::InvalidMedia);
            }
        }
    }
    Ok(cpus)
}

fn number(value: &str) -> Result<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MediaError::InvalidMedia);
    }
    value.parse().map_err(|_| MediaError::InvalidMedia)
}

fn parse_cpu(cpuinfo: &str, affinity: &BTreeSet<u32>) -> Result<CpuInfo> {
    if affinity.is_empty() || affinity.len() > MAX_CPUS || cpuinfo.len() > 4 * MAX_OUTPUT {
        return Err(MediaError::InvalidMedia);
    }
    let mut seen = BTreeSet::new();
    let mut cores = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut total_speed = 0.0;
    let mut speeds = 0;
    let mut known_topology = true;
    let mut known_names = true;
    for record in cpuinfo.split("\n\n") {
        let mut fields = BTreeMap::new();
        for line in record.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            if matches!(
                key,
                "processor" | "physical id" | "core id" | "model name" | "cpu MHz"
            ) && fields.insert(key, value.trim()).is_some()
            {
                return Err(MediaError::InvalidMedia);
            }
        }
        let Some(id) = fields.get("processor") else {
            continue;
        };
        let id = number(id)?;
        if !affinity.contains(&id) {
            continue;
        }
        if !seen.insert(id) {
            return Err(MediaError::InvalidMedia);
        }
        match (fields.get("physical id"), fields.get("core id")) {
            (Some(socket), Some(core)) => {
                cores.insert((number(socket)?, number(core)?));
            }
            _ => known_topology = false,
        }
        match fields.get("model name") {
            Some(name)
                if !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control) =>
            {
                names.insert((*name).to_owned());
            }
            None => known_names = false,
            _ => return Err(MediaError::InvalidMedia),
        }
        if let Some(speed) = fields.get("cpu MHz") {
            let speed = speed.parse::<f64>().map_err(|_| MediaError::InvalidMedia)?;
            if !speed.is_finite() || speed <= 0.0 || speed > 1_000_000.0 {
                return Err(MediaError::InvalidMedia);
            }
            total_speed += speed;
            speeds += 1;
        }
    }
    // Containers may expose fewer cpuinfo records than the affinity permits.
    // Only the observed intersection counts; unseen host CPUs are not inferred.
    if seen.is_empty() {
        return Err(MediaError::InvalidMedia);
    }
    Ok(CpuInfo {
        physical_cores: if known_topology {
            cores.len() as u32
        } else {
            0
        },
        logical_cores: seen.len() as u32,
        name: if known_names && names.len() == 1 {
            names.into_iter().next()
        } else {
            None
        },
        speed_mhz: if speeds == seen.len() {
            Some((total_speed / speeds as f64).round() as u32)
        } else {
            None
        },
    })
}

fn parse_memory(meminfo: &str) -> Result<MemoryInfo> {
    let mut total = None;
    let mut free = None;
    for line in meminfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let target = match key {
            "MemTotal" => &mut total,
            "MemFree" => &mut free,
            _ => continue,
        };
        if target.is_some() {
            return Err(MediaError::InvalidMedia);
        }
        let mut words = value.split_whitespace();
        let count = words.next().ok_or(MediaError::InvalidMedia)?;
        if count.is_empty()
            || !count.bytes().all(|byte| byte.is_ascii_digit())
            || words.next() != Some("kB")
            || words.next().is_some()
        {
            return Err(MediaError::InvalidMedia);
        }
        *target = Some(
            count
                .parse::<u64>()
                .map_err(|_| MediaError::InvalidMedia)?
                .checked_mul(1024)
                .ok_or(MediaError::InvalidMedia)?,
        );
    }
    let total_bytes = total.ok_or(MediaError::InvalidMedia)?;
    let free_bytes = free.ok_or(MediaError::InvalidMedia)?;
    if total_bytes == 0 || free_bytes > total_bytes {
        return Err(MediaError::InvalidMedia);
    }
    Ok(MemoryInfo {
        total_bytes,
        free_bytes,
    })
}

async fn probe_encoder(
    config: &EngineConfig,
    codec: Codec,
    encoder: &'static str,
    threads: usize,
) -> Result<()> {
    let mut command = Command::new(&config.ffmpeg);
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "quiet",
            "-max_alloc",
            "67108864",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=25",
            "-map",
            "0:v:0",
            "-an",
            "-frames:v",
            "8",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            encoder,
            "-threads",
        ])
        .arg(threads.to_string());
    match codec {
        Codec::H264 => {
            command.args(["-preset", "ultrafast", "-tune", "zerolatency"]);
        }
        Codec::Hevc => {
            command.args([
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-x265-params",
                "pools=1:frame-threads=1:wpp=0:log-level=error",
            ]);
        }
        Codec::Av1 => {
            command
                .args(["-preset", "12", "-svtav1-params"])
                .arg(format!("lp={threads}"));
        }
    }
    let child = command
        .args(["-f", "flv", "-flvflags", "no_duration_filesize", "pipe:1"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| MediaError::ProcessFailed)?;
    let guard = ProbeChild {
        child: Some(child),
        deadline: config.limits.shutdown_timeout.min(Duration::from_secs(5)),
    };
    let bytes = collect_output(
        guard,
        config.limits.startup_timeout.min(Duration::from_secs(15)),
    )
    .await?;
    validate_output(&bytes, codec).await
}

async fn collect_output(mut guard: ProbeChild, deadline: Duration) -> Result<Vec<u8>> {
    let operation = async {
        let child = guard.child.as_mut().ok_or(MediaError::ProcessFailed)?;
        let stdout = child.stdout.take().ok_or(MediaError::ProcessFailed)?;
        let mut bytes = Vec::new();
        stdout
            .take(MAX_OUTPUT as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| MediaError::Io)?;
        if bytes.len() > MAX_OUTPUT {
            return Err(MediaError::ResourceLimit);
        }
        let exit = child
            .wait()
            .await
            .map_err(|_| MediaError::ProcessCleanupFailed)?;
        if !exit.success() {
            return Err(MediaError::ProcessFailed);
        }
        Ok(bytes)
    };
    let result = timeout(deadline, operation)
        .await
        .map_err(|_| MediaError::StartTimeout)
        .and_then(|value| value);
    guard.terminate().await?;
    result
}

async fn validate_output(bytes: &[u8], codec: Codec) -> Result<()> {
    if bytes.len() > MAX_OUTPUT {
        return Err(MediaError::ResourceLimit);
    }
    let mut reader = FlvReader::new(bytes, 256 * 1024);
    let mut header = false;
    let mut frames = 0;
    let mut tags = 0;
    let mut ended = false;
    let mut last_dts = None;
    while let Some(tag) = reader.next().await? {
        tags += 1;
        if tags > 64 {
            return Err(MediaError::ResourceLimit);
        }
        if tag.kind() == 18 {
            continue;
        }
        if tag.kind() != 9 {
            return Err(MediaError::InvalidMedia);
        }
        let body = tag.body();
        if body.len() < 5 {
            return Err(MediaError::InvalidMedia);
        }
        let (packet, prefix) = if body[0] & 0x80 == 0 {
            if codec != Codec::H264 || body[0] & 0xf != 7 {
                return Err(MediaError::InvalidMedia);
            }
            (body[1], 5)
        } else {
            let fourcc = match codec {
                Codec::H264 => b"avc1",
                Codec::Hevc => b"hvc1",
                Codec::Av1 => b"av01",
            };
            if &body[1..5] != fourcc {
                return Err(MediaError::InvalidMedia);
            }
            let packet = body[0] & 0xf;
            (
                packet,
                if packet == 1 && codec != Codec::Av1 {
                    8
                } else {
                    5
                },
            )
        };
        match packet {
            0 if !header && !ended && body.len() > prefix => header = true,
            1 | 3 if header && !ended && body.len() > prefix => {
                if last_dts.is_some_and(|last| tag.timestamp_ms() < last) {
                    return Err(MediaError::InvalidMedia);
                }
                last_dts = Some(tag.timestamp_ms());
                frames += 1;
            }
            2 if header && !ended => ended = true,
            4 => {}
            _ => return Err(MediaError::InvalidMedia),
        }
    }
    if !header || frames != FRAMES {
        return Err(MediaError::InvalidMedia);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU: &str = "processor: 0\nphysical id: 0\ncore id: 0\nmodel name: Test CPU\ncpu MHz: 3000.0\n\nprocessor: 1\nphysical id: 0\ncore id: 1\nmodel name: Test CPU\ncpu MHz: 3400.0\n\nprocessor: 2\nphysical id: 0\ncore id: 0\nmodel name: Test CPU\ncpu MHz: 2800.0\n\nprocessor: 3\nphysical id: 1\ncore id: 0\nmodel name: Test CPU\ncpu MHz: 2600.0\n";

    #[test]
    fn cpu_topology_counts_only_allowed_threads_and_distinguishes_sockets() {
        let affinity = parse_affinity("Name: ignored\nCpus_allowed_list:\t0,2-3\n").unwrap();
        let cpu = parse_cpu(CPU, &affinity).unwrap();
        assert_eq!(cpu.logical_cores, 3);
        assert_eq!(cpu.physical_cores, 2);
        assert_eq!(cpu.name.as_deref(), Some("Test CPU"));
        assert_eq!(cpu.speed_mhz, Some(2800));
        let hyperthreads = parse_cpu(CPU, &BTreeSet::from([0, 2])).unwrap();
        assert_eq!(hyperthreads.logical_cores, 2);
        assert_eq!(hyperthreads.physical_cores, 1);
    }

    #[test]
    fn virtualized_cpuinfo_cannot_expand_affinity_into_unobserved_cores() {
        let cpu = parse_cpu(CPU, &BTreeSet::from([0, 1, 2, 3, 4, 5])).unwrap();
        assert_eq!(cpu.logical_cores, 4);
        assert_eq!(cpu.physical_cores, 3);
    }

    #[test]
    fn missing_topology_does_not_invent_physical_cores_or_clock_speed() {
        let cpu = parse_cpu("processor: 7\n\nprocessor: 12\n", &BTreeSet::from([7, 12])).unwrap();
        assert_eq!(cpu.logical_cores, 2);
        assert_eq!(cpu.physical_cores, 0);
        assert!(cpu.name.is_none());
        assert!(cpu.speed_mhz.is_none());
    }

    #[test]
    fn invalid_cpu_and_affinity_statistics_are_rejected() {
        for mask in ["", "3-2", "0,0", "0-2,2", "1,,3", "0-4294967295", "bad"] {
            assert!(parse_affinity(&format!("Cpus_allowed_list: {mask}\n")).is_err());
        }
        assert!(parse_affinity("Name: no mask\n").is_err());
        assert!(parse_affinity("Cpus_allowed_list: 0\nCpus_allowed_list: 1\n").is_err());
        assert!(parse_cpu(CPU, &BTreeSet::from([999])).is_err());
        assert!(parse_cpu("processor: 0\n\nprocessor: 0\n", &BTreeSet::from([0])).is_err());
        for speed in ["NaN", "inf", "-1", "0", "1000001", "bad"] {
            assert!(
                parse_cpu(
                    &format!("processor: 0\ncpu MHz: {speed}\n"),
                    &BTreeSet::from([0])
                )
                .is_err()
            );
        }
    }

    #[test]
    fn ram_snapshot_uses_real_kibibytes_and_memfree() {
        let memory =
            parse_memory("MemTotal: 8192 kB\nMemFree: 2048 kB\nMemAvailable: 6000 kB\n").unwrap();
        assert_eq!(memory.total_bytes, 8_388_608);
        assert_eq!(memory.free_bytes, 2_097_152);
    }

    #[test]
    fn missing_overflowing_or_inconsistent_memory_is_not_reported_as_hardware() {
        for input in [
            "MemTotal: 12 kB\n",
            "MemTotal: 0 kB\nMemFree: 0 kB\n",
            "MemTotal: 12 MB\nMemFree: 1 kB\n",
            "MemTotal: 12 kB\nMemFree: 13 kB\n",
            "MemTotal: 12 kB\nMemFree: -1 kB\n",
            "MemTotal: 18446744073709551615 kB\nMemFree: 0 kB\n",
            "MemTotal: 12 kB\nMemTotal: 12 kB\nMemFree: 1 kB\n",
        ] {
            assert!(parse_memory(input).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn linux_snapshot_reports_visible_resources_without_gpu_claims() {
        let report = host_report().await.unwrap();
        assert!(report.cpu.logical_cores > 0);
        assert!(report.cpu.physical_cores <= report.cpu.logical_cores);
        assert!(report.memory.total_bytes > 0);
        assert_eq!(report.system.name, "Linux");
        assert!(report.encoders.is_empty());
        assert!(!report.gpu_verified);
    }

    async fn synthetic_container(codec: Codec) -> Vec<u8> {
        use crate::flv::{FlvTag, HEADER};
        let (header, frame) = match codec {
            Codec::H264 => (vec![0x17, 0, 0, 0, 0, 1], vec![0x27, 1, 0, 0, 0, 2]),
            Codec::Hevc => (
                vec![0x90, b'h', b'v', b'c', b'1', 1],
                vec![0x93, b'h', b'v', b'c', b'1', 2],
            ),
            Codec::Av1 => (
                vec![0x90, b'a', b'v', b'0', b'1', 1],
                vec![0x91, b'a', b'v', b'0', b'1', 2],
            ),
        };
        let mut bytes = HEADER.to_vec();
        FlvTag::new(9, 0, header.into(), 64)
            .unwrap()
            .write_to(&mut bytes)
            .await
            .unwrap();
        for number in 0..FRAMES {
            FlvTag::new(9, number as u32 * 40, frame.clone().into(), 64)
                .unwrap()
                .write_to(&mut bytes)
                .await
                .unwrap();
        }
        bytes
    }

    #[tokio::test]
    async fn encoder_confirmation_requires_matching_header_and_all_frame_packets() {
        for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
            let bytes = synthetic_container(codec).await;
            assert!(validate_output(&bytes, codec).await.is_ok());
            let wrong = if codec == Codec::Av1 {
                Codec::H264
            } else {
                Codec::Av1
            };
            assert!(validate_output(&bytes, wrong).await.is_err());
            assert!(
                validate_output(&bytes[..bytes.len() - 21], codec)
                    .await
                    .is_err()
            );
            assert!(validate_output(&bytes[..13], codec).await.is_err());
            let mut wrong_previous_size = bytes.clone();
            *wrong_previous_size.last_mut().unwrap() ^= 1;
            assert!(validate_output(&wrong_previous_size, codec).await.is_err());
        }
        assert!(validate_output(&[], Codec::H264).await.is_err());
        assert!(
            validate_output(&vec![0; MAX_OUTPUT + 1], Codec::H264)
                .await
                .is_err()
        );
    }

    fn child(command: &str, arguments: &[&str]) -> (ProbeChild, u32) {
        let child = Command::new(command)
            .args(arguments)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        (
            ProbeChild {
                child: Some(child),
                deadline: Duration::from_secs(1),
            },
            pid,
        )
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn encoder_timeout_and_output_limit_kill_and_reap_the_process() {
        for (program, arguments, deadline, expected) in [
            (
                "/usr/bin/sleep",
                vec!["20"],
                Duration::from_millis(100),
                MediaError::StartTimeout,
            ),
            (
                "/usr/bin/head",
                vec!["-c", "1048577", "/dev/zero"],
                Duration::from_secs(2),
                MediaError::ResourceLimit,
            ),
            (
                "/usr/bin/false",
                vec![],
                Duration::from_secs(2),
                MediaError::ProcessFailed,
            ),
        ] {
            let (guard, pid) = child(program, &arguments);
            assert_eq!(collect_output(guard, deadline).await, Err(expected));
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelling_encoder_measurement_kills_and_reaps_its_process() {
        let (guard, pid) = child("/usr/bin/sleep", &["20"]);
        let task = tokio::spawn(collect_output(guard, Duration::from_secs(10)));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        timeout(Duration::from_secs(2), async {
            while std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Abgebrochene Encodermessung muss ihren Prozess abholen");
    }
}
