use super::*;
use crate::PublishSecret;
use fixtures::{
    audio_track, build_response, dual_canvas_preferences, fps, hardware_quality, hosts, key,
    preferences, preferences_with, response, x264_quality,
};

mod fixtures;

#[test]
fn http_auth_rejection_is_distinct_from_transient_contract_failure() {
    for status in [401, 403, 302] {
        assert_eq!(
            validate_http_status(reqwest::StatusCode::from_u16(status).unwrap()),
            Err(GoLiveError::HttpRejected)
        );
    }
    for status in [400, 404, 408, 422, 429, 500, 502, 503, 504] {
        assert_eq!(
            validate_http_status(reqwest::StatusCode::from_u16(status).unwrap()),
            Err(GoLiveError::ServiceUnavailable)
        );
    }
    assert_eq!(validate_http_status(reqwest::StatusCode::OK), Ok(()));
}

const HARDWARE_ENCODERS: [&str; 15] = [
    "jim_nvenc",
    "obs_nvenc_h264_tex",
    "jim_hevc_nvenc",
    "obs_nvenc_hevc_tex",
    "jim_av1_nvenc",
    "obs_nvenc_av1_tex",
    "h264_texture_amf",
    "h265_texture_amf",
    "av1_texture_amf",
    "obs_qsv11_v2",
    "obs_qsv11_hevc",
    "obs_qsv11_av1",
    "ffmpeg_vaapi",
    "ffmpeg_hevc_vaapi",
    "ffmpeg_av1_vaapi",
];

#[test]
fn diagnostic_status_emits_only_fixed_terms_never_raw_html() {
    let terms = status_terms(Some("<b>Unsupported GPU</b> sensitive-sentinel"));
    assert_eq!(terms, vec!["gpu", "unsupported"]);
    assert!(!format!("{terms:?}").contains("sensitive-sentinel"));
    assert!(status_terms(None).is_empty());
    assert!(status_terms(Some("sensitive-sentinel")).is_empty());
}

#[test]
fn configuration_preserves_temporary_auth_roles_and_private_debug() {
    let value = parse_configuration(
        &response(),
        &preferences(),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap();
    assert_eq!(value.video[0].wire_track, 0);
    assert_eq!(value.audio[0].role, AudioRole::Live);
    assert_eq!(value.audio[1].role, AudioRole::Vod);
    assert_eq!(
        value.target.playpath.expose_for_pipe(),
        b"synthetic-temporary?bandwidthtest=true&clientConfigId=synthetic-config"
    );
    let debug = format!("{value:?}");
    for secret in [
        "synthetic-temporary",
        "synthetic-original",
        "synthetic-config",
        "test.example",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn temporary_authentication_keeps_its_own_query() {
    let bytes = String::from_utf8(response())
        .unwrap()
        .replace(
            "synthetic-temporary\"",
            "synthetic-temporary?temporary=one%2Btwo&flag=active\"",
        )
        .replace("/{stream_key}\"", "/{stream_key}?endpoint=yes\"");
    let configuration = parse_configuration(
        bytes.as_bytes(),
        &preferences(),
        &PublishSecret::new(b"synthetic-original?old=discarded&bandwidthtest=true".to_vec())
            .unwrap(),
        &["test.example".into()],
        &[Codec::H264],
    )
    .unwrap();
    let path = std::str::from_utf8(configuration.target.playpath.expose_for_pipe()).unwrap();
    assert!(path.contains("temporary=one%2Btwo"));
    assert!(path.contains("flag=active"));
    assert!(path.contains("endpoint=yes"));
    assert!(path.contains("bandwidthtest=true"));
    assert!(path.contains("old=discarded"));
    assert!(path.ends_with("clientConfigId=synthetic-config"));
}

#[test]
fn conflicting_or_reserved_temporary_auth_queries_are_rejected() {
    for suffix in ["?bandwidthtest=false", "?clientConfigId=foreign"] {
        let bytes = String::from_utf8(response()).unwrap().replace(
            "synthetic-temporary\"",
            &format!("synthetic-temporary{suffix}\""),
        );
        assert_eq!(
            parse_configuration(
                bytes.as_bytes(),
                &preferences(),
                &PublishSecret::new(b"synthetic-original?bandwidthtest=true".to_vec()).unwrap(),
                &["test.example".into()],
                &[Codec::H264],
            )
            .unwrap_err(),
            GoLiveError::InvalidResponse
        );
    }
}

#[test]
fn probe_error_messages_keep_distinct_static_causes() {
    let errors = [
        GoLiveError::InvalidRequest,
        GoLiveError::Transport,
        GoLiveError::ResponseTooLarge,
        GoLiveError::InvalidResponse,
    ];
    let messages: std::collections::HashSet<_> =
        errors.iter().map(|error| error.message()).collect();
    assert_eq!(messages.len(), errors.len());
    for error in errors {
        assert_eq!(error.to_string(), error.message());
    }
}

#[test]
fn configure_advertises_only_the_proven_encoder_translation() {
    use crate::platform::hardware::EncoderProbe;
    let probes = vec![
        EncoderProbe {
            codec: Codec::H264,
            encoder: "libx264".into(),
            initialized: true,
        },
        EncoderProbe {
            codec: Codec::Hevc,
            encoder: "libx265".into(),
            initialized: true,
        },
        EncoderProbe {
            codec: Codec::Av1,
            encoder: "libsvtav1".into(),
            initialized: true,
        },
    ];
    assert_eq!(configurable_codecs(&probes), vec![Codec::H264]);
    assert!(configurable_codecs(&probes[1..]).is_empty());
}

#[test]
fn malformed_unsupported_or_foreign_config_is_not_publishable() {
    let cases = [
        (
            "\"track_id\":1",
            "\"track_id\":17",
            GoLiveError::UnsupportedAudioMapping,
        ),
        (
            "\"canvas_index\":0",
            "\"canvas_index\":1",
            GoLiveError::InvalidResponse,
        ),
        ("obs_x264", "jim_nvenc", GoLiveError::UnsupportedEncoder),
        ("test.example", "localhost", GoLiveError::EndpointRejected),
        (
            "\"success\"",
            "\"warning\"",
            GoLiveError::WarningNeedsDecision,
        ),
        ("\"success\"", "\"error\"", GoLiveError::AccountRejected),
        (
            "\"rate_control\":\"CBR\"",
            "\"rate_control\":\"CQP\"",
            GoLiveError::UnsupportedEncoder,
        ),
        ("\"bf\":0", "\"bf\":2", GoLiveError::UnsupportedEncoder),
    ];
    for (from, to, expected) in cases {
        let bytes = String::from_utf8(response()).unwrap().replace(from, to);
        let error = parse_configuration(
            bytes.as_bytes(),
            &preferences(),
            &PublishSecret::new(b"public-test".to_vec()).unwrap(),
            &["test.example".into()],
            &[Codec::H264],
        )
        .unwrap_err();
        assert_eq!(error, expected, "case {from}");
    }
}

#[test]
fn safe_probe_report_never_echoes_untrusted_response_text() {
    let response = String::from_utf8(response())
        .unwrap()
        .replace("obs_x264", "secret-reflected-as-encoder")
        .replace(
            "\"result\":\"success\"",
            "\"result\":\"success\",\"html_en_us\":\"secret-reflected-as-html\"",
        );
    let probe = inspect_configuration(response.as_bytes(), 200).unwrap();
    assert_eq!(probe.video[0].encoder, "unknown");
    assert_eq!(probe.live_audio_tracks, 1);
    assert_eq!(probe.vod_audio_tracks, 1);
    let json = serde_json::to_string(&probe).unwrap();
    assert!(!json.contains("secret-reflected"));
    assert!(!json.contains("synthetic-"));
    assert!(!json.contains("test.example"));
}

#[test]
fn request_uses_measured_capabilities_and_real_client_identity() {
    let capabilities = RequestCapabilities {
        gpu: Vec::new(),
        gaming_features: None,
        cpu: Cpu {
            physical_cores: 4,
            logical_cores: 8,
            name: Some("Measured".into()),
            speed: None,
        },
        memory: Memory {
            total: 1024,
            free: 512,
        },
        system: System {
            name: "Linux".into(),
            version: "measured".into(),
            release: "measured".into(),
            revision: "measured".into(),
            bits: 64,
            arm: false,
            build: 0,
            arm_emulation: false,
        },
    };
    let authentication = PublishSecret::new(b"synthetic-authentication".to_vec()).unwrap();
    let bytes = request_body(
        &authentication,
        &capabilities,
        &preferences(),
        &[Codec::H264, Codec::Hevc],
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["client"]["name"], "uplink");
    assert_eq!(
        value["client"]["supported_codecs"],
        serde_json::json!(["h264", "h265"])
    );
    assert_eq!(value["capabilities"]["cpu"]["physical_cores"], 4);
    assert_eq!(value["capabilities"]["gpu"], serde_json::json!([]));
    assert_eq!(value["capabilities"]["system"]["build"], 0);
    assert_eq!(value["capabilities"]["system"]["armEmulation"], false);
    assert!(value["preferences"]["composition_gpu_index"].is_null());
    assert_eq!(value["authentication"], "synthetic-authentication");
    assert_eq!(value["schema_version"], SCHEMA);
}

#[tokio::test]
async fn response_reader_rejects_redirects_oversize_and_interrupted_bodies() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (wire,expected) in [
        ("HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: https://secret.example/path\r\n\r\n".to_owned(),GoLiveError::HttpRejected),
        (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",MAX_RESPONSE+1),GoLiveError::ResponseTooLarge),
        ("HTTP/1.1 200 OK\r\nContent-Length: 200\r\n\r\nsecret-reflected".to_owned(),GoLiveError::Transport),
    ] {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let server=tokio::spawn(async move {
            let (mut socket,_)=listener.accept().await.unwrap();
            let mut request=[0;1024];assert!(socket.read(&mut request).await.unwrap()>0);
            socket.write_all(wire.as_bytes()).await.unwrap();
        });
        let client=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(1)).build().unwrap();
        let response=client.get(format!("http://{address}")).send().await.unwrap();
        let error=read_response(response).await.unwrap_err();
        assert_eq!(error,expected);
        assert!(!format!("{error:?} {error}").contains("secret"));
        server.await.unwrap();
    }
}

#[test]
fn real_response_shape_with_additional_fields_is_accepted() {
    let bytes = build_response(
        &[x264_quality(2560, 1440, 0, fps(60), 6_000)],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let configuration = parse_configuration(
        &bytes,
        &preferences_with(8_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap();
    assert_eq!(configuration.video.len(), 1);
    assert_eq!(configuration.video[0].width, 2560);
    assert_eq!(configuration.video[0].height, 1440);
    assert_eq!(configuration.audio.len(), 2);
}

#[test]
fn audited_hardware_encoder_ids_stay_rejected_not_remapped() {
    for encoder in HARDWARE_ENCODERS {
        let bytes = build_response(
            &[hardware_quality(encoder, 2560, 1440, 6_000)],
            &[audio_track(0, 160)],
            &[audio_track(1, 160)],
        );
        let error = parse_configuration(
            &bytes,
            &preferences_with(8_000, 3),
            &key(),
            &hosts(),
            &[Codec::H264],
        )
        .unwrap_err();
        assert_eq!(error, GoLiveError::UnsupportedEncoder, "case {encoder}");
        let audited = GoLiveEncoder::audited(encoder).unwrap();
        assert_ne!(audited.executable_encoder(), Some("libx264"));
    }
}

#[test]
fn measured_hevc_software_does_not_make_amf_hevc_executable() {
    let bytes = build_response(
        &[hardware_quality("h265_texture_amf", 2560, 1440, 6_000)],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let error = parse_configuration(
        &bytes,
        &preferences_with(8_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264, Codec::Hevc],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::UnsupportedEncoder);
    assert_eq!(
        GoLiveEncoder::audited("h265_texture_amf").unwrap().codec(),
        Codec::Hevc
    );
}

#[test]
fn unknown_encoder_id_is_rejected_and_probe_reports_unknown() {
    let bytes = build_response(
        &[hardware_quality("dream_new_encoder", 2560, 1440, 6_000)],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let error = parse_configuration(
        &bytes,
        &preferences_with(8_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::UnsupportedEncoder);
    let probe = inspect_configuration(&bytes, 200).unwrap();
    assert_eq!(probe.video[0].encoder, "unknown");
}

#[test]
fn multiple_quality_tiers_are_preserved_with_aggregate_limit() {
    let bytes = build_response(
        &[
            x264_quality(2560, 1440, 0, fps(60), 6_000),
            x264_quality(1280, 720, 0, fps(60), 2_500),
            x264_quality(854, 480, 0, fps(30), 800),
        ],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let configuration = parse_configuration(
        &bytes,
        &preferences_with(12_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap();
    assert_eq!(configuration.video.len(), 3);
    let tracks: Vec<u8> = configuration.video.iter().map(|v| v.wire_track).collect();
    assert_eq!(tracks, vec![0, 1, 2]);
    assert_eq!(configuration.video[2].framerate, fps(30));
    assert_eq!(configuration.video[2].bitrate_kbps, 800);
    assert_eq!(configuration.audio[0].role, AudioRole::Live);
    assert_eq!(configuration.audio[0].wire_track, 0);
    assert_eq!(configuration.audio[1].role, AudioRole::Vod);
    assert_eq!(configuration.audio[1].wire_track, 1);
}

#[test]
fn two_canvases_keep_index_and_framerate_mapping() {
    let bytes = build_response(
        &[
            x264_quality(2560, 1440, 0, fps(60), 6_000),
            x264_quality(1080, 1920, 1, fps(30), 3_000),
        ],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let configuration = parse_configuration(
        &bytes,
        &dual_canvas_preferences(),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap();
    assert_eq!(configuration.video[0].canvas_index, 0);
    assert_eq!(configuration.video[0].framerate, fps(60));
    assert_eq!(configuration.video[1].canvas_index, 1);
    assert_eq!(configuration.video[1].width, 1080);
    assert_eq!(configuration.video[1].height, 1920);
    assert_eq!(configuration.video[1].framerate, fps(30));
    let outside = build_response(
        &[x264_quality(2560, 1440, 2, fps(60), 6_000)],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let error = parse_configuration(
        &outside,
        &dual_canvas_preferences(),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::InvalidResponse);
}

#[test]
fn aggregate_bitrate_and_track_limits_are_enforced() {
    let over_aggregate = build_response(
        &[
            x264_quality(2560, 1440, 0, fps(60), 6_000),
            x264_quality(1280, 720, 0, fps(60), 2_500),
        ],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let error = parse_configuration(
        &over_aggregate,
        &preferences_with(8_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::InvalidResponse);
    let over_tracks = build_response(
        &[
            x264_quality(2560, 1440, 0, fps(60), 1_000),
            x264_quality(1280, 720, 0, fps(60), 1_000),
            x264_quality(854, 480, 0, fps(30), 1_000),
            x264_quality(640, 360, 0, fps(30), 1_000),
        ],
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let error = parse_configuration(
        &over_tracks,
        &preferences_with(20_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::InvalidResponse);
    let mut audio = Vec::new();
    for index in 0..17u32 {
        audio.push(audio_track(index, 160));
    }
    let over_audio = build_response(
        &[x264_quality(2560, 1440, 0, fps(60), 6_000)],
        &audio,
        &[audio_track(17, 160)],
    );
    let error = parse_configuration(
        &over_audio,
        &preferences_with(20_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::UnsupportedAudioMapping);
    let over_schema = build_response(
        &(0..17)
            .map(|index| x264_quality(2560, 1440, 0, fps(60), u32::try_from(index).unwrap() + 1))
            .collect::<Vec<_>>(),
        &[audio_track(0, 160)],
        &[audio_track(1, 160)],
    );
    let error = parse_configuration(
        &over_schema,
        &preferences_with(100_000, 16),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::InvalidResponse);
}

#[test]
fn contradictory_audio_track_ids_are_rejected() {
    let bytes = String::from_utf8(response())
        .unwrap()
        .replace("\"track_id\":0", "\"track_id\":2");
    let error = parse_configuration(
        bytes.as_bytes(),
        &preferences_with(12_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap_err();
    assert_eq!(error, GoLiveError::UnsupportedAudioMapping);
}

#[test]
fn status_results_map_to_visible_errors() {
    for (result, expected) in [
        ("warning", GoLiveError::WarningNeedsDecision),
        ("error", GoLiveError::AccountRejected),
        ("paused", GoLiveError::InvalidResponse),
    ] {
        let bytes = String::from_utf8(response()).unwrap().replace(
            "\"result\":\"success\"",
            &format!("\"result\":\"{result}\""),
        );
        assert_eq!(
            parse_configuration(
                bytes.as_bytes(),
                &preferences(),
                &key(),
                &hosts(),
                &[Codec::H264],
            )
            .unwrap_err(),
            expected,
            "case {result}"
        );
    }
}

#[test]
fn x264_core_settings_stay_strict() {
    for (from, to) in [
        ("\"rate_control\":\"CBR\"", "\"rate_control\":\"VBR\""),
        ("\"bf\":0", "\"bf\":3"),
        ("\"preset\":\"veryfast\"", "\"preset\":\"p5\""),
        ("\"tune\":\"zerolatency\"", "\"tune\":\"hq\""),
        ("\"keyint_sec\":2", "\"keyint_sec\":8"),
        ("\"profile\":\"high\"", "\"profile\":\"main10\""),
        ("\"bitrate\":6000", "\"bitrate\":20001"),
        ("\"x264opts\":\"\"", "\"x264opts\":\"log-level=debug\""),
    ] {
        let bytes = String::from_utf8(build_response(
            &[x264_quality(2560, 1440, 0, fps(60), 6_000)],
            &[audio_track(0, 160)],
            &[audio_track(1, 160)],
        ))
        .unwrap()
        .replace(from, to);
        let error = parse_configuration(
            bytes.as_bytes(),
            &preferences_with(8_000, 3),
            &key(),
            &hosts(),
            &[Codec::H264],
        )
        .unwrap_err();
        assert_eq!(error, GoLiveError::UnsupportedEncoder, "case {from}");
    }
}

#[test]
fn probe_preserves_audited_encoder_names() {
    let bytes = build_response(
        &[
            hardware_quality("obs_nvenc_h264_tex", 1920, 1080, 6_000),
            x264_quality(1280, 720, 0, fps(60), 2_500),
        ],
        &[audio_track(0, 160)],
        &[],
    );
    let probe = inspect_configuration(&bytes, 200).unwrap();
    assert_eq!(probe.configuration_status, "success");
    assert_eq!(probe.video[0].encoder, "obs_nvenc_h264_tex");
    assert_eq!(probe.video[1].encoder, "obs_x264");
    assert_eq!(probe.live_audio_tracks, 1);
    assert_eq!(probe.vod_audio_tracks, 0);
    assert!(probe.rtmps_endpoint_available);
    assert!(!probe.arbitrary_audio_ids);
}

#[test]
fn twitch_encoder_identity_is_preserved_for_integration_audit() {
    let configuration = parse_configuration(
        &response(),
        &preferences(),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap();
    assert_eq!(configuration.encoders, vec![GoLiveEncoder::ObsX264]);
    let tiers = parse_configuration(
        &build_response(
            &[
                x264_quality(2560, 1440, 0, fps(60), 6_000),
                x264_quality(1280, 720, 0, fps(60), 2_500),
            ],
            &[audio_track(0, 160)],
            &[audio_track(1, 160)],
        ),
        &preferences_with(12_000, 3),
        &key(),
        &hosts(),
        &[Codec::H264],
    )
    .unwrap();
    assert_eq!(
        tiers.encoders,
        vec![GoLiveEncoder::ObsX264, GoLiveEncoder::ObsX264]
    );
    assert!(format!("{configuration:?}").contains("ObsX264"));
}
