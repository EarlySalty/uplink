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
    pub audio_encoding: Vec<(u8, crate::AudioEncoding)>,
    pub layout: Option<LayoutSpec>,
    pub bframes: u32,
    /// Fehlende Farbmetadaten der Quelle bleiben unbekannt und werden nicht umetikettiert.
    pub signal_bt709: bool,
}
#[derive(Clone)]
pub(crate) struct Routing {
    pub timestamp_offset_ms: u32,
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
            timestamp_offset_ms: route.timestamp_offset_ms,
            id: id.to_owned(),
            profile_origin: "running_graph",
            video: route
                .video
                .iter()
                .map(|(group, wire)| crate::VideoProcessing {
                    wire_track: *wire,
                    canvas_index: group
                        .and_then(|group| self.profiles.get(group))
                        .map_or(0, |p| u8::from(p.layout.is_some())),
                    encoder: group.and_then(|group| self.profiles.get(group)).map(|p| {
                        match p.video.codec {
                            Codec::H264 => "libx264",
                            Codec::Hevc => "libx265",
                            Codec::Av1 => "libsvtav1",
                        }
                    }),
                    layout_id: group
                        .and_then(|group| self.profiles.get(group))
                        .and_then(|p| p.layout.as_ref())
                        .map(|l| l.revision.id),
                    layout_revision: group
                        .and_then(|group| self.profiles.get(group))
                        .and_then(|p| p.layout.as_ref())
                        .map(|l| l.revision.revision),
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
                bframes: 0,
                signal_bt709: true,
                audio_encoding: Vec::new(),
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
                timestamp_offset_ms: 0,
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
                std::iter::once(output.live_audio_track).chain(output.vod_audio_track)
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
                let desired = &output.video;
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
                            bframes: 0,
                            signal_bt709: false,
                            audio_encoding: Vec::new(),
                        });
                        profiles.len() - 1
                    }
                };
                let mut audio = Vec::new();
                for (destination, wire_id) in
                    [Some(output.live_audio_track), output.vod_audio_track]
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
                    timestamp_offset_ms: 0,
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
                        timestamp_offset_ms: 0,
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
        Self::mixed(source, &[], outputs)
    }

    pub(crate) fn mixed(
        source: &SourceObservation,
        desired: &[DesiredOutput],
        outputs: &[crate::ProgramOutput],
    ) -> Result<Self> {
        let selected = outputs
            .iter()
            .flat_map(|output| output.audio.iter().map(|audio| audio.source_wire_track))
            .chain(desired.iter().flat_map(|output| {
                std::iter::once(output.live_audio_track).chain(output.vod_audio_track)
            }))
            .collect();
        let mut graph = Self::observed_selected(source, desired, &selected)?;
        let source_fps = uplink_core::FrameRate::new(source.fps_numerator, source.fps_denominator)
            .map_err(|_| MediaError::InvalidMedia)?;
        let mut target_ids: HashSet<_> = desired.iter().map(|output| &output.target.id).collect();
        let mut shared_audio = Vec::new();
        let mut shared_audio_requests = HashMap::new();
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
                let mut audio_encoding = Vec::new();
                let mut audio_requests = HashMap::new();
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
                    if shared_audio_requests
                        .get(&source)
                        .is_some_and(|prior| *prior != route.encoding)
                        || audio_requests
                            .insert(source, route.encoding)
                            .is_some_and(|prior| prior != route.encoding)
                    {
                        return Err(MediaError::UnsupportedProfile);
                    }
                    if let Some(encoding) = route.encoding {
                        if !(1..=2).contains(&encoding.channels)
                            || !(32..=320).contains(&encoding.bitrate_kbps)
                        {
                            return Err(MediaError::UnsupportedProfile);
                        }
                        if !audio_encoding.contains(&(source, encoding)) {
                            audio_encoding.push((source, encoding));
                        }
                    }
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
                        || profile.fps
                            > uplink_core::FrameRate::new(60, 1)
                                .map_err(|_| MediaError::InvalidConfiguration)?
                        || profile.rate.target_kbps == 0
                        || profile.rate.target_kbps > 20_000
                        || profile.rate.max_kbps != profile.rate.target_kbps
                        || profile.rate.buffer_kbits == 0
                        || profile.rate.buffer_kbits > profile.rate.target_kbps * 4
                        || profile.gop.keyframe_interval_frames == 0
                        || profile.gop.keyframe_interval_frames > 480
                        || !profile.gop.closed
                        || request.bframes > 2
                        || (profile.codec != Codec::H264 && request.bframes != 0)
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
                    let copy_source = request.canvas_index == 0
                        && request.layout.is_none()
                        && profile.codec == Codec::Hevc
                        && source.codec == "hevc"
                        && profile.width == source.width
                        && profile.height == source.height
                        && profile.fps == source_fps;
                    if copy_source {
                        video.push((None, request.wire_track));
                        continue;
                    }
                    let group = match graph.profiles.iter().position(|existing| {
                        existing.video == *profile
                            && existing.layout == request.layout
                            && existing.bframes == request.bframes
                    }) {
                        Some(index) => {
                            graph.profiles[index].signal_bt709 = true;
                            index
                        }
                        None => {
                            graph.profiles.push(EncodeProfile {
                                video: profile.clone(),
                                layout: request.layout.clone(),
                                bframes: request.bframes,
                                signal_bt709: true,
                                audio_encoding: audio_encoding.clone(),
                            });
                            graph.profiles.len() - 1
                        }
                    };
                    video.push((Some(group), request.wire_track));
                }
                for &encoding in &audio_encoding {
                    if !shared_audio.contains(&encoding) {
                        shared_audio.push(encoding);
                    }
                }
                shared_audio_requests.extend(audio_requests);
                let audio_group = video.iter().find_map(|(group, _)| *group);
                if !audio_encoding.is_empty() && audio_group.is_none() {
                    return Err(MediaError::UnsupportedProfile);
                }
                Ok(Routing {
                    timestamp_offset_ms: 0,
                    group: audio_group,
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
                        timestamp_offset_ms: 0,
                        group: None,
                        video: Vec::new(),
                        audio: Vec::new(),
                        failure: Some(reason),
                    }
                }
            });
        }
        for profile in &mut graph.profiles {
            profile.audio_encoding = shared_audio.clone();
        }
        if !shared_audio.is_empty() {
            for route in graph.routes.iter_mut().take(desired.len()) {
                route.group = None;
            }
            for route in graph.routes.iter_mut().skip(desired.len()) {
                route.timestamp_offset_ms = AAC_MUX_OFFSET_MS;
            }
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
        if self.profiles.iter().any(|profile| {
            profile
                .maximum_input_timestamp_ms()
                .is_some_and(|maximum| video_origin_pts_ms > maximum)
        }) {
            return Err(MediaError::InvalidMedia);
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
            for (track, encoding) in &profile.audio_encoding {
                args.extend([
                    format!("-c:a:{track}").into(),
                    "aac".into(),
                    format!("-b:a:{track}").into(),
                    format!("{}k", encoding.bitrate_kbps).into(),
                    format!("-ac:a:{track}").into(),
                    encoding.channels.to_string().into(),
                    format!("-ar:a:{track}").into(),
                    "48000".into(),
                ]);
            }
            profile.encoder_args(&mut args, threads);
            if profile.mux_timestamp_offset_ms() != 0 {
                args.extend([
                    "-output_ts_offset".into(),
                    format!("0.{:03}", profile.mux_timestamp_offset_ms()).into(),
                ]);
            }
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
const AAC_MUX_OFFSET_MS: u32 = (1024_u32 * 1000).div_ceil(48000);

impl EncodeProfile {
    pub(crate) fn mux_timestamp_offset_ms(&self) -> u32 {
        if self.audio_encoding.is_empty() {
            0
        } else {
            AAC_MUX_OFFSET_MS
        }
    }

    pub(crate) fn maximum_input_timestamp_ms(&self) -> Option<i64> {
        if self.audio_encoding.is_empty() {
            return None;
        }
        let numerator = i64::from(self.video.fps.numerator());
        let frame_ms = (i64::from(self.video.fps.denominator()) * 1000 + numerator - 1) / numerator;
        Some(
            i64::from(i32::MAX)
                - i64::from(AAC_MUX_OFFSET_MS)
                - frame_ms.max(i64::from(AAC_MUX_OFFSET_MS)),
        )
    }

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
        match video.codec {
            Codec::H264 => {
                args.extend(
                    [
                        "-c:v",
                        "libx264",
                        "-preset",
                        "veryfast",
                        "-x264-params",
                        "nal-hrd=cbr:force-cfr=1",
                    ]
                    .into_iter()
                    .map(OsString::from),
                );
                if self.bframes == 0 {
                    args.extend(["-tune".into(), "zerolatency".into()]);
                }
            }
            Codec::Hevc => args.extend(
                ["-c:v", "libx265", "-preset", "fast"]
                    .into_iter()
                    .map(OsString::from),
            ),
            Codec::Av1 => args.extend(
                [
                    "-c:v",
                    "libsvtav1",
                    "-preset",
                    "8",
                    "-flags",
                    "+global_header",
                ]
                .into_iter()
                .map(OsString::from),
            ),
        }
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
            ("-bf", self.bframes.to_string()),
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
    use uplink_core::{
        Chroma, Color, ColorPrimaries, ColorRange, FrameRate, Gop, LayoutRevision, Matrix,
        RateControl, Transfer,
    };

    fn program_source() -> SourceObservation {
        let mut source = source();
        source.color_primaries = Some("bt709".into());
        source.color_transfer = Some("bt709".into());
        source.color_matrix = Some("bt709".into());
        source.color_range = Some("tv".into());
        source
    }
    fn program_profile(codec: Codec, width: u32, height: u32) -> VideoProfile {
        VideoProfile {
            width,
            height,
            fps: FrameRate::new(25, 1).unwrap(),
            codec,
            codec_profile: if codec == Codec::H264 { "high" } else { "main" }.into(),
            level: if codec == Codec::H264 { "4.2" } else { "5.1" }.into(),
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
                target_kbps: 384,
                max_kbps: 384,
                buffer_kbits: 768,
            },
            gop: Gop {
                keyframe_interval_frames: 50,
                closed: true,
            },
        }
    }
    fn program_video(
        wire_track: u8,
        canvas_index: u8,
        profile: VideoProfile,
        layout: Option<LayoutSpec>,
    ) -> crate::ProgramVideo {
        crate::ProgramVideo {
            wire_track,
            canvas_index,
            profile,
            bframes: 0,
            layout,
        }
    }
    fn program_audio(source: u8, destination: u8) -> crate::ProgramAudio {
        crate::ProgramAudio {
            encoding: None,
            source_wire_track: source,
            destination_wire_track: destination,
        }
    }
    fn program_output(
        id: &str,
        video: Vec<crate::ProgramVideo>,
        audio: Vec<crate::ProgramAudio>,
    ) -> crate::ProgramOutput {
        crate::ProgramOutput {
            target: output(id, 0, None).target,
            video,
            audio,
        }
    }
    fn crop_layout(x: u32, y: u32, width: u32, height: u32) -> LayoutSpec {
        LayoutSpec {
            revision: LayoutRevision { id: 1, revision: 1 },
            composition: Composition::Crop(Crop {
                x,
                y,
                width,
                height,
            }),
        }
    }
    #[test]
    fn native_2k_program_copies_hevc_top_and_encodes_only_lower_h264() {
        let mut source = program_source();
        source.codec = "hevc".into();
        source.width = 2560;
        source.height = 1440;
        source.fps_numerator = 60;
        source.fps_denominator = 1;
        let mut top = program_profile(Codec::Hevc, 2560, 1440);
        top.fps = FrameRate::new(60, 1).unwrap();
        top.rate.target_kbps = 9_000;
        top.rate.max_kbps = 9_000;
        top.rate.buffer_kbits = 18_000;
        top.gop.keyframe_interval_frames = 120;
        let mut lower = program_profile(Codec::H264, 1920, 1080);
        lower.fps = FrameRate::new(60, 1).unwrap();
        lower.rate.target_kbps = 7_500;
        lower.rate.max_kbps = 7_500;
        lower.rate.buffer_kbits = 15_000;
        lower.gop.keyframe_interval_frames = 120;
        let graph = Graph::program(
            &source,
            &[program_output(
                "twitch",
                vec![
                    crate::ProgramVideo {
                        wire_track: 0,
                        canvas_index: 0,
                        profile: top,
                        bframes: 0,
                        layout: None,
                    },
                    crate::ProgramVideo {
                        wire_track: 1,
                        canvas_index: 0,
                        profile: lower,
                        bframes: 2,
                        layout: None,
                    },
                ],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert!(graph.routes[0].failure.is_none());
        assert_eq!(
            graph.profiles.len(),
            1,
            "1440p HEVC darf keinen Encode belegen"
        );
        assert_eq!(graph.profiles[0].video.codec, Codec::H264);
        assert_eq!(graph.profiles[0].bframes, 2);
        assert_eq!(graph.routes[0].video[0], (None, 0));
        assert_eq!(graph.routes[0].video[1].1, 1);
        assert!(graph.routes[0].video[1].0.is_some());
        let status = graph.describe_route(&graph.routes[0], "twitch");
        assert_eq!(status.video[0].mode, "copy");
        assert_eq!(status.video[0].encoder, None);
        assert_eq!(status.video[1].mode, "encode");
        assert_eq!(status.video[1].encoder, Some("libx264"));
        let args = graph
            .arguments(&[PathBuf::from("/private/native-2k.sock")], 4, 0)
            .unwrap();
        let args: Vec<_> = args.iter().map(|value| value.to_string_lossy()).collect();
        assert_eq!(
            args.iter().filter(|arg| arg.as_ref() == "libx264").count(),
            1
        );
        assert!(!args.iter().any(|arg| arg.as_ref() == "libx265"));
        assert!(!args.iter().any(|arg| arg.as_ref() == "zerolatency"));
        assert!(
            args.windows(2)
                .any(|pair| pair[0].as_ref() == "-bf" && pair[1].as_ref() == "2")
        );
    }

    #[test]
    fn native_2k_av1_program_encodes_hevc_top_and_lower_h264() {
        let mut source = program_source();
        source.codec = "av1".into();
        source.width = 2560;
        source.height = 1440;
        source.fps_numerator = 60;
        source.fps_denominator = 1;
        let mut top = program_profile(Codec::Hevc, 2560, 1440);
        top.fps = FrameRate::new(60, 1).unwrap();
        top.rate.target_kbps = 9_000;
        top.rate.max_kbps = 9_000;
        top.rate.buffer_kbits = 18_000;
        top.gop.keyframe_interval_frames = 120;
        let mut lower = program_profile(Codec::H264, 1920, 1080);
        lower.fps = FrameRate::new(60, 1).unwrap();
        lower.rate.target_kbps = 7_500;
        lower.rate.max_kbps = 7_500;
        lower.rate.buffer_kbits = 15_000;
        lower.gop.keyframe_interval_frames = 120;
        let graph = Graph::program(
            &source,
            &[program_output(
                "twitch",
                vec![
                    crate::ProgramVideo {
                        wire_track: 0,
                        canvas_index: 0,
                        profile: top,
                        bframes: 0,
                        layout: None,
                    },
                    crate::ProgramVideo {
                        wire_track: 1,
                        canvas_index: 0,
                        profile: lower,
                        bframes: 2,
                        layout: None,
                    },
                ],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert!(graph.routes[0].failure.is_none());
        assert_eq!(graph.profiles.len(), 2);
        assert_eq!(graph.profiles[0].video.codec, Codec::Hevc);
        assert_eq!(graph.profiles[1].video.codec, Codec::H264);
        let status = graph.describe_route(&graph.routes[0], "twitch");
        assert!(status.video.iter().all(|video| video.mode == "encode"));
        assert_eq!(status.video[0].encoder, Some("libx265"));
        assert_eq!(status.video[1].encoder, Some("libx264"));
        let args = graph
            .arguments(
                &[
                    PathBuf::from("/private/native-2k-av1-top.sock"),
                    PathBuf::from("/private/native-2k-av1-low.sock"),
                ],
                4,
                0,
            )
            .unwrap();
        let args: Vec<_> = args.iter().map(|value| value.to_string_lossy()).collect();
        assert!(args.iter().any(|arg| arg.as_ref() == "libx265"));
        assert!(args.iter().any(|arg| arg.as_ref() == "libx264"));
    }

    #[test]
    fn program_caps_output_frame_rate_like_the_single_track_path() {
        let mut source = program_source();
        source.fps_numerator = 120;
        let mut over = program_profile(Codec::H264, 256, 144);
        over.fps = FrameRate::new(120, 1).unwrap();
        let graph = Graph::program(
            &source,
            &[program_output(
                "too-fast",
                vec![program_video(0, 0, over, None)],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::UnsupportedProfile),
            "Der Mehrspurpfad braucht denselben Bildraten-Deckel wie der Einzelspurpfad"
        );
        assert!(graph.profiles.is_empty());
        let mut at = program_profile(Codec::H264, 256, 144);
        at.fps = FrameRate::new(60, 1).unwrap();
        let graph = Graph::program(
            &source,
            &[program_output(
                "ceiling",
                vec![program_video(0, 0, at, None)],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert_eq!(graph.routes[0].failure, None);
        assert_eq!(graph.profiles.len(), 1);
    }
    #[test]
    fn program_enforces_canvas_and_wire_track_contract() {
        let source = program_source();
        let audio = vec![program_audio(0, 0)];
        let graph = Graph::program(
            &source,
            &[program_output(
                "canvas-without-layout",
                vec![program_video(
                    0,
                    1,
                    program_profile(Codec::H264, 256, 144),
                    None,
                )],
                audio.clone(),
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::UnsupportedProfile),
            "Canvas 1 ohne Layout bleibt abgewiesen"
        );
        let graph = Graph::program(
            &source,
            &[program_output(
                "unknown-canvas",
                vec![program_video(
                    0,
                    2,
                    program_profile(Codec::H264, 256, 144),
                    Some(crop_layout(0, 0, 180, 180)),
                )],
                audio.clone(),
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::UnsupportedProfile),
            "Canvas jenseits 1 bleibt abgewiesen"
        );
        let graph = Graph::program(
            &source,
            &[program_output(
                "split-canvas",
                vec![
                    program_video(0, 0, program_profile(Codec::H264, 256, 144), None),
                    program_video(
                        5,
                        0,
                        program_profile(Codec::Hevc, 144, 256),
                        Some(crop_layout(0, 0, 180, 180)),
                    ),
                ],
                audio.clone(),
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::InvalidConfiguration),
            "Ein Canvas trägt genau ein Layout"
        );
        let graph = Graph::program(
            &source,
            &[program_output(
                "duplicate-wire",
                vec![
                    program_video(0, 0, program_profile(Codec::H264, 256, 144), None),
                    program_video(0, 0, program_profile(Codec::Hevc, 144, 256), None),
                ],
                audio.clone(),
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::UnsupportedProfile),
            "Ein Video-Draht trägt genau eine Ausgabe"
        );
    }
    #[test]
    fn program_accepts_hevc_only_with_its_explicit_contract() {
        let source = program_source();
        let hevc = program_profile(Codec::Hevc, 144, 256);
        let graph = Graph::program(
            &source,
            &[program_output(
                "hevc",
                vec![program_video(5, 0, hevc.clone(), None)],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert_eq!(graph.routes[0].failure, None);
        assert_eq!(graph.profiles.len(), 1);
        assert_eq!(graph.profiles[0].video.codec, Codec::Hevc);
        assert!(graph.profiles[0].signal_bt709);
        let mut wrong_profile = hevc.clone();
        wrong_profile.codec_profile = "high".into();
        let graph = Graph::program(
            &source,
            &[program_output(
                "hevc-high",
                vec![program_video(5, 0, wrong_profile, None)],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::UnsupportedProfile)
        );
        let mut rate_hq = hevc;
        rate_hq.rate.mode = RateMode::HqCbr;
        let graph = Graph::program(
            &source,
            &[program_output(
                "hevc-hqcbr",
                vec![program_video(5, 0, rate_hq, None)],
                vec![program_audio(0, 0)],
            )],
        )
        .unwrap();
        assert_eq!(
            graph.routes[0].failure,
            Some(MediaError::UnsupportedProfile)
        );
    }

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
    fn mixed_rejects_duplicate_ids_and_isolates_unknown_program_colour() {
        let program = |id| {
            program_output(
                id,
                vec![program_video(
                    0,
                    0,
                    program_profile(Codec::H264, 256, 144),
                    None,
                )],
                vec![program_audio(0, 0)],
            )
        };
        assert!(matches!(
            Graph::mixed(&source(), &[output("same", 0, None)], &[program("same")]),
            Err(MediaError::InvalidConfiguration)
        ));
        let graph = Graph::mixed(
            &source(),
            &[output("youtube", 0, None)],
            &[program("twitch")],
        )
        .unwrap();
        assert_eq!(graph.routes[0].failure, None);
        assert_eq!(
            graph.routes[1].failure,
            Some(MediaError::UnsupportedProfile)
        );
        assert_eq!(graph.profiles.len(), 1);
        assert!(!graph.profiles[0].signal_bt709);
    }

    #[test]
    fn program_audio_copy_encode_conflicts_are_isolated_in_both_orders() {
        for encoded_first in [false, true] {
            let program = |id, encoded| {
                let mut audio = program_audio(0, 0);
                if encoded {
                    audio.encoding = Some(crate::AudioEncoding {
                        channels: 2,
                        bitrate_kbps: 160,
                    });
                }
                program_output(
                    id,
                    vec![program_video(
                        0,
                        0,
                        program_profile(Codec::H264, 256, 144),
                        None,
                    )],
                    vec![audio],
                )
            };
            let graph = Graph::mixed(
                &program_source(),
                &[output("ordinary", 0, None)],
                &[
                    program("first", encoded_first),
                    program("conflict", !encoded_first),
                ],
            )
            .unwrap();
            assert_eq!(graph.routes[0].failure, None);
            assert_eq!(graph.routes[1].failure, None);
            assert_eq!(
                graph.routes[2].failure,
                Some(MediaError::UnsupportedProfile)
            );
            assert_eq!(
                graph
                    .profiles
                    .iter()
                    .any(|profile| !profile.audio_encoding.is_empty()),
                encoded_first
            );
            if encoded_first {
                assert_eq!(graph.routes[0].group, None);
            }
        }
    }

    #[test]
    fn encoded_aac_reserves_mux_offset_before_the_flv_timestamp_boundary() {
        let mut audio = program_audio(0, 0);
        audio.encoding = Some(crate::AudioEncoding {
            channels: 2,
            bitrate_kbps: 160,
        });
        let graph = Graph::program(
            &program_source(),
            &[program_output(
                "twitch",
                vec![program_video(
                    0,
                    0,
                    program_profile(Codec::H264, 256, 144),
                    None,
                )],
                vec![audio],
            )],
        )
        .unwrap();
        let maximum = graph.profiles[0].maximum_input_timestamp_ms().unwrap();
        assert_eq!(maximum, i64::from(i32::MAX) - 22 - 40);
        assert_eq!(graph.routes[0].timestamp_offset_ms, 22);
        let paths = [PathBuf::from("/private/example.sock")];
        assert!(graph.arguments(&paths, 1, maximum).is_ok());
        assert!(matches!(
            graph.arguments(&paths, 1, maximum + 1),
            Err(MediaError::InvalidMedia)
        ));
        let copied = Graph::observed(&source(), &[output("youtube", 0, None)]).unwrap();
        assert_eq!(copied.profiles[0].maximum_input_timestamp_ms(), None);
        assert_eq!(copied.routes[0].timestamp_offset_ms, 0);
    }
    #[test]
    fn observed_hdr_missing_audio_and_unapproved_upscale_are_rejected() {
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
        assert_eq!(
            upscale.routes[0].failure,
            Some(MediaError::UnsupportedProfile)
        );
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
            bframes: 0,
            layout: None,
        };
        let outputs = [
            ProgramOutput {
                target: output("multi", 0, None).target,
                video: vec![video(0), video(7)],
                audio: vec![
                    ProgramAudio {
                        encoding: None,
                        source_wire_track: 0,
                        destination_wire_track: 4,
                    },
                    ProgramAudio {
                        encoding: None,
                        source_wire_track: 1,
                        destination_wire_track: 12,
                    },
                ],
            },
            ProgramOutput {
                target: output("duplicate", 0, None).target,
                video: vec![video(0), video(0)],
                audio: vec![ProgramAudio {
                    encoding: None,
                    source_wire_track: 0,
                    destination_wire_track: 0,
                }],
            },
            ProgramOutput {
                target: output("shared", 0, None).target,
                video: vec![video(0)],
                audio: vec![ProgramAudio {
                    encoding: None,
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
        invalid.video.width = 640;
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
