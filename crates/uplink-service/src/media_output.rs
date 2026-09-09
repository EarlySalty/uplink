use uplink_core::{
    Chroma, Codec, Color, ColorPrimaries, ColorRange, FrameRate, Gop, Matrix, RateControl,
    RateMode, Transfer, VideoProfile,
};
use uplink_media::platform::twitch::{
    AudioRole, Canvas, GoLiveEncoder, Preferences, Rational, TwitchConfiguration,
};
use uplink_media::{
    AudioEncoding, DesiredOutput, ProgramAudio, ProgramOutput, ProgramVideo, SourceObservation,
};

pub fn profile(
    width: u32,
    height: u32,
    fps: FrameRate,
    codec: Codec,
    bitrate: u32,
    seconds: u32,
    codec_profile: String,
) -> Result<VideoProfile, &'static str> {
    let frames = u64::from(fps.numerator())
        .checked_mul(u64::from(seconds))
        .ok_or("Schlüsselbildabstand ist ungültig.")?
        .div_ceil(u64::from(fps.denominator()));
    Ok(VideoProfile {
        width,
        height,
        fps,
        codec,
        codec_profile,
        level: match codec {
            Codec::H264 if u64::from(width) * u64::from(height) <= 1920 * 1080 => "4.2",
            Codec::H264 => "5.2",
            _ => "5.1",
        }
        .into(),
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
            buffer_kbits: bitrate
                .checked_mul(2)
                .ok_or("Ausgangsbitrate ist ungültig.")?,
        },
        gop: Gop {
            keyframe_interval_frames: u32::try_from(frames)
                .map_err(|_| "Schlüsselbildabstand ist ungültig.")?,
            closed: true,
        },
    })
}

pub fn ordinary(output: DesiredOutput) -> Result<ProgramOutput, &'static str> {
    let video = output.video;
    let profile = profile(
        video.width,
        video.height,
        video.fps,
        video.codec,
        video.bitrate_kbps,
        2,
        if video.codec == Codec::H264 {
            "high"
        } else {
            "main"
        }
        .into(),
    )?;
    let audio = std::iter::once(output.live_audio_track)
        .chain(output.vod_audio_track)
        .enumerate()
        .map(|(index, source)| ProgramAudio {
            source_wire_track: source,
            destination_wire_track: index as u8,
            encoding: None,
        })
        .collect();
    Ok(ProgramOutput {
        target: output.target,
        video: vec![ProgramVideo {
            wire_track: 0,
            canvas_index: 0,
            profile,
            layout: output.layout,
        }],
        audio,
    })
}

pub fn preferences(
    source: &SourceObservation,
    output: &DesiredOutput,
    config: &crate::config::EnhancedConfig,
) -> Result<Preferences, &'static str> {
    let audio = source
        .audio
        .iter()
        .find(|audio| audio.wire_track == output.live_audio_track)
        .ok_or("Die für Twitch gewählte Live-Tonspur fehlt.")?;
    if audio.codec != "aac" || audio.sample_rate != 48000 || !(1..=2).contains(&audio.channels) {
        return Err(
            "Der gemessene Twitch-Ton benötigt AAC mit 48 kHz und einem oder zwei Kanälen.",
        );
    }
    if let Some(vod) = output.vod_audio_track {
        let vod = source
            .audio
            .iter()
            .find(|audio| audio.wire_track == vod)
            .ok_or("Die für Twitch gewählte VOD-Tonspur fehlt.")?;
        if vod.codec != "aac" || vod.sample_rate != 48000 || vod.channels != audio.channels {
            return Err("Live- und VOD-Ton passen nicht zum Twitch-Audiovertrag.");
        }
    }
    Ok(Preferences {
        maximum_aggregate_bitrate: config.maximum_aggregate_bitrate,
        maximum_video_tracks: config.maximum_video_tracks,
        vod_track_audio: output.vod_audio_track.is_some(),
        audio_samples_per_sec: audio.sample_rate,
        audio_channels: u32::from(audio.channels),
        audio_max_buffering_ms: 1000,
        audio_fixed_buffering: false,
        canvases: vec![Canvas {
            width: source.width.min(output.video.width),
            height: source.height.min(output.video.height),
            canvas_width: source.width,
            canvas_height: source.height,
            framerate: Rational {
                numerator: source.fps_numerator,
                denominator: source.fps_denominator,
            },
        }],
    })
}

pub fn twitch(
    config: TwitchConfiguration,
    live: u8,
    vod: Option<u8>,
) -> Result<ProgramOutput, &'static str> {
    if config.video.is_empty()
        || config.video.len() != config.encoders.len()
        || config
            .encoders
            .iter()
            .any(|encoder| *encoder != GoLiveEncoder::ObsX264)
    {
        return Err("Der von Twitch geforderte Encoder ist auf diesem Server nicht verfügbar.");
    }
    let mut video = Vec::new();
    for (item, encoder) in config.video.into_iter().zip(config.encoders) {
        if item.codec != encoder.codec() {
            return Err("Twitch-Encoder und Codec widersprechen sich.");
        }
        if item.canvas_index != 0 {
            return Err("Hochkant bleibt aus, bis die Bildgestaltung gewählt und freigegeben ist.");
        }
        let fps = FrameRate::new(item.framerate.numerator, item.framerate.denominator)
            .map_err(|_| "Twitch-Bildrate ist ungültig.")?;
        video.push(ProgramVideo {
            wire_track: item.wire_track,
            canvas_index: 0,
            profile: profile(
                item.width,
                item.height,
                fps,
                item.codec,
                item.bitrate_kbps,
                item.keyframe_seconds,
                item.profile,
            )?,
            layout: None,
        });
    }
    let audio = config
        .audio
        .into_iter()
        .map(|item| {
            Ok(ProgramAudio {
                source_wire_track: match item.role {
                    AudioRole::Live => live,
                    AudioRole::Vod => vod.ok_or("Die von Twitch geforderte VOD-Tonspur fehlt.")?,
                },
                destination_wire_track: item.wire_track,
                encoding: Some(AudioEncoding {
                    channels: item.channels,
                    bitrate_kbps: item.bitrate_kbps,
                }),
            })
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    Ok(ProgramOutput {
        target: config.target,
        video,
        audio,
    })
}

pub fn capacity_key(source: &SourceObservation, output: &ProgramOutput) -> String {
    let mut key = format!(
        "{}:{}x{}@{}/{}>{}",
        source.codec,
        source.width,
        source.height,
        source.fps_numerator,
        source.fps_denominator,
        output.target.id
    );
    for video in &output.video {
        let p = &video.profile;
        key.push_str(&format!(
            "|v{}c{}:{:?}:{}x{}@{}/{}:{}:{}:{}",
            video.wire_track,
            video.canvas_index,
            p.codec,
            p.width,
            p.height,
            p.fps.numerator(),
            p.fps.denominator(),
            p.rate.target_kbps,
            p.gop.keyframe_interval_frames,
            p.codec_profile
        ));
        if let Some(layout) = &video.layout {
            key.push_str(&format!(
                ":layout{}r{}",
                layout.revision.id, layout.revision.revision
            ));
        }
    }
    for audio in &output.audio {
        key.push_str(&format!(
            "|a{}>{}:{}",
            audio.source_wire_track,
            audio.destination_wire_track,
            audio.encoding.map_or_else(
                || "copy".into(),
                |e| format!("aac:{}:{}", e.channels, e.bitrate_kbps)
            )
        ));
    }
    key
}
