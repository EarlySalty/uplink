mod config;

use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};
use uplink_core::{AudioAction, AudioPlan, AudioRole, OutputStatus, Plan, Rejection, VideoAction};

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const USAGE: &str = "Aufruf: uplink plan --config <datei.toml>";

fn config_path(args: &[OsString]) -> Result<PathBuf, &'static str> {
    if args.len() != 3 || args[0] != "plan" || args[1] != "--config" {
        return Err(USAGE);
    }
    let path = PathBuf::from(&args[2]);
    if path.extension().is_none_or(|ext| ext != "toml") {
        return Err("Konfiguration muss eine TOML-Datei sein.");
    }
    Ok(path)
}

fn read_config(path: &Path) -> Result<String, &'static str> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "Konfigurationsdatei ist nicht lesbar.")?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err("Konfiguration muss eine reguläre Datei mit höchstens 256 KiB sein.");
    }
    let file = File::open(path).map_err(|_| "Konfigurationsdatei ist nicht lesbar.")?;
    let mut text = String::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Konfigurationsdatei ist nicht als UTF-8 lesbar.")?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err("Konfiguration überschreitet 256 KiB.");
    }
    Ok(text)
}

fn role(role: AudioRole) -> &'static str {
    match role {
        AudioRole::Live => "Live",
        AudioRole::Vod => "VOD",
    }
}

fn audio_line(writer: &mut impl Write, label: &str, audio: &AudioPlan) -> io::Result<()> {
    let action = match audio.action {
        AudioAction::Copy => "kopieren",
        AudioAction::Encode => "neu encodieren",
    };
    writeln!(
        writer,
        "  {label}: Spur {} ({}-Mix, {action})",
        audio.source_track,
        role(audio.role)
    )
}

fn rejection(reason: &Rejection) -> String {
    match reason {
        Rejection::TargetVideoUnsupported => {
            "Ziel unterstützt das gewünschte Videoprofil laut Szenario nicht.".into()
        }
        Rejection::TargetAudioUnsupported(r) => format!(
            "Ziel unterstützt das gewünschte {}-Audioformat laut Szenario nicht.",
            role(*r)
        ),
        Rejection::MissingAudio(r) => format!(
            "Die erforderliche {}-Audiospur fehlt; kein Ersatz durch eine andere Mischung.",
            role(*r)
        ),
        Rejection::VideoWorkerUnavailable => {
            "Für dieses Videoprofil fehlt ein deklarierter Encoder.".into()
        }
        Rejection::AudioWorkerUnavailable(r) => format!(
            "Für den {}-Mix fehlt ein deklarierter Audioencoder.",
            role(*r)
        ),
        Rejection::VideoDecoderUnavailable => {
            "Für das Eingangsprofil fehlt ein deklarierter Decoder.".into()
        }
        Rejection::AudioDecoderUnavailable(r) => format!(
            "Für den {}-Mix fehlt ein deklarierter Audiodecoder.",
            role(*r)
        ),
        Rejection::LayoutWorkerUnavailable => {
            "Für die gewünschte Layoutrevision fehlt ein deklarierter Kompositionspfad.".into()
        }
        Rejection::UpscaleNotApproved => "Hochskalierung ist noch nicht freigegeben.".into(),
        Rejection::FrameDuplicationNotApproved => {
            "Erhöhung der Bildrate ist noch nicht freigegeben.".into()
        }
    }
}

fn show_plan(plan: &Plan, writer: &mut impl Write) -> io::Result<()> {
    writeln!(
        writer,
        "Nur Szenarioplan: Eingänge und Fähigkeiten sind deklariert, nicht live geprüft."
    )?;
    writeln!(
        writer,
        "Video-Encodes: {}; gemeinsam benötigte Video-Decoder: {}",
        plan.encode_groups.len(),
        plan.shared_video_decode_count
    )?;
    for output in &plan.outputs {
        let state = match output.status {
            OutputStatus::Planned => "geplant",
            OutputStatus::PendingInput => "Eingang ausstehend",
            OutputStatus::Rejected => "abgelehnt",
        };
        writeln!(
            writer,
            "{}: {state}, {}×{}, {}/{} fps",
            output.id,
            output.requested_video.width,
            output.requested_video.height,
            output.requested_video.fps.numerator(),
            output.requested_video.fps.denominator()
        )?;
        if let Some(action) = output.video {
            writeln!(
                writer,
                "  Video: {}",
                match action {
                    VideoAction::Copy => "kopieren",
                    VideoAction::Encode => "neu encodieren",
                }
            )?;
        }
        if let Some(audio) = &output.live_audio {
            audio_line(writer, "Live-Ton", audio)?;
        }
        if let Some(audio) = &output.vod_audio {
            audio_line(writer, "VOD-Ton", audio)?;
        }
        for reason in &output.reasons {
            writeln!(writer, "  {}", rejection(reason))?;
        }
    }
    Ok(())
}

fn run() -> Result<u8, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        writeln!(
            io::stdout().lock(),
            "{USAGE}\nPrüft nur eine lokale Szenariodatei und startet keine Streams."
        )
        .map_err(|_| "Ausgabe konnte nicht geschrieben werden.")?;
        return Ok(0);
    }
    let text = read_config(&config_path(&args)?)?;
    let input = config::parse(&text)?;
    let plan = uplink_core::plan(&input).map_err(|error| error.to_string())?;
    show_plan(&plan, &mut io::stdout().lock())
        .map_err(|_| "Ausgabe konnte nicht geschrieben werden.")?;
    Ok(
        if plan
            .outputs
            .iter()
            .any(|output| output.status == OutputStatus::Rejected)
        {
            3
        } else if plan
            .outputs
            .iter()
            .any(|output| output.status == OutputStatus::PendingInput)
        {
            4
        } else {
            0
        },
    )
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{error}");
            ExitCode::from(2)
        }
    }
}
