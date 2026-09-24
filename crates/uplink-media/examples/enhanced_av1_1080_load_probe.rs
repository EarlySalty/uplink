use std::{fs, os::unix::fs::DirBuilderExt, path::PathBuf, process::Command, time::Instant};

const FFMPEG: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg";

struct PrivateDirectory(PathBuf);

impl PrivateDirectory {
    fn create() -> Result<Self, String> {
        for _ in 0..16 {
            let mut random = [0u8; 8];
            getrandom::fill(&mut random).map_err(|_| "Zufall nicht verfügbar".to_owned())?;
            let suffix = random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = PathBuf::from(format!("/tmp/uplink-enhanced-av1-{suffix}"));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => {
                    return Err("Privater Benchmarkordner konnte nicht angelegt werden".into());
                }
            }
        }
        Err("Kein freier Benchmarkordner".into())
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn bounded_arg(index: usize, default: u32, min: u32, max: u32) -> u32 {
    std::env::args()
        .nth(index)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(default)
}

fn cpu_sample() -> Option<(u64, u64)> {
    let line = fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .next()?
        .to_owned();
    let mut values = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse::<u64>().ok());
    let user = values.next()?;
    let nice = values.next()?;
    let system = values.next()?;
    let idle = values.next()?;
    let iowait = values.next().unwrap_or(0);
    let irq = values.next().unwrap_or(0);
    let softirq = values.next().unwrap_or(0);
    let steal = values.next().unwrap_or(0);
    Some((
        user + nice + system + idle + iowait + irq + softirq + steal,
        idle + iowait,
    ))
}

fn run(label: &str, args: &[String], seconds: u32) -> Result<(), String> {
    let before = cpu_sample();
    let started = Instant::now();
    let status = Command::new("/usr/bin/nice")
        .args(["-n", "19", FFMPEG])
        .args(args)
        .status()
        .map_err(|_| "Benchmarkprozess konnte nicht gestartet werden".to_owned())?;
    let elapsed = started.elapsed().as_secs_f64();
    let after = cpu_sample();
    if !status.success() {
        return Err(format!("{label} ist fehlgeschlagen"));
    }
    let cpu = before
        .zip(after)
        .and_then(|((total_a, idle_a), (total_b, idle_b))| {
            let total = total_b.checked_sub(total_a)?;
            let idle = idle_b.checked_sub(idle_a)?;
            (total > 0).then_some(100.0 * (total - idle) as f64 / total as f64)
        });
    println!(
        "{label}: wall={elapsed:.2}s media={seconds}s speed={:.2}x host_cpu={:.1}%",
        f64::from(seconds) / elapsed,
        cpu.unwrap_or_default()
    );
    Ok(())
}

fn main() -> Result<(), String> {
    let seconds = bounded_arg(1, 6, 2, 20);
    let threads = bounded_arg(2, 2, 1, 8);
    let directory = PrivateDirectory::create()?;
    let source = directory.0.join("source.mkv");
    let common = vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
    ];

    let mut make = common.clone();
    make.extend(
        [
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=60",
            "-t",
            &seconds.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-c:v",
            "libsvtav1",
            "-preset",
            "12",
            "-svtav1-params",
            "lp=8:pred-struct=1:irefresh-type=2",
            "-b:v",
            "5000k",
            "-maxrate",
            "5000k",
            "-bufsize",
            "10000k",
            "-g",
            "120",
            "-y",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    make.push(source.display().to_string());
    let status = Command::new("/usr/bin/nice")
        .args(["-n", "19", FFMPEG])
        .args(&make)
        .status()
        .map_err(|_| "AV1-Testquelle konnte nicht erzeugt werden".to_owned())?;
    if !status.success() {
        return Err("AV1-Testquelle konnte nicht erzeugt werden".to_owned());
    }

    let mut decode = common.clone();
    decode.extend([
        "-i".to_owned(),
        source.display().to_string(),
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-f".to_owned(),
        "null".to_owned(),
        "-".to_owned(),
    ]);
    run("av1_1080_decode", &decode, seconds)?;

    let mut full = common.clone();
    full.extend(["-i".to_owned(), source.display().to_string()]);
    full.extend([
        "-filter_complex",
        "[0:v:0]split=5[top][hd][sd][low][mobile];[top]format=yuv420p[v0];[hd]scale=1280:720:flags=bicubic,format=yuv420p[v1];[sd]scale=854:480:flags=bicubic,fps=30,format=yuv420p[v2];[low]scale=640:360:flags=bicubic,fps=30,format=yuv420p[v3];[mobile]scale=426:240:flags=bicubic,fps=30,format=yuv420p[v4]",
        "-map", "[v0]", "-map", "[v1]", "-map", "[v2]", "-map", "[v3]", "-map", "[v4]",
        "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency", "-x264-params", "nal-hrd=cbr:force-cfr=1", "-profile:v", "high", "-bf", "0",
        "-b:v:0", "7500k", "-minrate:v:0", "7500k", "-maxrate:v:0", "7500k", "-bufsize:v:0", "15000k", "-g:v:0", "120",
        "-b:v:1", "4500k", "-minrate:v:1", "4500k", "-maxrate:v:1", "4500k", "-bufsize:v:1", "9000k", "-g:v:1", "120",
        "-b:v:2", "2500k", "-minrate:v:2", "2500k", "-maxrate:v:2", "2500k", "-bufsize:v:2", "5000k", "-g:v:2", "60",
        "-b:v:3", "1200k", "-minrate:v:3", "1200k", "-maxrate:v:3", "1200k", "-bufsize:v:3", "2400k", "-g:v:3", "60",
        "-b:v:4", "500k", "-minrate:v:4", "500k", "-maxrate:v:4", "500k", "-bufsize:v:4", "1000k", "-g:v:4", "60",
        "-threads:v:0", &threads.to_string(), "-threads:v:1", &threads.to_string(), "-threads:v:2", &threads.to_string(), "-threads:v:3", &threads.to_string(), "-threads:v:4", &threads.to_string(),
        "-f", "null", "-",
    ].into_iter().map(str::to_owned));
    run("av1_1080_to_five_h264", &full, seconds)?;

    Ok(())
}
