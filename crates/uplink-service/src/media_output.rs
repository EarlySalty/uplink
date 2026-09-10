use uplink_core::{
    Chroma, Codec, Color, ColorPrimaries, ColorRange, FrameRate, Gop, LayoutRevision, Matrix,
    RateControl, RateMode, Transfer, VideoProfile,
};
use uplink_media::platform::twitch::{
    AudioRole, Canvas, GoLiveEncoder, Preferences, Rational, TwitchConfiguration,
};
use uplink_media::portrait::compile_portrait;
use uplink_media::{
    AudioEncoding, Composition, DesiredOutput, ProgramAudio, ProgramOutput, ProgramVideo,
    SourceObservation,
};

pub struct HochkantWahl {
    pub ziel: (u32, u32),
    pub composition: Composition,
    pub revision: LayoutRevision,
}

fn portrait_fehlertext(error: uplink_media::portrait::PortraitError) -> &'static str {
    use uplink_media::portrait::PortraitError;
    match error {
        PortraitError::InvalidDimensions => {
            "Die Quell- oder Zielabmessung für Hochkant hat keine Fläche."
        }
        PortraitError::NotPortrait => "Die Hochkantausgabe hat kein Hochkantformat.",
        PortraitError::GameplayCropOutside => "Der Gameplay-Ausschnitt liegt außerhalb der Quelle.",
        PortraitError::GameplayCropGeometry => {
            "Der Gameplay-Ausschnitt passt nicht in ein sauberes YUV420-Bild."
        }
        PortraitError::CameraCropOutside => "Der Kamera-Ausschnitt liegt außerhalb der Quelle.",
        PortraitError::CameraCropGeometry => {
            "Der Kamera-Ausschnitt passt nicht in ein sauberes YUV420-Bild."
        }
        PortraitError::CameraBoxOutside => "Die Kamerabox liegt außerhalb des Hochkantbilds.",
        PortraitError::CameraBoxGeometry => {
            "Die Kamerabox passt nicht in ein sauberes YUV420-Bild."
        }
        PortraitError::CameraHeightInvalid => {
            "Die Kamerahöhe lässt keinen gültigen Platz im Hochkantbild."
        }
    }
}

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

pub fn preferences(
    source: &SourceObservation,
    output: &DesiredOutput,
    config: &crate::config::EnhancedConfig,
    hochkant_ziel: Option<(u32, u32)>,
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
    let framerate = Rational {
        numerator: source.fps_numerator,
        denominator: source.fps_denominator,
    };
    let mut canvases = vec![Canvas {
        width: source.width,
        height: source.height,
        canvas_width: source.width,
        canvas_height: source.height,
        framerate,
    }];
    if let Some((breite, hoehe)) = hochkant_ziel {
        canvases.push(Canvas {
            width: breite,
            height: hoehe,
            canvas_width: breite,
            canvas_height: hoehe,
            framerate,
        });
    }
    Ok(Preferences {
        maximum_aggregate_bitrate: config.maximum_aggregate_bitrate,
        maximum_video_tracks: config.maximum_video_tracks,
        vod_track_audio: output.vod_audio_track.is_some(),
        audio_samples_per_sec: audio.sample_rate,
        audio_channels: u32::from(audio.channels),
        audio_max_buffering_ms: 1000,
        audio_fixed_buffering: false,
        canvases,
    })
}

pub fn twitch(
    config: TwitchConfiguration,
    live: u8,
    vod: Option<u8>,
    hochkant: Option<&HochkantWahl>,
    source: &SourceObservation,
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
        if item.canvas_index > 1 {
            return Err(
                "Twitch hat eine Canvas-Ausgabe gemeldet, die dieser Server nicht unterstützt.",
            );
        }
        if item.canvas_index == 1 && hochkant.is_none() {
            return Err("Hochkant bleibt aus, bis die Bildgestaltung gewählt und freigegeben ist.");
        }
        let fps = FrameRate::new(item.framerate.numerator, item.framerate.denominator)
            .map_err(|_| "Twitch-Bildrate ist ungültig.")?;
        let profile = profile(
            item.width,
            item.height,
            fps,
            item.codec,
            item.bitrate_kbps,
            item.keyframe_seconds,
            item.profile,
        )?;
        let layout = if item.canvas_index == 1 {
            let wahl = hochkant.expect("Canvas 1 nur mit Hochkantwahl");
            if (item.width, item.height) != wahl.ziel {
                return Err(
                    "Twitch hat für die gewählte Hochkantfassung zusätzlich Stufen in einer anderen Größe angeboten; Bildgestaltung und Zielgröße passen nicht zusammen.",
                );
            }
            Some(
                compile_portrait(
                    source.width,
                    source.height,
                    &profile,
                    wahl.revision.clone(),
                    wahl.composition.clone(),
                )
                .map_err(portrait_fehlertext)?,
            )
        } else {
            None
        };
        video.push(ProgramVideo {
            wire_track: item.wire_track,
            canvas_index: item.canvas_index as u8,
            profile,
            layout,
        });
    }
    if hochkant.is_some() && !video.iter().any(|video| video.canvas_index == 1) {
        return Err("Twitch hat für die gewählte Hochkantfassung keine Ausgabe angeboten.");
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

#[cfg(test)]
mod tests {
    use super::*;
    use uplink_core::Codec;
    use uplink_media::platform::twitch::{AudioConfiguration, VideoConfiguration};
    use uplink_media::{AudioObservation, Crop, DesiredVideo, PublishSecret, PublishTarget};

    fn quelle() -> SourceObservation {
        SourceObservation {
            video_wire_track: 0,
            codec: "av1".into(),
            width: 1920,
            height: 1080,
            fps_numerator: 60,
            fps_denominator: 1,
            pixel_format: "yuv420p".into(),
            color_primaries: None,
            color_transfer: None,
            color_matrix: None,
            color_range: None,
            rate_control: None,
            gop_frames: None,
            audio: vec![AudioObservation {
                wire_track: 0,
                codec: "aac".into(),
                sample_rate: 48000,
                channels: 2,
            }],
            sampled_events: 1,
            sampled_bytes: 1,
            sampled_duration_ms: 1,
        }
    }

    fn stufe(wire_track: u8, canvas: usize, breite: u32, hoehe: u32) -> VideoConfiguration {
        VideoConfiguration {
            wire_track,
            canvas_index: canvas,
            width: breite,
            height: hoehe,
            framerate: Rational {
                numerator: 60,
                denominator: 1,
            },
            codec: Codec::H264,
            bitrate_kbps: 4500,
            keyframe_seconds: 2,
            profile: "high".into(),
        }
    }

    fn konfiguration(stufen: Vec<VideoConfiguration>) -> TwitchConfiguration {
        TwitchConfiguration {
            target: PublishTarget {
                id: "twitch".into(),
                endpoint: "rtmps://ingest.example/app".into(),
                playpath: PublishSecret::new(b"synthetic-key".to_vec()).unwrap(),
                tls: None,
                allowed_hosts: vec!["ingest.example".into()],
                allow_loopback: false,
                allow_unencrypted: false,
            },
            audio: vec![AudioConfiguration {
                wire_track: 0,
                role: AudioRole::Live,
                channels: 2,
                bitrate_kbps: 160,
            }],
            encoders: stufen.iter().map(|_| GoLiveEncoder::ObsX264).collect(),
            video: stufen,
        }
    }

    fn hochkant_wahl() -> HochkantWahl {
        HochkantWahl {
            ziel: (1080, 1920),
            composition: Composition::Crop(Crop {
                x: 96,
                y: 54,
                width: 1728,
                height: 972,
            }),
            revision: uplink_core::LayoutRevision { id: 7, revision: 3 },
        }
    }

    #[test]
    fn hochkantstufe_in_fremder_groesse_wird_offen_abgewiesen() {
        let fehler = match twitch(
            konfiguration(vec![stufe(0, 0, 1280, 720), stufe(1, 1, 720, 1280)]),
            0,
            None,
            Some(&hochkant_wahl()),
            &quelle(),
        ) {
            Err(fehler) => fehler,
            Ok(_) => panic!("Eine Hochkantstufe in fremder Größe wird abgewiesen"),
        };
        assert!(fehler.contains("Zielgröße"));
    }

    #[test]
    fn querformat_ohne_wahl_bleibt_ohne_layout() {
        let programm = twitch(
            konfiguration(vec![stufe(0, 0, 1280, 720)]),
            0,
            None,
            None,
            &quelle(),
        )
        .unwrap();
        assert_eq!(programm.video.len(), 1);
        assert_eq!(programm.video[0].canvas_index, 0);
        assert!(programm.video[0].layout.is_none());
        assert_eq!(
            programm.audio[0].encoding,
            Some(AudioEncoding {
                channels: 2,
                bitrate_kbps: 160
            })
        );
    }

    #[test]
    fn canvas_ein_ohne_wahl_wird_sichtbar_abgewiesen() {
        let fehler = match twitch(
            konfiguration(vec![stufe(0, 0, 1280, 720), stufe(1, 1, 1080, 1920)]),
            0,
            None,
            None,
            &quelle(),
        ) {
            Err(fehler) => fehler,
            Ok(_) => panic!("Canvas 1 ohne Wahl wird abgewiesen"),
        };
        assert!(fehler.contains("Hochkant"));
    }

    #[test]
    fn canvas_ein_mit_wahl_traegt_versioniertes_layout() {
        let programm = twitch(
            konfiguration(vec![stufe(0, 0, 1280, 720), stufe(1, 1, 1080, 1920)]),
            0,
            None,
            Some(&hochkant_wahl()),
            &quelle(),
        )
        .unwrap();
        let quer = programm
            .video
            .iter()
            .find(|video| video.canvas_index == 0)
            .unwrap();
        let hoch = programm
            .video
            .iter()
            .find(|video| video.canvas_index == 1)
            .unwrap();
        assert!(quer.layout.is_none());
        let layout = hoch.layout.as_ref().unwrap();
        assert_eq!(layout.revision.id, 7);
        assert_eq!(layout.revision.revision, 3);
        assert_eq!(hoch.profile.width, 1080);
        assert_eq!(hoch.profile.height, 1920);
        assert_eq!(hoch.profile.rate.target_kbps, 4500);
        assert_eq!(hoch.profile.gop.keyframe_interval_frames, 120);
    }

    #[test]
    fn fremde_canvas_wird_abgewiesen() {
        let fehler = match twitch(
            konfiguration(vec![stufe(0, 0, 1280, 720), stufe(1, 2, 720, 1280)]),
            0,
            None,
            Some(&hochkant_wahl()),
            &quelle(),
        ) {
            Err(fehler) => fehler,
            Ok(_) => panic!("Eine fremde Canvas-Ausgabe wird abgewiesen"),
        };
        assert!(fehler.contains("Canvas"));
    }

    #[test]
    fn angefragter_hochkant_ohne_angebot_wird_nicht_still_uebergangen() {
        assert!(
            twitch(
                konfiguration(vec![stufe(0, 0, 1280, 720)]),
                0,
                None,
                Some(&hochkant_wahl()),
                &quelle(),
            )
            .is_err()
        );
    }

    #[test]
    fn enhanced_fordert_gemessene_quelle_statt_gespeichertem_einzelziel_an() {
        let mut source = quelle();
        source.width = 2560;
        source.height = 1440;
        let request = preferences(
            &source,
            &test_wunsch(),
            &crate::config::EnhancedConfig::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            (request.canvases[0].width, request.canvases[0].height),
            (2560, 1440)
        );
        assert_eq!(
            (
                request.canvases[0].canvas_width,
                request.canvases[0].canvas_height
            ),
            (2560, 1440)
        );
    }

    #[test]
    fn anfrage_beantragt_canvas_ein_nur_mit_wahl() {
        let config = crate::config::EnhancedConfig::default();
        let ohne = preferences(&quelle(), &test_wunsch(), &config, None).unwrap();
        assert_eq!(ohne.canvases.len(), 1);
        let mit = preferences(&quelle(), &test_wunsch(), &config, Some((1080, 1920))).unwrap();
        assert_eq!(mit.canvases.len(), 2);
        assert_eq!(
            (mit.canvases[1].width, mit.canvases[1].height),
            (1080, 1920)
        );
    }

    fn test_wunsch() -> DesiredOutput {
        DesiredOutput {
            target: PublishTarget {
                id: "twitch".into(),
                endpoint: "rtmps://ingest.example/app".into(),
                playpath: PublishSecret::new(b"synthetic-key".to_vec()).unwrap(),
                tls: None,
                allowed_hosts: vec!["ingest.example".into()],
                allow_loopback: false,
                allow_unencrypted: false,
            },
            video: DesiredVideo {
                width: 1920,
                height: 1080,
                fps: FrameRate::new(60, 1).unwrap(),
                bitrate_kbps: 9000,
                codec: Codec::H264,
            },
            live_audio_track: 0,
            vod_audio_track: None,
            layout: None,
        }
    }
}
