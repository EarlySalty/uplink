use std::time::Duration;
use uplink_core::*;

fn profile(codec: Codec) -> VideoProfile {
    VideoProfile {
        width: 1920,
        height: 1080,
        fps: FrameRate::new(60, 1).unwrap(),
        codec,
        codec_profile: "main".into(),
        level: "4.2".into(),
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
            target_kbps: 6000,
            max_kbps: 6000,
            buffer_kbits: 12000,
        },
        gop: Gop {
            keyframe_interval_frames: 120,
            closed: true,
        },
    }
}

fn audio() -> AudioProfile {
    AudioProfile {
        codec: AudioCodec::Aac,
        sample_rate: 48000,
        channels: 2,
        bitrate_kbps: 160,
    }
}

fn scope() -> SessionScope {
    SessionScope::new(10, 20, 1).unwrap()
}

fn source() -> Source {
    Source {
        scope: scope(),
        video_track: 1,
        video: profile(Codec::Av1),
        audio: vec![
            AudioTrack {
                track_id: 2,
                role: AudioRole::Live,
                profile: audio(),
            },
            AudioTrack {
                track_id: 3,
                role: AudioRole::Vod,
                profile: audio(),
            },
        ],
    }
}

fn limits() -> Limits {
    Limits {
        max_width: 4096,
        max_height: 4096,
        max_pixels: 9_000_000,
        max_fps: FrameRate::new(120, 1).unwrap(),
        max_video_kbps: 20000,
        max_audio_kbps: 512,
        max_audio_tracks: 4,
        max_outputs: 8,
        max_unique_encodes: 4,
    }
}

fn request(id: &str) -> OutputRequest {
    let video = profile(Codec::H264);
    OutputRequest {
        id: id.into(),
        platform: Platform::Twitch,
        video: video.clone(),
        layout: None,
        encoder_preset_revision: "test-1".into(),
        live_audio: AudioRequest {
            role: AudioRole::Live,
            profile: audio(),
        },
        vod_audio: None,
        capabilities: TargetCapabilities {
            video: vec![video],
            audio: vec![audio()],
        },
    }
}

fn input(requests: Vec<OutputRequest>) -> PlanInput {
    PlanInput {
        limits: limits(),
        source: Some(source()),
        worker: WorkerCapabilities {
            encodable_video: vec![profile(Codec::H264)],
            decodable_video: vec![profile(Codec::Av1), profile(Codec::H264)],
            decodable_audio: vec![audio()],
            encodable_audio: vec![audio()],
            compositable_layouts: vec![
                LayoutRevision { id: 1, revision: 1 },
                LayoutRevision { id: 1, revision: 2 },
            ],
        },
        outputs: requests,
    }
}

#[test]
fn equivalent_frame_rates_have_one_identity() {
    assert_eq!(
        FrameRate::new(60000, 1000).unwrap(),
        FrameRate::new(60, 1).unwrap()
    );
    assert_ne!(
        FrameRate::new(60000, 1001).unwrap(),
        FrameRate::new(60, 1).unwrap()
    );
    assert!(FrameRate::new(0, 1).is_err());
    assert!(FrameRate::new(60, 0).is_err());
}

#[test]
fn different_audio_routes_share_video_encode() {
    let one = request("twitch");
    let mut two = request("kick");
    two.platform = Platform::Kick;
    two.live_audio.role = AudioRole::Vod;
    let plan = plan(&input(vec![one, two])).unwrap();
    assert_eq!(plan.encode_groups.len(), 1);
    assert_eq!(plan.encode_groups[0].outputs, vec!["twitch", "kick"]);
    assert_eq!(plan.shared_video_decode_count, 1);
    assert_eq!(plan.outputs[1].live_audio.as_ref().unwrap().source_track, 3);
}

#[test]
fn missing_vod_does_not_fall_back_to_live() {
    let mut req = request("twitch");
    req.vod_audio = Some(AudioRequest {
        role: AudioRole::Vod,
        profile: audio(),
    });
    let mut scenario = input(vec![req]);
    scenario
        .source
        .as_mut()
        .unwrap()
        .audio
        .retain(|track| track.role != AudioRole::Vod);
    let plan = plan(&scenario).unwrap();
    assert_eq!(plan.outputs[0].status, OutputStatus::Rejected);
    assert!(
        plan.outputs[0]
            .reasons
            .contains(&Rejection::MissingAudio(AudioRole::Vod))
    );
    assert!(plan.encode_groups.is_empty());
}

#[test]
fn passthrough_requires_full_profile_match() {
    let mut scenario = input(vec![request("youtube")]);
    scenario.source.as_mut().unwrap().video = profile(Codec::H264);
    assert_eq!(
        plan(&scenario).unwrap().outputs[0].video,
        Some(VideoAction::Copy)
    );
    scenario.source.as_mut().unwrap().video.rate.target_kbps = 5000;
    scenario.source.as_mut().unwrap().video.rate.max_kbps = 5000;
    scenario
        .worker
        .decodable_video
        .push(scenario.source.as_ref().unwrap().video.clone());
    assert_eq!(
        plan(&scenario).unwrap().outputs[0].video,
        Some(VideoAction::Encode)
    );
}

#[test]
fn layout_and_keyframe_changes_split_encodes() {
    let one = request("one");
    let mut two = request("two");
    two.layout = Some(LayoutRevision { id: 1, revision: 2 });
    let mut three = request("three");
    three.video.gop.keyframe_interval_frames = 60;
    three.capabilities.video = vec![three.video.clone()];
    let mut scenario = input(vec![one, two, three]);
    scenario
        .worker
        .encodable_video
        .push(scenario.outputs[2].video.clone());
    assert_eq!(plan(&scenario).unwrap().encode_groups.len(), 3);
}

#[test]
fn encode_keys_never_share_between_tenants_or_generations() {
    let first = plan(&input(vec![request("one")]))
        .unwrap()
        .encode_groups
        .remove(0)
        .key;
    let mut changed = input(vec![request("one")]);
    changed.source.as_mut().unwrap().scope = SessionScope::new(11, 20, 1).unwrap();
    let second = plan(&changed).unwrap().encode_groups.remove(0).key;
    assert_ne!(first, second);
    changed.source.as_mut().unwrap().scope = SessionScope::new(10, 20, 2).unwrap();
    let third = plan(&changed).unwrap().encode_groups.remove(0).key;
    assert_ne!(first, third);
}

#[test]
fn unknown_input_stays_pending_and_keeps_wish() {
    let mut scenario = input(vec![request("one")]);
    scenario.source = None;
    let result = plan(&scenario).unwrap();
    assert_eq!(result.outputs[0].status, OutputStatus::PendingInput);
    assert_eq!(result.outputs[0].requested_video, scenario.outputs[0].video);
    assert!(result.encode_groups.is_empty());
}

#[test]
fn unavailable_target_audio_is_rejected_without_source() {
    for role in [AudioRole::Live, AudioRole::Vod] {
        let mut scenario = input(vec![request("one")]);
        scenario.source = None;
        if role == AudioRole::Live {
            scenario.outputs[0].capabilities.audio.clear();
        } else {
            let mut unsupported_audio = audio();
            unsupported_audio.sample_rate = 44100;
            scenario.outputs[0].vod_audio = Some(AudioRequest {
                role,
                profile: unsupported_audio,
            });
        }
        let result = plan(&scenario).unwrap();
        assert_eq!(result.outputs[0].status, OutputStatus::Rejected);
        assert_eq!(
            result.outputs[0].reasons,
            vec![Rejection::TargetAudioUnsupported(role)]
        );
        assert_eq!(result.outputs[0].live_audio, None);
        assert_eq!(result.outputs[0].vod_audio, None);
        assert!(result.encode_groups.is_empty());
    }
}

#[test]
fn unavailable_layout_is_rejected_without_source() {
    let mut scenario = input(vec![request("one")]);
    scenario.source = None;
    scenario.outputs[0].layout = Some(LayoutRevision {
        id: 77,
        revision: 1,
    });
    let result = plan(&scenario).unwrap();
    assert_eq!(result.outputs[0].status, OutputStatus::Rejected);
    assert_eq!(
        result.outputs[0].reasons,
        vec![Rejection::LayoutWorkerUnavailable]
    );
    assert_eq!(result.outputs[0].video, None);
    assert!(result.encode_groups.is_empty());
}

#[test]
fn unsupported_target_is_rejected_without_lowering_quality() {
    let mut scenario = input(vec![request("one")]);
    scenario.outputs[0].capabilities.video.clear();
    let result = plan(&scenario).unwrap();
    assert!(
        result.outputs[0]
            .reasons
            .contains(&Rejection::TargetVideoUnsupported)
    );
    assert_eq!(result.outputs[0].requested_video, scenario.outputs[0].video);
    assert!(result.encode_groups.is_empty());
}

#[test]
fn capacity_exhaustion_rejects_plan_without_partial_reservation() {
    let mut scenario = input(vec![request("one"), request("two")]);
    scenario.outputs[1].layout = Some(LayoutRevision { id: 1, revision: 1 });
    scenario.limits.max_unique_encodes = 1;
    assert!(matches!(plan(&scenario), Err(Error::EncodeCapacity)));
}

#[test]
fn encoding_requires_source_decoder_and_layout_capability() {
    let mut scenario = input(vec![request("one")]);
    scenario.worker.decodable_video.clear();
    assert!(
        plan(&scenario).unwrap().outputs[0]
            .reasons
            .contains(&Rejection::VideoDecoderUnavailable)
    );
    scenario.worker.decodable_video.push(source().video);
    scenario.outputs[0].layout = Some(LayoutRevision {
        id: 77,
        revision: 1,
    });
    assert!(
        plan(&scenario).unwrap().outputs[0]
            .reasons
            .contains(&Rejection::LayoutWorkerUnavailable)
    );
}

#[test]
fn duplicate_tracks_and_invalid_dimensions_are_rejected() {
    let mut scenario = input(vec![request("one")]);
    scenario.source.as_mut().unwrap().audio[1].track_id = 2;
    assert!(plan(&scenario).is_err());
    let mut scenario = input(vec![request("one")]);
    scenario.outputs[0].video.width = 0;
    assert!(plan(&scenario).is_err());
}

fn buffer() -> PacketBuffer {
    PacketBuffer::new(
        scope(),
        1,
        BufferLimits {
            max_bytes: 8,
            max_packets: 3,
            max_age: Duration::from_millis(100),
        },
    )
    .unwrap()
}

fn packet(scope: SessionScope, dts: u64, size: usize) -> Packet {
    Packet {
        scope,
        track_id: 1,
        decode_timestamp_ms: dts,
        payload: vec![1; size].into_boxed_slice(),
    }
}

#[test]
fn byte_backpressure_is_explicit_and_preserves_existing_packets() {
    let mut buffer = buffer();
    buffer.push(packet(scope(), 0, 6), Duration::ZERO).unwrap();
    assert_eq!(
        buffer.push(packet(scope(), 1, 3), Duration::ZERO),
        Err(BufferError::Full)
    );
    assert_eq!(buffer.bytes(), 6);
    let output = buffer.pop(Duration::ZERO).unwrap();
    assert_eq!(output.packet.unwrap().payload.len(), 6);
}

#[test]
fn age_limit_drops_old_packets_with_visible_count() {
    let mut buffer = buffer();
    buffer.push(packet(scope(), 0, 6), Duration::ZERO).unwrap();
    let result = buffer
        .push(packet(scope(), 1, 3), Duration::from_millis(101))
        .unwrap();
    assert_eq!(result.expired_packets, 1);
    assert_eq!(buffer.bytes(), 3);
}

#[test]
fn packets_from_old_generation_cannot_enter_new_buffer() {
    let mut buffer = buffer();
    let old = SessionScope::new(10, 20, 2).unwrap();
    assert_eq!(
        buffer.push(packet(old, 0, 1), Duration::ZERO),
        Err(BufferError::WrongScope)
    );
    assert_eq!(buffer.bytes(), 0);
}

#[test]
fn packet_count_time_and_decode_order_are_bounded() {
    let mut buffer = buffer();
    for timestamp in 5001..=5003 {
        buffer
            .push(packet(scope(), timestamp, 1), Duration::from_millis(10))
            .unwrap();
    }
    assert_eq!(
        buffer.push(packet(scope(), 5004, 1), Duration::from_millis(10)),
        Err(BufferError::Full)
    );
    assert_eq!(
        buffer.pop(Duration::from_millis(9)),
        Err(BufferError::ClockRegression)
    );
    buffer.pop(Duration::from_millis(10)).unwrap();
    assert_eq!(
        buffer.push(packet(scope(), 0, 1), Duration::from_millis(10)),
        Err(BufferError::TimestampRegression)
    );
}

#[test]
fn timestamp_regression_buffer_clamps_small_steps_and_limits_repeated_steps() {
    for delta in [1, 2000] {
        let mut buffer = buffer();
        buffer
            .push(packet(scope(), 3000, 1), Duration::ZERO)
            .unwrap();
        buffer.pop(Duration::ZERO).unwrap();
        for _ in 0..50 {
            buffer
                .push(packet(scope(), 3000 - delta, 1), Duration::ZERO)
                .unwrap();
            assert_eq!(
                buffer
                    .pop(Duration::ZERO)
                    .unwrap()
                    .packet
                    .unwrap()
                    .decode_timestamp_ms,
                3000
            );
        }
        assert_eq!(
            buffer.push(packet(scope(), 3000 - delta, 1), Duration::ZERO),
            Err(BufferError::TimestampRegression)
        );
        buffer
            .push(packet(scope(), 3000 - delta, 1), Duration::from_secs(60))
            .unwrap();
    }
}

#[test]
fn timestamp_regression_buffer_large_jump_and_sliding_window_boundaries() {
    let mut buffer = buffer();
    buffer
        .push(packet(scope(), 10000, 1), Duration::ZERO)
        .unwrap();
    buffer.pop(Duration::ZERO).unwrap();
    assert_eq!(
        buffer.push(packet(scope(), 7999, 1), Duration::ZERO),
        Err(BufferError::TimestampRegression)
    );
    for index in 0..50 {
        let now = Duration::from_secs(index);
        buffer.push(packet(scope(), 9999, 1), now).unwrap();
        buffer.pop(now).unwrap();
    }
    assert_eq!(
        buffer.push(packet(scope(), 9999, 1), Duration::from_millis(59999)),
        Err(BufferError::TimestampRegression)
    );
    buffer
        .push(packet(scope(), 9999, 1), Duration::from_secs(60))
        .unwrap();
    buffer.pop(Duration::from_secs(60)).unwrap();
    assert_eq!(
        buffer.push(packet(scope(), 9999, 1), Duration::from_secs(60)),
        Err(BufferError::TimestampRegression)
    );
    buffer
        .push(packet(scope(), 9999, 1), Duration::from_secs(61))
        .unwrap();
}
