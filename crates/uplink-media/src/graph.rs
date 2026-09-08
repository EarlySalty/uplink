use crate::{
    Composition, Crop, DesiredOutput, LayoutSpec, MediaError, Result, SessionSpec,
    SourceObservation,
};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
};
use uplink_core::{
    AudioAction, Chroma, Codec, ColorPrimaries, ColorRange, Matrix, OutputStatus, RateMode,
    Transfer, VideoAction, VideoProfile,
};
use uplink_ingest::{MediaKind, WireCodec, WireTrack};

pub(crate) struct EncodeProfile {
    pub video: VideoProfile,
    pub layout: Option<LayoutSpec>,
    /// Fehlende Farbmetadaten der Quelle bleiben unbekannt und werden nicht umetikettiert.
    pub signal_bt709: bool,
}
pub(crate) struct Routing {
    pub group: Option<usize>,
    pub audio: Vec<(u8, u8)>,
}
pub(crate) struct Graph {
    pub profiles: Vec<EncodeProfile>,
    pub routes: Vec<Routing>,
    pub input_audio: HashMap<WireTrack, u8>,
    pub expected_tracks: HashSet<WireTrack>,
    pub video_track: WireTrack,
    pub codecs: HashMap<WireTrack, WireCodec>,
}

impl Graph {
    pub(crate) fn build(spec: &SessionSpec) -> Result<Self> {
        let source = spec.input.source.as_ref().ok_or(MediaError::MissingTrack)?;
        let plan = uplink_core::plan(&spec.input).map_err(|_| MediaError::InvalidPlan)?;
        if plan.outputs.is_empty()
            || plan
                .outputs
                .iter()
                .any(|out| out.status != OutputStatus::Planned)
            || spec.routes.len() != plan.outputs.len()
        {
            return Err(MediaError::InvalidPlan);
        }
        let mut ids = HashMap::new();
        let mut wires = HashSet::new();
        for binding in &spec.tracks {
            if ids.insert(binding.logical_id, binding.wire).is_some() || !wires.insert(binding.wire)
            {
                return Err(MediaError::InvalidConfiguration);
            }
        }
        let video_track = *ids
            .get(&source.video_track)
            .ok_or(MediaError::MissingTrack)?;
        if video_track.kind != MediaKind::Video || video_track != spec.identity.track {
            return Err(MediaError::InvalidConfiguration);
        }
        let mut input_audio = HashMap::new();
        let mut codecs = HashMap::from([(
            video_track,
            match source.video.codec {
                Codec::Av1 => WireCodec::Av1,
                Codec::H264 => WireCodec::H264,
                Codec::Hevc => return Err(MediaError::UnsupportedProfile),
            },
        )]);
        let mut logical_audio = HashMap::new();
        for (index, track) in source.audio.iter().enumerate() {
            let wire = *ids.get(&track.track_id).ok_or(MediaError::MissingTrack)?;
            if wire.kind != MediaKind::Audio || index > 15 {
                return Err(MediaError::InvalidConfiguration);
            }
            input_audio.insert(wire, index as u8);
            codecs.insert(wire, WireCodec::Aac);
            logical_audio.insert(track.track_id, index as u8);
        }
        if ids.len() != source.audio.len() + 1 {
            return Err(MediaError::InvalidConfiguration);
        }
        let mut profiles = Vec::new();
        let mut output_groups = HashMap::new();
        for (index, group) in plan.encode_groups.iter().enumerate() {
            let request = spec
                .input
                .outputs
                .iter()
                .find(|out| out.id == group.outputs[0])
                .ok_or(MediaError::InvalidPlan)?;
            validate_encoder(&request.video)?;
            if source.video.color != request.video.color
                || source.video.bit_depth != request.video.bit_depth
            {
                return Err(MediaError::UnsupportedProfile);
            }
            let layout = match &request.layout {
                Some(revision) => Some(
                    spec.layouts
                        .iter()
                        .find(|layout| &layout.revision == revision)
                        .ok_or(MediaError::UnsupportedProfile)?
                        .clone(),
                ),
                None => None,
            };
            if let Some(layout) = &layout {
                validate_layout(layout, &source.video, &request.video)?;
            }
            profiles.push(EncodeProfile {
                video: request.video.clone(),
                layout,
                signal_bt709: true,
            });
            for id in &group.outputs {
                output_groups.insert(id.as_str(), index);
            }
        }
        let mut route_ids = HashSet::new();
        let mut routes = Vec::new();
        for route in &spec.routes {
            if !route_ids.insert(route.output_id.as_str()) {
                return Err(MediaError::InvalidPlan);
            }
            let output = plan
                .outputs
                .iter()
                .find(|out| out.id == route.output_id)
                .ok_or(MediaError::InvalidPlan)?;
            let group = match output.video {
                Some(VideoAction::Copy) => None,
                Some(VideoAction::Encode) => Some(
                    *output_groups
                        .get(output.id.as_str())
                        .ok_or(MediaError::InvalidPlan)?,
                ),
                None => return Err(MediaError::InvalidPlan),
            };
            let mut audio = Vec::new();
            for (destination, planned) in std::iter::once(output.live_audio.as_ref())
                .chain(std::iter::once(output.vod_audio.as_ref()))
                .enumerate()
            {
                if let Some(planned) = planned {
                    // Die erste echte Kette kopiert kompatibles AAC. Einen nötigen
                    // Audioencode darf sie ausdrücklich nicht still ersetzen.
                    if planned.action != AudioAction::Copy {
                        return Err(MediaError::UnsupportedProfile);
                    }
                    audio.push((
                        *logical_audio
                            .get(&planned.source_track)
                            .ok_or(MediaError::MissingTrack)?,
                        destination as u8,
                    ));
                }
            }
            routes.push(Routing { group, audio });
        }
        Ok(Self {
            profiles,
            routes,
            input_audio,
            expected_tracks: wires,
            video_track,
            codecs,
        })
    }

    pub(crate) fn observed(source: &SourceObservation, outputs: &[DesiredOutput]) -> Result<Self> {
        use uplink_core::{Color, FrameRate, Gop, RateControl};
        if outputs.is_empty()
            || source.pixel_format != "yuv420p"
            || !matches!(source.codec.as_str(), "av1" | "h264")
            || source
                .color_transfer
                .as_deref()
                .is_some_and(|v| !matches!(v, "bt709" | "smpte170m"))
            || source
                .color_primaries
                .as_deref()
                .is_some_and(|v| v != "bt709")
            || source
                .color_matrix
                .as_deref()
                .is_some_and(|v| !matches!(v, "bt709" | "smpte170m"))
        {
            return Err(MediaError::UnsupportedProfile);
        }
        let source_fps = FrameRate::new(source.fps_numerator, source.fps_denominator)
            .map_err(|_| MediaError::InvalidMedia)?;
        let video_track = WireTrack {
            kind: MediaKind::Video,
            wire_id: source.video_wire_track,
        };
        let mut expected_tracks = HashSet::from([video_track]);
        let mut codecs = HashMap::from([(
            video_track,
            if source.codec == "av1" {
                WireCodec::Av1
            } else {
                WireCodec::H264
            },
        )]);
        let mut input_audio = HashMap::new();
        for (index, audio) in source.audio.iter().enumerate() {
            if audio.codec != "aac"
                || audio.sample_rate != 48000
                || !(1..=2).contains(&audio.channels)
                || index >= 16
            {
                return Err(MediaError::UnsupportedProfile);
            }
            let wire = WireTrack {
                kind: MediaKind::Audio,
                wire_id: audio.wire_track,
            };
            expected_tracks.insert(wire);
            input_audio.insert(wire, index as u8);
            codecs.insert(wire, WireCodec::Aac);
        }
        let mut profiles: Vec<EncodeProfile> = Vec::new();
        let mut routes = Vec::new();
        let mut ids = HashSet::new();
        for output in outputs {
            let desired = &output.video;
            if !ids.insert(output.target.id.as_str())
                || output.target.id.is_empty()
                || desired.codec != Codec::H264
                || desired.width < 2
                || desired.height < 2
                || desired.width > 4096
                || desired.height > 4096
                || u64::from(desired.width) * u64::from(desired.height)
                    > u64::from(source.width) * u64::from(source.height)
                || desired.fps > source_fps
                || desired.fps
                    > FrameRate::new(60, 1).map_err(|_| MediaError::InvalidConfiguration)?
                || desired.bitrate_kbps == 0
                || desired.bitrate_kbps > 20000
            {
                return Err(MediaError::UnsupportedProfile);
            }
            let video = VideoProfile {
                width: desired.width,
                height: desired.height,
                fps: desired.fps,
                codec: Codec::H264,
                codec_profile: "high".into(),
                level: if u64::from(desired.width) * u64::from(desired.height) <= 1920 * 1080 {
                    "4.2"
                } else {
                    "5.2"
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
                    target_kbps: desired.bitrate_kbps,
                    max_kbps: desired.bitrate_kbps,
                    buffer_kbits: desired.bitrate_kbps * 2,
                },
                gop: Gop {
                    keyframe_interval_frames: u32::try_from(
                        (u64::from(desired.fps.numerator()) * 2)
                            .div_ceil(u64::from(desired.fps.denominator())),
                    )
                    .map_err(|_| MediaError::InvalidConfiguration)?,
                    closed: true,
                },
            };
            validate_encoder(&video)?;
            if let Some(layout) = &output.layout {
                validate_layout_dimensions(layout, source.width, source.height, &video)?;
            }
            let group = match profiles
                .iter()
                .position(|p| p.video == video && p.layout == output.layout)
            {
                Some(index) => index,
                None => {
                    profiles.push(EncodeProfile {
                        video,
                        layout: output.layout.clone(),
                        signal_bt709: false,
                    });
                    profiles.len() - 1
                }
            };
            let mut audio = Vec::new();
            for (destination, wire_id) in [Some(output.live_audio_track), output.vod_audio_track]
                .into_iter()
                .enumerate()
            {
                if let Some(wire_id) = wire_id {
                    let wire = WireTrack {
                        kind: MediaKind::Audio,
                        wire_id,
                    };
                    audio.push((
                        *input_audio.get(&wire).ok_or(MediaError::MissingTrack)?,
                        destination as u8,
                    ));
                }
            }
            routes.push(Routing {
                group: Some(group),
                audio,
            });
        }
        Ok(Self {
            profiles,
            routes,
            input_audio,
            expected_tracks,
            video_track,
            codecs,
        })
    }

    pub(crate) fn arguments(
        &self,
        paths: &[PathBuf],
        threads: usize,
        video_origin_pts_ms: i64,
    ) -> Result<Vec<OsString>> {
        if paths.len() != self.profiles.len() || paths.is_empty() {
            return Err(MediaError::InvalidConfiguration);
        }
        let mut args: Vec<OsString> = [
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "quiet",
            "-copyts",
            "-max_alloc",
            "67108864",
            "-filter_complex_threads",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        args.push(threads.to_string().into());
        args.extend([
            "-threads".into(),
            threads.to_string().into(),
            "-max_pixels".into(),
            "9000000".into(),
            "-f".into(),
            "flv".into(),
            "-i".into(),
            "pipe:0".into(),
        ]);
        let mut filter = if self.profiles.len() == 1 {
            "[0:v:0]null[src0];".to_owned()
        } else {
            format!(
                "[0:v:0]split={}{};",
                self.profiles.len(),
                (0..self.profiles.len())
                    .map(|index| format!("[src{index}]"))
                    .collect::<String>()
            )
        };
        for (index, profile) in self.profiles.iter().enumerate() {
            filter.push_str(&profile.filter(index, video_origin_pts_ms));
            filter.push(';');
        }
        filter.pop();
        args.extend(["-filter_complex".into(), filter.into()]);
        for (index, (profile, path)) in self.profiles.iter().zip(paths).enumerate() {
            args.extend([
                "-map".into(),
                format!("[out{index}]").into(),
                "-map".into(),
                "0:a".into(),
                "-c:a".into(),
                "copy".into(),
            ]);
            profile.encoder_args(&mut args, threads);
            args.extend([
                "-f".into(),
                "flv".into(),
                "-flvflags".into(),
                "no_duration_filesize".into(),
                "-flush_packets".into(),
                "1".into(),
                "-avoid_negative_ts".into(),
                "disabled".into(),
                format!("unix://{}", path.display()).into(),
            ]);
        }
        Ok(args)
    }
}

fn validate_encoder(video: &VideoProfile) -> Result<()> {
    if video.bit_depth != 8
        || video.chroma != Chroma::Yuv420
        || !video.width.is_multiple_of(2)
        || !video.height.is_multiple_of(2)
        || video.rate.mode != RateMode::Cbr
        || video.color.primaries != ColorPrimaries::Bt709
        || video.color.transfer != Transfer::Bt709
        || video.color.matrix != Matrix::Bt709
        || video.color.range != ColorRange::Limited
    {
        return Err(MediaError::UnsupportedProfile);
    }
    let profile_valid = match video.codec {
        Codec::H264 => ["baseline", "main", "high"].contains(&video.codec_profile.as_str()),
        Codec::Av1 | Codec::Hevc => video.codec_profile == "main",
    };
    if !profile_valid
        || video
            .level
            .parse::<f32>()
            .ok()
            .is_none_or(|level| !level.is_finite() || !(1.0..=6.3).contains(&level))
    {
        return Err(MediaError::UnsupportedProfile);
    }
    Ok(())
}
fn validate_crop(crop: Crop, source: &VideoProfile) -> Result<()> {
    if crop.width < 2
        || crop.height < 2
        || crop
            .x
            .checked_add(crop.width)
            .is_none_or(|end| end > source.width)
        || crop
            .y
            .checked_add(crop.height)
            .is_none_or(|end| end > source.height)
    {
        return Err(MediaError::InvalidConfiguration);
    }
    Ok(())
}
fn validate_layout(
    layout: &LayoutSpec,
    source: &VideoProfile,
    output: &VideoProfile,
) -> Result<()> {
    validate_layout_dimensions(layout, source.width, source.height, output)
}
fn validate_layout_dimensions(
    layout: &LayoutSpec,
    width: u32,
    height: u32,
    output: &VideoProfile,
) -> Result<()> {
    let check = |crop: Crop| {
        if crop.width < 2
            || crop.height < 2
            || crop.x.checked_add(crop.width).is_none_or(|end| end > width)
            || crop
                .y
                .checked_add(crop.height)
                .is_none_or(|end| end > height)
        {
            Err(MediaError::InvalidConfiguration)
        } else {
            Ok(())
        }
    };
    match layout.composition {
        Composition::Crop(crop) => check(crop),
        Composition::Stacked {
            gameplay,
            camera,
            camera_height,
        } => {
            check(gameplay)?;
            check(camera)?;
            if camera_height < 2 || camera_height >= output.height - 2 || camera_height % 2 != 0 {
                return Err(MediaError::InvalidConfiguration);
            }
            Ok(())
        }
        Composition::PictureInPicture {
            gameplay,
            camera,
            camera_box,
        } => {
            check(gameplay)?;
            check(camera)?;
            validate_crop(camera_box, output)
        }
    }
}
fn crop(crop: Crop) -> String {
    format!("crop={}:{}:{}:{}", crop.width, crop.height, crop.x, crop.y)
}
fn fit(width: u32, height: u32) -> String {
    format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease:flags=lanczos,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
    )
}
impl EncodeProfile {
    fn filter(&self, index: usize, video_origin_pts_ms: i64) -> String {
        let video = &self.video;
        let tail = format!(
            "setpts=PTS-STARTPTS,fps={}/{},settb=1/1000,setpts=PTS+{video_origin_pts_ms},format=yuv420p[out{index}]",
            video.fps.numerator(),
            video.fps.denominator()
        );
        match self.layout.as_ref().map(|layout| &layout.composition) {
            None => format!("[src{index}]{},{tail}", fit(video.width, video.height)),
            Some(Composition::Crop(area)) => format!(
                "[src{index}]{},{},{tail}",
                crop(*area),
                fit(video.width, video.height)
            ),
            Some(Composition::Stacked {
                gameplay,
                camera,
                camera_height,
            }) => format!(
                "[src{index}]split=2[g{index}][c{index}];[g{index}]{},{}[gf{index}];[c{index}]{},{}[cf{index}];[cf{index}][gf{index}]vstack=inputs=2,{tail}",
                crop(*gameplay),
                fit(video.width, video.height - camera_height),
                crop(*camera),
                fit(video.width, *camera_height)
            ),
            Some(Composition::PictureInPicture {
                gameplay,
                camera,
                camera_box,
            }) => format!(
                "[src{index}]split=2[g{index}][c{index}];[g{index}]{},{}[gf{index}];[c{index}]{},{}[cf{index}];[gf{index}][cf{index}]overlay={}:{},{tail}",
                crop(*gameplay),
                fit(video.width, video.height),
                crop(*camera),
                fit(camera_box.width, camera_box.height),
                camera_box.x,
                camera_box.y
            ),
        }
    }
    fn encoder_args(&self, args: &mut Vec<OsString>, threads: usize) {
        let video = &self.video;
        let codec_args: &[&str] = match video.codec {
            Codec::H264 => &[
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-tune",
                "zerolatency",
                "-x264-params",
                "nal-hrd=cbr:force-cfr=1",
            ],
            Codec::Hevc => &[
                "-c:v",
                "libx265",
                "-preset",
                "fast",
                "-x265-params",
                "log-level=error:strict-cbr=1",
            ],
            Codec::Av1 => &[
                "-c:v",
                "libaom-av1",
                "-usage",
                "realtime",
                "-cpu-used",
                "8",
                "-lag-in-frames",
                "0",
            ],
        };
        args.extend(codec_args.iter().map(OsString::from));
        for (name, value) in [
            ("-threads:v", threads.to_string()),
            ("-b:v", format!("{}k", video.rate.target_kbps)),
            ("-minrate:v", format!("{}k", video.rate.target_kbps)),
            ("-maxrate:v", format!("{}k", video.rate.max_kbps)),
            ("-bufsize:v", format!("{}k", video.rate.buffer_kbits)),
            ("-g", video.gop.keyframe_interval_frames.to_string()),
            (
                "-keyint_min",
                video.gop.keyframe_interval_frames.to_string(),
            ),
            ("-sc_threshold", "0".into()),
            ("-bf", "0".into()),
            ("-enc_time_base:v", "1:1000".into()),
            ("-fps_mode:v", "passthrough".into()),
        ] {
            args.push(name.into());
            args.push(value.into());
        }
        if self.signal_bt709 {
            args.extend(
                [
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-color_range",
                    "tv",
                ]
                .into_iter()
                .map(Into::into),
            );
        }
        if video.codec != Codec::Av1 {
            args.extend([
                "-profile:v".into(),
                video.codec_profile.clone().into(),
                "-level:v".into(),
                video.level.clone().into(),
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioObservation, DesiredVideo, PublishSecret, PublishTarget};
    use uplink_core::FrameRate;

    fn source() -> SourceObservation {
        SourceObservation {
            video_wire_track: 0,
            codec: "h264".into(),
            width: 320,
            height: 180,
            fps_numerator: 25,
            fps_denominator: 1,
            pixel_format: "yuv420p".into(),
            color_primaries: None,
            color_transfer: None,
            color_matrix: None,
            color_range: None,
            rate_control: None,
            gop_frames: None,
            audio: vec![0, 1]
                .into_iter()
                .map(|wire_track| AudioObservation {
                    wire_track,
                    codec: "aac".into(),
                    sample_rate: 48000,
                    channels: 1,
                })
                .collect(),
            sampled_events: 150,
            sampled_bytes: 50000,
            sampled_duration_ms: 1000,
        }
    }
    fn output(id: &str, live: u8, vod: Option<u8>) -> DesiredOutput {
        DesiredOutput {
            target: PublishTarget {
                id: id.into(),
                endpoint: "rtmps://localhost/live".into(),
                playpath: PublishSecret::new(b"synthetic".to_vec()).unwrap(),
                tls: None,
                allowed_hosts: vec!["localhost".into()],
                allow_loopback: true,
                allow_unencrypted: false,
            },
            video: DesiredVideo {
                width: 320,
                height: 180,
                fps: FrameRate::new(25, 1).unwrap(),
                bitrate_kbps: 384,
                codec: Codec::H264,
            },
            live_audio_track: live,
            vod_audio_track: vod,
            layout: None,
        }
    }
    #[test]
    fn observed_source_never_invents_copy_or_cbr_and_audio_does_not_duplicate_encode() {
        let graph = Graph::observed(
            &source(),
            &[output("one", 0, Some(1)), output("two", 1, None)],
        )
        .unwrap();
        assert_eq!(graph.profiles.len(), 1);
        assert_eq!(graph.routes[0].group, Some(0));
        assert_eq!(graph.routes[1].group, Some(0));
        assert_eq!(graph.routes[0].audio, vec![(0, 0), (1, 1)]);
        assert_eq!(graph.routes[1].audio, vec![(1, 0)]);
        assert!(!graph.profiles[0].signal_bt709);
        let args = graph
            .arguments(&[PathBuf::from("/private/example.sock")], 1, 137)
            .unwrap();
        assert!(!args.iter().any(|v| v == "-color_primaries"));
    }
    #[test]
    fn observed_hdr_missing_audio_and_unapproved_upscale_are_rejected() {
        let mut hdr = source();
        hdr.color_transfer = Some("smpte2084".into());
        assert!(matches!(
            Graph::observed(&hdr, &[output("one", 0, None)]),
            Err(MediaError::UnsupportedProfile)
        ));
        assert!(matches!(
            Graph::observed(&source(), &[output("one", 0, Some(7))]),
            Err(MediaError::MissingTrack)
        ));
        let mut upscale = output("one", 0, None);
        upscale.video.width = 640;
        assert!(matches!(
            Graph::observed(&source(), &[upscale]),
            Err(MediaError::UnsupportedProfile)
        ));
    }
    #[test]
    fn large_reduced_frame_rate_components_do_not_overflow_gop() {
        let mut desired = output("one", 0, None);
        desired.video.fps = FrameRate::new(4_000_000_001, 1_000_000_000).unwrap();
        let graph = Graph::observed(&source(), &[desired]).unwrap();
        assert_eq!(graph.profiles[0].video.gop.keyframe_interval_frames, 9);
    }
}
