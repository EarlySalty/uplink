use std::{fs, path::PathBuf, process::Command, time::Instant};

const FFMPEG: &str = "/opt/uplink/media/ffmpeg8-c733b4b2/ffmpeg";

fn bounded_arg(index: usize, default: u32, min: u32, max: u32) -> u32 {
    std::env::args()
        .nth(index)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (min..=max).contains(value))
        .unwrap_or(default)
}

fn cpu_sample() -> Option<(u64, u64)> {
    let line = fs::read_to_string("/proc/stat").ok()?.lines().next()?.to_owned();
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
    let cpu = before.zip(after).and_then(|((total_a, idle_a), (total_b, idle_b))| {
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
    let seconds = bounded_arg(1, 4, 2, 20);
    let threads = bounded_arg(2, 2, 1, 16);
    let x265_preset = match std::env::args().nth(3).as_deref() {
        Some("ultrafast") => "ultrafast",
        Some("superfast") => "superfast",
        _ => "fast",
    };
    let source = PathBuf::from(format!("/tmp/uplink-native2k-av1-{}.mkv", std::process::id()));
    let common = vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
    ];

    let mut make = common.clone();
    make.extend([
        "-f", "lavfi", "-i", "testsrc2=size=2560x1440:rate=60", "-t",
        &seconds.to_string(), "-pix_fmt", "yuv420p", "-color_primaries", "bt709",
        "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv",
        "-c:v", "libsvtav1", "-preset", "12", "-svtav1-params", "lp=8:pred-struct=1:irefresh-type=2",
        "-b:v", "6500k", "-maxrate", "6500k", "-bufsize", "13000k", "-g", "120", "-y",
    ].into_iter().map(str::to_owned));
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
    decode.extend(["-i".to_owned(), source.display().to_string(), "-map".to_owned(), "0:v:0".to_owned(), "-f".to_owned(), "null".to_owned(), "-".to_owned()]);
    run("av1_decode", &decode, seconds)?;

    let mut top_only = common.clone();
    top_only.extend(["-i".to_owned(), source.display().to_string()]);
    top_only.extend([
        "-map", "0:v:0", "-c:v", "libx265", "-preset", x265_preset, "-x265-params",
        &format!("log-level=error:strict-cbr=1:pools={threads}:frame-threads={threads}:rc-lookahead=0:bframes=0:open-gop=0"),
        "-b:v", "9000k", "-minrate:v", "9000k", "-maxrate:v", "9000k", "-bufsize:v", "18000k",
        "-g", "120", "-bf", "0", "-threads:v", &threads.to_string(), "-f", "null", "-",
    ].into_iter().map(str::to_owned));
    run("av1_to_hevc_top", &top_only, seconds)?;

    let mut full = common.clone();
    full.extend(["-i".to_owned(), source.display().to_string()]);
    full.extend([
        "-filter_complex",
        "[0:v:0]split=4[top][fhd][hd][low];[top]format=yuv420p[v0];[fhd]scale=1920:1080:flags=lanczos,format=yuv420p[v1];[hd]scale=1280:720:flags=lanczos,format=yuv420p[v2];[low]scale=640:360:flags=lanczos,fps=30,format=yuv420p[v3]",
        "-map", "[v0]", "-map", "[v1]", "-map", "[v2]", "-map", "[v3]",
        "-c:v:0", "libx265", "-preset:v:0", x265_preset, "-x265-params:v:0",
        &format!("log-level=error:strict-cbr=1:pools={threads}:frame-threads={threads}:rc-lookahead=0:bframes=0:open-gop=0"),
        "-b:v:0", "9000k", "-minrate:v:0", "9000k", "-maxrate:v:0", "9000k", "-bufsize:v:0", "18000k", "-g:v:0", "120", "-bf:v:0", "0",
        "-c:v:1", "libx264", "-preset:v:1", "veryfast", "-x264-params:v:1", "nal-hrd=cbr:force-cfr=1", "-b:v:1", "7500k", "-minrate:v:1", "7500k", "-maxrate:v:1", "7500k", "-bufsize:v:1", "15000k", "-g:v:1", "120", "-bf:v:1", "2",
        "-c:v:2", "libx264", "-preset:v:2", "veryfast", "-x264-params:v:2", "nal-hrd=cbr:force-cfr=1", "-b:v:2", "3500k", "-minrate:v:2", "3500k", "-maxrate:v:2", "3500k", "-bufsize:v:2", "7000k", "-g:v:2", "120", "-bf:v:2", "2",
        "-c:v:3", "libx264", "-preset:v:3", "veryfast", "-x264-params:v:3", "nal-hrd=cbr:force-cfr=1", "-b:v:3", "500k", "-minrate:v:3", "500k", "-maxrate:v:3", "500k", "-bufsize:v:3", "1000k", "-g:v:3", "60", "-bf:v:3", "2",
        "-threads:v:0", &threads.to_string(), "-threads:v:1", &threads.to_string(), "-threads:v:2", &threads.to_string(), "-threads:v:3", &threads.to_string(),
        "-f", "null", "-",
    ].into_iter().map(str::to_owned));
    run("av1_to_twitch_2k_full", &full, seconds)?;

    let _ = fs::remove_file(&source);
    Ok(())
}
