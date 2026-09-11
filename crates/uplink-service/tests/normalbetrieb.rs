use std::{sync::Arc, time::Duration};
#[path = "support/database.rs"]
mod database;
#[path = "../../uplink-ingest/tests/support/tls.rs"]
mod tls;
use database::fixture;
/// Derselbe normale Dienst und Coordinator wie beim Betriebsstart, mit eigener
/// öffentlicher Test-CA auf Loopback und ausschließlich synthetischen Zugängen.
#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und den installierten geprüften FFmpeg-8-Build."]
async fn normal_coordinator_keeps_healthy_output_and_reports_real_graph() {
    normal_audio_case(false, None, false).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn normal_single_aac_live_mode_reaches_twitch_compatible_output() {
    normal_audio_case(true, Some("live"), false).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn twitch_without_enhanced_profiles_sends_desired_output_and_stays_alive() {
    normal_case_mode(true, Some("live"), false, None, None, true, false).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn enhanced_without_measured_profiles_falls_back_after_valid_publish_grant() {
    normal_case_mode(true, Some("live"), false, None, None, true, true).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn enhanced_fallback_preserves_distinct_live_and_vod_audio() {
    normal_case_mode(false, Some("separate_vod"), false, None, None, true, true).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn missing_separate_vod_audio_stops_only_twitch() {
    normal_audio_case(true, Some("separate_vod"), false).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn explicit_separate_vod_preserves_both_distinct_aac_feeds() {
    normal_audio_case(false, Some("separate_vod"), false).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn rejected_platform_publish_marks_the_entire_failed_session() {
    normal_audio_case(true, Some("live"), true).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn production_twitch_with_profile_zero_capacity_and_no_generation_stays_blocked() {
    normal_case(true, None, false, Some(("11", 0)), None).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn production_twitch_with_stale_generation_preserves_healthy_youtube() {
    normal_case(true, None, false, Some(("11", 2)), None).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn production_twitch_with_wrong_token_owner_preserves_healthy_youtube() {
    normal_case(true, None, false, Some(("12", 3)), None).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn malformed_layout_with_portrait_disabled_preserves_healthy_youtube() {
    normal_case(true, None, false, None, Some(false)).await;
}

#[tokio::test]
#[ignore = "Benötigt isolierte PostgreSQL 16 und geprüften FFmpeg 8."]
async fn malformed_layout_with_portrait_enabled_stops_only_twitch() {
    normal_case(true, None, false, Some(("11", 3)), Some(true)).await;
}

async fn normal_audio_case(single_audio: bool, twitch_mode: Option<&str>, all_failed: bool) {
    normal_case(single_audio, twitch_mode, all_failed, None, None).await;
}

async fn av1_bt709_fixture(ffmpeg: &std::path::Path) -> Vec<u8> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(ffmpeg)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "flv",
            "-i",
            "pipe:0",
            "-map",
            "0",
            "-c:a",
            "copy",
            "-c:v",
            "libsvtav1",
            "-flags",
            "+global_header",
            "-preset",
            "11",
            "-svtav1-params",
            "lp=2",
            "-crf",
            "35",
            "-b:v",
            "0",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-f",
            "flv",
            "-flvflags",
            "no_metadata+no_duration_filesize",
            "pipe:1",
        ])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let write = tokio::spawn(async move {
        stdin
            .write_all(include_bytes!(
                "../../../experiments/scuffle-probe/fixtures/av1.flv"
            ))
            .await
            .unwrap();
    });
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    write.await.unwrap();
    assert!(output.status.success());
    output.stdout
}

async fn normal_case(
    single_audio: bool,
    twitch_mode: Option<&str>,
    all_failed: bool,
    production: Option<(&'static str, i64)>,
    malformed_layout: Option<bool>,
) {
    normal_case_mode(
        single_audio,
        twitch_mode,
        all_failed,
        production,
        malformed_layout,
        false,
        false,
    )
    .await;
}

async fn normal_case_mode(
    single_audio: bool,
    twitch_mode: Option<&str>,
    all_failed: bool,
    production: Option<(&'static str, i64)>,
    malformed_layout: Option<bool>,
    unmeasured_twitch: bool,
    enhanced_fallback: bool,
) {
    use futures::FutureExt;
    use sha2::{Digest, Sha256};
    use uplink_ingest::{
        AuthorizedSession, Authorizer, EventKind, IngestLimits, IngestServer, MediaKind,
    };
    use uplink_media::{
        MediaLimits, PublishSecret, PublishTarget, flv::FlvReader, pusher::RunningPusher,
    };
    const OUTPUT_KEY: &str = "synthetic-full-key?keep=this-entire-value";
    let twitch_missing = single_audio && twitch_mode == Some("separate_vod");
    let healthy_platform = if twitch_mode.is_some() && !twitch_missing {
        "twitch"
    } else {
        "youtube"
    };
    struct DestinationAuth {
        reject: bool,
    }
    impl Authorizer for DestinationAuth {
        async fn authorize(&self, app: &str, key: &str) -> Result<AuthorizedSession, ()> {
            if !self.reject && app == "live" && key == OUTPUT_KEY {
                AuthorizedSession::new(1, 1).map_err(|_| ())
            } else {
                Err(())
            }
        }
    }
    let (database, mut state) = fixture().await;
    let broker_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let broker = if let Some((owner, _)) = production.or(enhanced_fallback.then_some(("11", 3))) {
        let calls = broker_calls.clone();
        let app = axum::Router::new().route(
            "/twitch/api/v2/internal/platform-token",
            axum::routing::get(move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>, headers: axum::http::HeaderMap| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    assert_eq!(query.get("streamer").map(String::as_str), Some("11"));
                    assert_eq!(query.get("platform").map(String::as_str), Some("twitch"));
                    assert_eq!(query.get("purpose").map(String::as_str), Some("publish"));
                    assert!(headers.contains_key("X-Internal-Token"));
                    axum::Json(serde_json::json!({"purpose":"publish","token_owner":owner,"platform_user_id":"11","scopes":["channel:read:stream_key"],"access_token":"synthetic-test","connection_generation":3}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Some((address, handle))
    } else {
        None
    };
    let certificates = tls::test_tls();
    let output = IngestServer::bind_loopback(
        0,
        certificates.server.clone(),
        Arc::new(DestinationAuth { reject: all_failed }),
        IngestLimits::local_probe(),
    )
    .await
    .unwrap();
    let output_address = output.local_addr().unwrap();
    let config = &mut Arc::get_mut(&mut state).unwrap().config;
    if let Some((address, _)) = &broker {
        config.chat.as_mut().unwrap().bot_base_url = format!("http://{address}");
    } else {
        config.chat = None;
    }
    config.media.work_directory = database.directory.join("media");
    let ca_path = database.directory.join("public-test-ca.pem");
    std::fs::write(&ca_path, &certificates.certificate_pem).unwrap();
    config.loopback_test_ca = Some(ca_path);
    if let Some((_, generation)) = production {
        config.media.enhanced.capacity_units = if generation == 0 { 0 } else { 2 };
        config.media.enhanced.profiles = vec![uplink_service::config::CapacityProfile {
            key: "synthetic-measured-profile".into(),
            units: 1,
        }];
    }
    config.platforms.retain(|platform| {
        platform.name == healthy_platform
            || ((twitch_missing || production.is_some()) && platform.name == "twitch")
    });
    for policy in &mut config.platforms {
        policy.allowed_hosts = vec![if production.is_some() && policy.name == "twitch" {
            "ingest.example".into()
        } else {
            "localhost".into()
        }];
        if unmeasured_twitch && policy.name == "twitch" {
            policy.allowed_hosts.push("ingest.example".into());
        }
        policy.use_vod_audio = policy.name == "twitch" || !single_audio;
    }
    let valid = state
        .secrets
        .encryption
        .seal(
            OUTPUT_KEY.as_bytes(),
            &format!("destination:11:{healthy_platform}"),
        )
        .unwrap();
    let endpoint = format!("rtmps://localhost:{}/live", output_address.port());
    state.store.query("INSERT INTO relay.destinations VALUES(11,'kick','rtmps://missing.invalid/live',$1,true,256,144,25,500),(11,$2,$3,$4,true,256,144,25,500)", &[&vec![0u8], &healthy_platform, &endpoint, &valid]).await.unwrap();
    if twitch_missing {
        let key = state
            .secrets
            .encryption
            .seal(OUTPUT_KEY.as_bytes(), "destination:11:twitch")
            .unwrap();
        state
            .store
            .query(
                "INSERT INTO relay.destinations VALUES(11,'twitch',$1,$2,true,256,144,25,500)",
                &[&endpoint, &key],
            )
            .await
            .unwrap();
    }
    if let Some((_, generation)) = production {
        let key = state
            .secrets
            .encryption
            .seal(OUTPUT_KEY.as_bytes(), "destination:11:twitch")
            .unwrap();
        state.store.query("INSERT INTO relay.destinations(streamer_id,platform,rtmp_url,stream_key_enc,enabled,width,height,fps,bitrate_kbps,twitch_audio_mode) VALUES(11,'twitch','rtmps://ingest.example/live',$1,true,256,144,25,500,'live')", &[&key]).await.unwrap();
        state.store.query("UPDATE relay.destinations SET twitch_output_mode='enhanced' WHERE streamer_id=11 AND platform='twitch'", &[]).await.unwrap();
        if generation > 0 {
            state.store.query("INSERT INTO relay.destination_fences(streamer_id,platform,generation,deleted) VALUES(11,'twitch',$1,false)", &[&generation]).await.unwrap();
        }
    }
    if let Some(mode) = twitch_mode {
        use tower::ServiceExt;
        let request = axum::http::Request::builder().method("PUT").uri("/v1/me/destinations")
            .header("X-Relay-Auth","synthetic-api").header("Content-Type","application/json")
            .body(axum::body::Body::from(serde_json::json!({"streamer_id":11,"destinations":[{"platform":"twitch","twitch_audio_mode":mode}]}).to_string())).unwrap();
        assert_eq!(
            uplink_service::api::router(state.clone())
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            axum::http::StatusCode::OK
        );
    }
    if enhanced_fallback {
        state.store.query("UPDATE relay.destinations SET twitch_output_mode='enhanced' WHERE streamer_id=11 AND platform='twitch'", &[]).await.unwrap();
        state.store.query("INSERT INTO relay.destination_fences(streamer_id,platform,generation,deleted) VALUES(11,'twitch',3,false) ON CONFLICT(streamer_id,platform) DO UPDATE SET generation=3", &[]).await.unwrap();
    }
    if let Some(enabled) = malformed_layout {
        state
            .store
            .query(
                "INSERT INTO relay.hochkant_layouts(streamer_id,revision,layout) VALUES(11,1,'{}')",
                &[],
            )
            .await
            .unwrap();
        if enabled {
            state.store.query("UPDATE relay.destinations SET hochkant_enabled=true,hochkant_width=144,hochkant_height=256 WHERE streamer_id=11 AND platform='twitch'", &[]).await.unwrap();
        }
    }
    let coordinator = Arc::new(uplink_service::media::Coordinator::new(state.clone()).unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let (ready, bound) = tokio::sync::oneshot::channel();
    let runtime_state = state.clone();
    let runtime_tls = certificates.server.clone();
    let mut runtime = tokio::spawn(async move {
        uplink_service::runtime::serve_with_ready(
            runtime_state,
            runtime_tls,
            coordinator,
            async {
                let _ = stopped.await;
            },
            Some(ready),
        )
        .await
    });
    let (api, input) = bound.await.unwrap();
    let mut receiving = tokio::spawn(async move {
        let mut connection = output.accept().await.unwrap();
        let mut packets = std::collections::BTreeMap::<u8, Vec<(u32, String)>>::new();
        let mut video = 0;
        while let Some(event) = connection.next().await {
            if event.event_kind != EventKind::Frame {
                continue;
            }
            match event.identity.track.kind {
                MediaKind::Video => {
                    assert_eq!(event.codec, uplink_ingest::WireCodec::H264);
                    video += 1;
                }
                MediaKind::Audio => packets
                    .entry(event.identity.track.wire_id)
                    .or_default()
                    .push((
                        event.dts_ms,
                        format!("SHA256:{:x}", Sha256::digest(event.payload())),
                    )),
            }
        }
        let report = connection.finish().await;
        (video, packets, report)
    });
    let result = std::panic::AssertUnwindSafe(async {
        let producer = RunningPusher::start(PublishTarget { id:"synthetic-obs".into(), endpoint:format!("rtmps://localhost:{}/live",input.port()), playpath:PublishSecret::new(b"rsr_00000000000000000000000000000000".to_vec()).unwrap(), tls:Some(certificates.client.clone()), allowed_hosts:vec!["localhost".into()], allow_loopback:true, allow_unencrypted:false }, MediaLimits::default()).await.unwrap();
        let av1_fixture;
        let source_bytes = if unmeasured_twitch {
            av1_fixture = av1_bt709_fixture(&state.config.media.ffmpeg).await;
            av1_fixture.as_slice()
        } else {
            &include_bytes!("../../../experiments/scuffle-probe/fixtures/h264.flv")[..]
        };
        let mut source = FlvReader::new(source_bytes, 65536);
        while let Some(tag) = source.next().await.unwrap() {
            if single_audio && tag.audio_track().unwrap() == Some(1) { continue; }
            producer.try_send(Arc::new(tag)).unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        // Quelle bleibt offen, damit laufender Graph und Endzustand getrennt
        // über genau die normalen autorisierten HTTP-Routen beobachtet werden.
        let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status: serde_json::Value = http.get(format!("http://{api}/v1/me/destinations?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
                let healthy = status["destinations"].as_array().unwrap().iter().find(|item| item["platform"]==healthy_platform).unwrap();
                if all_failed && healthy["output_state"] == "failed" {
                    assert!(healthy["active_profile"].is_null());
                    break;
                }
                if healthy["output_state"]=="sending" {
                    assert_eq!(healthy["active_profile"]["width"],256);
                    assert_eq!(healthy["active_profile"]["height"],144);
                    assert_eq!(healthy["active_profile"]["profile_origin"],"running_graph");
                    assert_eq!(healthy["publication_confirmed"],false);
                    if healthy_platform == "twitch" {
                        assert_eq!(healthy["active_audio_mode"],twitch_mode.unwrap());
                        let routes = healthy["active_audio_routes"].as_array().expect("laufende Twitch-Audiorouten");
                        assert_eq!(routes[0]["source_wire_track"],0);
                        assert_eq!(routes[0]["destination_wire_track"],0);
                        assert_eq!(routes[0]["role"],"live");
                        if twitch_mode == Some("separate_vod") {
                            assert_eq!(routes.len(),2);
                            assert_eq!(routes[1]["source_wire_track"],1);
                            assert_eq!(routes[1]["destination_wire_track"],1);
                            assert_eq!(routes[1]["role"],"vod");
                        } else {
                            assert_eq!(routes.len(),1);
                        }
                    }
                    if unmeasured_twitch {
                        let sessions = state.registry.status(11);
                        let mode = &sessions[0].output_modes["twitch"];
                        assert_eq!(mode.requested, if enhanced_fallback { uplink_service::destinations::TwitchOutputMode::Enhanced } else { uplink_service::destinations::TwitchOutputMode::Single });
                        assert_eq!(mode.active, Some(uplink_service::destinations::TwitchOutputMode::Single));
                        assert_eq!(mode.fallback_reason.is_some(), enhanced_fallback);
                        assert_eq!(broker_calls.load(std::sync::atomic::Ordering::SeqCst), usize::from(enhanced_fallback));
                    }
                    if twitch_missing {
                        let twitch = status["destinations"].as_array().unwrap().iter().find(|item| item["platform"]=="twitch").unwrap();
                        assert_eq!(twitch["output_state"],"failed");
                        assert!(twitch["reason"].as_str().unwrap().contains("Medienspur fehlt"));
                        assert!(twitch["active_audio_mode"].is_null());
                        assert_eq!(twitch["twitch_audio_mode"],"separate_vod");
                    }
                    if let Some((owner, _)) = production {
                        let twitch = status["destinations"].as_array().unwrap().iter().find(|item| item["platform"]=="twitch").unwrap();
                        assert_eq!(twitch["output_state"],"failed");
                        assert!(twitch["reason"].as_str().unwrap().contains(if malformed_layout == Some(true) {"Hochkantwahl"} else if owner == "11" {"Twitch-Verbindung"} else {"Kontoinhaber"}));
                        assert!(twitch["active_profile"].is_null());
                        assert_eq!(broker_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                    }
                    let rejected = &status["destinations"][0];
                    assert_eq!(rejected["platform"],"kick");
                    assert_eq!(rejected["output_state"],"failed");
                    assert!(rejected["reason"].as_str().unwrap().contains("Konfiguration"));
                    break;
                }
                assert!(state.registry.status(11).first().is_none_or(|session| session.error.is_none()), "Coordinator darf gesunden Ausgang nicht wegen eines anderen Ziels abbrechen: {:?}", serde_json::to_value(state.registry.status(11)).unwrap());
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }).await.expect("Gesunder Ausgang muss rechtzeitig Medien liefern");
        if unmeasured_twitch {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let session = state.registry.status(11);
            assert!(session[0].active);
            assert!(session[0].error.is_none());
            assert!(!receiving.is_finished());
        }
        let me:serde_json::Value=http.get(format!("http://{api}/v1/me?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
        assert_eq!(me["session"]["source_observation"]["width"],320);
        assert_eq!(me["session"]["source_observation"]["height"],180);
        assert_eq!(me["session"]["source_observation"]["audio"].as_array().unwrap().len(),if single_audio {1} else {2});
        assert_eq!(me["session"]["outputs"]["encode_groups"],1);
        if twitch_mode == Some("live") && !all_failed {
            let response = http.put(format!("http://{api}/v1/me/destinations")).header("X-Relay-Auth","synthetic-api")
                .json(&serde_json::json!({"streamer_id":11,"destinations":[{"platform":"twitch","twitch_audio_mode":"separate_vod","connection_generation":if enhanced_fallback {3} else {0}}]})).send().await.unwrap();
            let status = response.status();
            let detail: serde_json::Value = response.json().await.unwrap();
            assert!(status.is_success(), "Audioänderung: {status} {:?}", detail.get("error"));
            let changed:serde_json::Value = http.get(format!("http://{api}/v1/me/destinations?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
            let twitch = changed["destinations"].as_array().unwrap().iter().find(|item|item["platform"]=="twitch").unwrap();
            assert_eq!(twitch["twitch_audio_mode"],"separate_vod");
            assert_eq!(twitch["effective_audio_mode"],"separate_vod");
            assert_eq!(twitch["active_audio_mode"],"live","Speichern darf laufendes Audio nicht still umschalten");
            if enhanced_fallback {
                assert_eq!(twitch["requested_output_mode"], "enhanced");
                assert_eq!(twitch["active_output_mode"], "single");
                assert!(twitch["fallback_reason"].is_string());
            }
        }
        producer.finish().await.unwrap();
        let (video, audio, _report) = tokio::time::timeout(Duration::from_secs(15), &mut receiving).await.unwrap().unwrap();
        assert_eq!(video,if all_failed { 0 } else { 50 });
        let reference:serde_json::Value=serde_json::from_str(if unmeasured_twitch {
            include_str!("../../../experiments/scuffle-probe/fixtures/av1.ffprobe.json")
        } else {
            include_str!("../../../experiments/scuffle-probe/fixtures/h264.ffprobe.json")
        }).unwrap();
        assert_eq!(audio.len(),if all_failed {0} else if single_audio {1} else {2},"Keine erfundene VOD-Ersatzspur");
        for wire in 0..if all_failed {0} else if single_audio {1u8} else {2} {
            let expected:Vec<_>=reference["packets"].as_array().unwrap().iter().filter(|packet| packet["stream_index"]==u64::from(wire)+1).map(|packet|(packet["dts"].as_u64().unwrap() as u32,packet["data_hash"].as_str().unwrap().to_owned())).collect();
            assert_eq!(audio[&wire],expected,"AAC-Rolle und Zeitlinie müssen bytegenau stimmen");
        }
        tokio::time::timeout(Duration::from_secs(3),async {while state.registry.active_count()!=0 {tokio::time::sleep(Duration::from_millis(20)).await;}}).await.unwrap();
        assert_eq!(state.registry.status(11)[0].ingest_end_reason.as_deref(),Some("ExplicitStop"));
        if all_failed {
            assert_eq!(state.registry.status(11)[0].state, "Fehler", "Alle Plattformausgänge sind fehlgeschlagen; die Session darf nicht erfolgreich beendet heißen");
            assert!(state.registry.status(11)[0].error.is_some());
        }
        let ended:serde_json::Value=http.get(format!("http://{api}/v1/me?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
        assert!(ended["session"].is_null(), "Ein beendeter Eingang darf nicht als empfangende Session erscheinen");
        let finished:serde_json::Value=http.get(format!("http://{api}/v1/me/destinations?streamer_id=11")).header("X-Relay-Auth","synthetic-api").send().await.unwrap().json().await.unwrap();
        let healthy = finished["destinations"].as_array().unwrap().iter().find(|item| item["platform"]==healthy_platform).unwrap();
        assert_eq!(healthy["output_state"],if all_failed {"failed"} else {"finished"});
        assert!(healthy["active_profile"].is_null());
        assert!(healthy["active_audio_mode"].is_null());
    }).catch_unwind();
    let result = tokio::time::timeout(Duration::from_secs(35), result).await;
    if !receiving.is_finished() {
        receiving.abort();
        let _ = receiving.await;
    }
    let _ = stop.send(());
    if tokio::time::timeout(Duration::from_secs(12), &mut runtime)
        .await
        .is_err()
    {
        runtime.abort();
        let _ = runtime.await;
    }
    drop(state);
    if let Some((_, handle)) = broker {
        handle.abort();
        let _ = handle.await;
    }
    database.stop().await;
    assert!(
        matches!(result, Ok(Ok(()))),
        "Gekoppelter Normalbetrieb fehlgeschlagen"
    );
}
