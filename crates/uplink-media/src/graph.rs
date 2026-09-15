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
#[derive(Clone)]
pub(crate) struct Routing {
    /// Audio/Metadaten kommen nur aus dieser einen Gruppe, auch bei mehreren Videos.
    pub group: Option<usize>,
    pub video: Vec<(Option<usize>, u8)>,
    pub audio: Vec<(u8, u8)>,
    pub failure: Option<MediaError>,
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
    pub(crate) fn describe_route(&self, route: &Routing, id: &str) -> crate::OutputGraph {
        crate::OutputGraph {
            id: id.to_owned(),
            profile_origin: "running_graph",
            video: route
                .video
                .iter()
                .map(|(group, wire)| crate::VideoProcessing {
                    wire_track: *wire,
                    mode: if group.is_some() { "encode" } else { "copy" },
                    encode_group: *group,
                    profile: group.and_then(|group| self.profiles.get(group)).map(|p| {
                        crate::ProcessingProfile {
                            width: p.video.width,
                            height: p.video.height,
                            fps_numerator: p.video.fps.numerator(),
                            fps_denominator: p.video.fps.denominator(),
                            codec: match p.video.codec {
                                Codec::H264 => "h264",
                                Codec::Av1 => "av1",
                                Codec::Hevc => "hevc",
                            },
                            target_bitrate_kbps: p.video.rate.target_kbps,
                            keyframe_interval_frames: p.video.gop.keyframe_interval_frames,
                        }
                    }),
                })
                .collect(),
            audio: route
                .audio
                .iter()
                .filter_map(|(index, destination)| {
                    self.input_audio
                        .iter()
                        .find(|(_, value)| *value == index)
                        .map(|(wire, _)| crate::AudioProcessing {
                            source_wire_track: wire.wire_id,
                            destination_wire_track: *destination,
                        })
                })
                .collect(),
        }
    }

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
                Codec::Hevc => WireCodec::Hevc,
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
            routes.push(Routing {
                group,
                video: vec![(group, 0)],
                audio,
                failure: None,
            });
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
        let selected = outputs
            .iter()
            .flat_map(|output| {
                let (live, vod) = observed_audio(source, output);
                std::iter::once(live).chain(vod)
            })
            .collect();
        Self::observed_selected(source, outputs, &selected)
    }

    fn observed_selected(
        source: &SourceObservation,
        outputs: &[DesiredOutput],
        selected: &HashSet<u8>,
    ) -> Result<Self> {
        use uplink_core::{Color, FrameRate, Gop, RateControl};
        if source.pixel_format != "yuv420p"
            || !matches!(source.codec.as_str(), "av1" | "h264" | "hevc")
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
            match source.codec.as_str() {
                "av1" => WireCodec::Av1,
                "hevc" => WireCodec::Hevc,
                _ => WireCodec::H264,
            },
        )]);
        let mut input_audio = HashMap::new();
        let selected_audio = source
            .audio
            .iter()
            .filter(|audio| selected.contains(&audio.wire_track));
        for (index, audio) in selected_audio.enumerate() {
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
            if !ids.insert(output.target.id.as_str()) || output.target.id.is_empty() {
                return Err(MediaError::InvalidConfiguration);
            }
            let prior_profiles = profiles.len();
            let route = (|| {
                let mut desired = output.video.clone();
                desired.width = desired.width.min(4096).min(source.width) & !1;
                desired.height = desired.height.min(4096).min(source.height) & !1;
                desired.fps = desired.fps.min(source_fps).min(FrameRate::new(60, 1).map_err(|_| MediaError::InvalidConfiguration)?);
                desired.bitrate_kbps = desired.bitrate_kbps.min(20000);
                if desired.width < 2
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
                    codec: desired.codec,
                    codec_profile: if desired.codec == Codec::H264 {
                        "high"
                    } else {
                        "main"
                    }
                    .into(),
                    level: match desired.codec {
                        Codec::H264
                            if u64::from(desired.width) * u64::from(desired.height)
                                <= 1920 * 1080 =>
                        {
                            "4.2"
                        }
                        Codec::H264 => "5.2",
                        Codec::Hevc | Codec::Av1 => "5.1",
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
                let (live, vod) = observed_audio(source, output);
                for (destination, wire_id) in
                    [Some(live), vod]
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
                Ok(Routing {
                    group: Some(group),
                    video: vec![(Some(group), 0)],
                    audio,
                    failure: None,
                })
            })();
            routes.push(match route {
                Ok(route) => route,
                Err(reason) => {
                    profiles.truncate(prior_profiles);
                    Routing {
                        group: None,
                        video: Vec::new(),
                        audio: Vec::new(),
                        failure: Some(reason),
                    }
                }
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

    pub(crate) fn program(
        source: &SourceObservation,
        outputs: &[crate::ProgramOutput],
    ) -> Result<Self> {
        let selected = outputs
            .iter()
            .flat_map(|output| output.audio.iter().map(|audio| audio.source_wire_track))
            .collect();
        let mut graph = Self::observed_selected(source, &[], &selected)?;
        let source_fps = uplink_core::FrameRate::new(source.fps_numerator, source.fps_denominator)
            .map_err(|_| MediaError::InvalidMedia)?;
        let mut target_ids = HashSet::new();
        for output in outputs {
            if output.target.id.is_empty() || !target_ids.insert(&output.target.id) {
                return Err(MediaError::InvalidConfiguration);
            }
            let prior = graph.profiles.len();
            let compiled = (|| {
                if output.video.is_empty()
                    || output.video.len() > 16
                    || output.audio.is_empty()
                    || output.audio.len() > 16
                {
                    return Err(MediaError::InvalidConfiguration);
                }
                let mut audio = Vec::new();
                let mut audio_ids = HashSet::new();
                for route in &output.audio {
                    if !audio_ids.insert(route.destination_wire_track) {
                        return Err(MediaError::InvalidConfiguration);
                    }
                    let wire = WireTrack {
                        kind: MediaKind::Audio,
                        wire_id: route.source_wire_track,
                    };
                    let source = *graph
                        .input_audio
                        .get(&wire)
                        .ok_or(MediaError::MissingTrack)?;
                    audio.push((source, route.destination_wire_track));
                }
                let mut video = Vec::new();
                let mut video_ids = HashSet::new();
                let mut canvases: HashMap<u8, Option<&LayoutSpec>> = HashMap::new();
                for request in &output.video {
                    let profile = &request.profile;
                    if !video_ids.insert(request.wire_track)
                        || request.canvas_index > 1
                        || (request.canvas_index == 1 && request.layout.is_none())
                        || profile.width < 2
                        || profile.height < 2
                        || profile.width > 4096
                        || profile.height > 4096
                        || u64::from(profile.width) * u64::from(profile.height)
                            > u64::from(source.width) * u64::from(source.height)
                        || profile.fps > source_fps
                        || profile.rate.target_kbps == 0
                        || profile.rate.target_kbps > 20_000
                        || profile.rate.max_kbps != profile.rate.target_kbps
                        || profile.rate.buffer_kbits == 0
                        || profile.rate.buffer_kbits > profile.rate.target_kbps * 4
                        || profile.gop.keyframe_interval_frames == 0
                        || profile.gop.keyframe_interval_frames > 480
                        || !profile.gop.closed
                    {
                        return Err(MediaError::UnsupportedProfile);
                    }
                    if let Some(previous) =
                        canvases.insert(request.canvas_index, request.layout.as_ref())
                        && previous != request.layout.as_ref()
                    {
                        return Err(MediaError::InvalidConfiguration);
                    }
                    validate_encoder(profile)?;
                    // This API accepts a complete requested colour profile. Unlike the
                    // basic desired-size API it cannot silently keep unknown colour tags.
                    if source.color_primaries.as_deref() != Some("bt709")
                        || source.color_transfer.as_deref() != Some("bt709")
                        || source.color_matrix.as_deref() != Some("bt709")
                        || source.color_range.as_deref() != Some("tv")
                    {
                        return Err(MediaError::UnsupportedProfile);
                    }
                    if let Some(layout) = &request.layout {
                        validate_layout_dimensions(layout, source.width, source.height, profile)?;
                    }
                    let group = match graph.profiles.iter().position(|existing| {
                        existing.video == *profile && existing.layout == request.layout
                    }) {
                        Some(index) => index,
                        None => {
                            graph.profiles.push(EncodeProfile {
                                video: profile.clone(),
                                layout: request.layout.clone(),
                                signal_bt709: true,
                            });
                            graph.profiles.len() - 1
                        }
                    };
                    video.push((Some(group), request.wire_track));
                }
                Ok(Routing {
                    group: video[0].0,
                    video,
                    audio,
                    failure: None,
                })
            })();
            graph.routes.push(match compiled {
                Ok(route) => route,
                Err(reason) => {
                    graph.profiles.truncate(prior);
                    Routing {
                        group: None,
                        video: Vec::new(),
                        audio: Vec::new(),
                        failure: Some(reason),
                    }
                }
            });
        }
        Ok(graph)
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

fn observed_audio(source: &SourceObservation, output: &DesiredOutput) -> (u8, Option<u8>) {
    match output.target.id.as_str() {
        "twitch" => (0, source.audio.iter().any(|audio| audio.wire_track == 1).then_some(1)),
        "kick" | "youtube" | "tiktok" => (0, None),
        _ => (output.live_audio_track, output.vod_audio_track),
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
            Codec::Hevc => &["-c:v", "libx265", "-preset", "fast"],
            Codec::Av1 => &[
                "-c:v",
                "libsvtav1",
                "-preset",
                "8",
                "-flags",
                "+global_header",
            ],
        };
        args.extend(codec_args.iter().map(OsString::from));
        if video.codec == Codec::Hevc {
            args.extend(["-x265-params".into(),format!("log-level=error:strict-cbr=1:pools={threads}:frame-threads={threads}:rc-lookahead=0:bframes=0:open-gop=0").into()]);
        }
        if video.codec == Codec::Av1 {
            // lp bezeichnet eine Parallelitätsstufe, keine feste OS-Threadzahl.
            args.extend([
                "-svtav1-params".into(),
                format!(
                    "lp=2:pred-struct=1:irefresh-type=2:profile=0:level={}",
                    video.level
                )
                .into(),
            ]);
        }
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
    fn regression_twitch_single_audio_with_vod_request_uses_live_mix() {
        let mut source = source();
        source.audio.truncate(1);
        source.width = 2560;
        source.height = 1440;
        source.fps_numerator = 60;
        let mut desired = output("twitch", 0, Some(1));
        desired.video.width = 1920;
        desired.video.height = 1080;
        desired.video.fps = FrameRate::new(60, 1).unwrap();
        desired.video.bitrate_kbps = 6000;
        let graph = Graph::observed(&source, &[desired]).unwrap();
        assert_eq!(graph.routes[0].failure, None, "Eine Quellspur muss Twitch samt VOD starten");
        assert_eq!(graph.routes[0].audio, vec![(0, 0)]);
    }

    #[test]
    fn twitch_automatic_audio_uses_one_two_or_three_source_tracks_without_extra_encodes() {
        for count in 1..=3 {
            let mut source = source();
            source.audio = (0..count).map(|wire_track| AudioObservation {wire_track,codec:"aac".into(),sample_rate:48000,channels:2}).collect();
            let graph = Graph::observed(&source, &[output("twitch", 7, Some(12)),output("youtube",1,Some(1)),output("kick",1,Some(1)),output("tiktok",1,Some(1))]).unwrap();
            assert_eq!(graph.profiles.len(),1);
            assert!(graph.routes.iter().all(|route|route.failure.is_none()));
            let twitch=graph.describe_route(&graph.routes[0], "twitch");
            let mapped:Vec<_>=twitch.audio.iter().map(|audio|(audio.source_wire_track,audio.destination_wire_track)).collect();
            assert_eq!(mapped,if count==1 {vec![(0,0)]} else {vec![(0,0),(1,1)]});
            for route in &graph.routes[1..] { assert_eq!(route.audio,vec![(0,0)]); }
            assert_eq!(graph.input_audio.len(),usize::from(count.min(2)));
        }
    }

    #[test]
    fn excessive_video_wish_is_clamped_to_source_and_engine_limits() {
        let mut source=source();
        source.width=7680;
        source.height=4320;
        source.fps_numerator=120;
        let mut desired=output("twitch",0,None);
        desired.video.width=8192;
        desired.video.height=8192;
        desired.video.fps=FrameRate::new(240,1).unwrap();
        desired.video.bitrate_kbps=100000;
        let graph=Graph::observed(&source,&[desired]).unwrap();
        assert_eq!(graph.routes[0].failure,None);
        let video=&graph.profiles[0].video;
        assert_eq!((video.width,video.height,video.fps,video.rate.target_kbps),(4096,4096,FrameRate::new(60,1).unwrap(),20000));
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
    fn unused_auxiliary_audio_does_not_block_selected_mix() {
        let mut source = source();
        source.audio[0].sample_rate = 44100;
        let graph = Graph::observed(&source, &[output("selected", 1, None)]).unwrap();
        assert_eq!(graph.routes[0].failure, None);
        assert_eq!(graph.input_audio.len(), 1);
        assert!(!graph.expected_tracks.contains(&WireTrack {
            kind: MediaKind::Audio,
            wire_id: 0
        }));
        assert_eq!(graph.routes[0].audio, vec![(0, 0)]);
        assert!(matches!(
            Graph::observed(&source, &[output("invalid", 0, None)]),
            Err(MediaError::UnsupportedProfile)
        ));
    }
    #[test]
    fn observed_hdr_and_missing_audio_are_rejected_but_large_wishes_are_clamped() {
        let mut hdr = source();
        hdr.color_transfer = Some("smpte2084".into());
        assert!(matches!(
            Graph::observed(&hdr, &[output("one", 0, None)]),
            Err(MediaError::UnsupportedProfile)
        ));
        let missing = Graph::observed(&source(), &[output("one", 0, Some(7))]).unwrap();
        assert_eq!(missing.routes[0].failure, Some(MediaError::MissingTrack));
        let mut upscale = output("one", 0, None);
        upscale.video.width = 640;
        let upscale = Graph::observed(&source(), &[upscale]).unwrap();
        assert_eq!(upscale.routes[0].failure, None);
        assert_eq!(upscale.profiles[0].video.width, 320);
    }
    #[test]
    fn multivideo_shares_profiles_but_keeps_one_audio_anchor_and_unique_track_ids() {
        use crate::{ProgramAudio, ProgramOutput, ProgramVideo};
        let mut source = source();
        source.audio.push(AudioObservation {
            wire_track: 8,
            codec: "aac".into(),
            sample_rate: 44100,
            channels: 1,
        });
        source.color_primaries = Some("bt709".into());
        source.color_transfer = Some("bt709".into());
        source.color_matrix = Some("bt709".into());
        source.color_range = Some("tv".into());
        let basic = output("shape", 0, None);
        let mut declared = Graph::observed(&source, &[basic]).unwrap();
        let profile = declared.profiles.remove(0).video;
        let video = |id| ProgramVideo {
            wire_track: id,
            canvas_index: 0,
            profile: profile.clone(),
            layout: None,
        };
        let outputs = [
            ProgramOutput {
                target: output("multi", 0, None).target,
                video: vec![video(0), video(7)],
                audio: vec![
                    ProgramAudio {
                        source_wire_track: 0,
                        destination_wire_track: 4,
                    },
                    ProgramAudio {
                        source_wire_track: 1,
                        destination_wire_track: 12,
                    },
                ],
            },
            ProgramOutput {
                target: output("duplicate", 0, None).target,
                video: vec![video(0), video(0)],
                audio: vec![ProgramAudio {
                    source_wire_track: 0,
                    destination_wire_track: 0,
                }],
            },
            ProgramOutput {
                target: output("shared", 0, None).target,
                video: vec![video(0)],
                audio: vec![ProgramAudio {
                    source_wire_track: 1,
                    destination_wire_track: 0,
                }],
            },
        ];
        let graph = Graph::program(&source, &outputs).unwrap();
        assert_eq!(graph.profiles.len(), 1);
        assert_eq!(graph.routes[0].video, vec![(Some(0), 0), (Some(0), 7)]);
        assert_eq!(graph.routes[0].group, Some(0));
        assert_eq!(graph.routes[0].audio, vec![(0, 4), (1, 12)]);
        assert_eq!(
            graph.routes[1].failure,
            Some(MediaError::UnsupportedProfile)
        );
        assert_eq!(graph.routes[2].failure, None);
        source.color_transfer = None;
        let unknown = Graph::program(&source, &outputs).unwrap();
        assert!(unknown.profiles.is_empty());
        assert!(
            unknown
                .routes
                .iter()
                .all(|route| route.failure == Some(MediaError::UnsupportedProfile))
        );
    }
    #[test]
    fn large_reduced_frame_rate_components_do_not_overflow_gop() {
        let mut desired = output("one", 0, None);
        desired.video.fps = FrameRate::new(4_000_000_001, 1_000_000_000).unwrap();
        let graph = Graph::observed(&source(), &[desired]).unwrap();
        assert_eq!(graph.profiles[0].video.gop.keyframe_interval_frames, 9);
    }

    #[test]
    fn missing_vod_or_incompatible_target_does_not_cancel_healthy_outputs() {
        let mut invalid = output("incompatible", 0, None);
        invalid.video.width = 1;
        let graph = Graph::observed(
            &source(),
            &[
                output("missing-vod", 0, Some(12)),
                output("healthy", 1, None),
                invalid,
            ],
        )
        .expect("separate target failures must preserve a healthy graph");
        assert_eq!(graph.profiles.len(), 1);
        assert_eq!(graph.routes.len(), 3);
        assert_eq!(graph.routes[1].audio, vec![(1, 0)]);
        assert_eq!(graph.routes[0].failure, Some(MediaError::MissingTrack));
        assert_eq!(graph.routes[1].failure, None);
        assert_eq!(
            graph.routes[2].failure,
            Some(MediaError::UnsupportedProfile)
        );
    }

    #[test]
    fn processing_description_excludes_rejected_profiles_and_keeps_audio_wire_roles() {
        let graph = Graph::observed(
            &source(),
            &[
                output("missing-vod", 0, Some(12)),
                output("healthy", 1, Some(0)),
            ],
        )
        .unwrap();
        let rejected = graph.describe_route(&graph.routes[0], "missing-vod");
        assert!(rejected.video.is_empty());
        let healthy = graph.describe_route(&graph.routes[1], "healthy");
        assert_eq!(healthy.video.len(), 1);
        assert_eq!(healthy.video[0].mode, "encode");
        let profile = healthy.video[0].profile.as_ref().unwrap();
        assert_eq!(profile.width, graph.profiles[0].video.width);
        assert_eq!(
            profile.target_bitrate_kbps,
            graph.profiles[0].video.rate.target_kbps
        );
        assert_eq!(healthy.audio[0].source_wire_track, 1);
        assert_eq!(healthy.audio[0].destination_wire_track, 0);
        assert_eq!(healthy.audio[1].source_wire_track, 0);
        assert_eq!(healthy.audio[1].destination_wire_track, 1);
        let encoded = serde_json::to_value(healthy).unwrap();
        assert_eq!(encoded["profile_origin"], "running_graph");
        assert!(encoded.get("measured_bitrate_kbps").is_none());
    }
}
