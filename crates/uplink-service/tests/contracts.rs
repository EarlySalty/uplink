use uplink_service::{config::Config, crypto::Secret, registry::Registry};

#[test]
fn advertised_dock_addresses_require_an_usable_public_origin() {
    let example = include_str!("../../../config/uplink-beispiel.toml");
    for address in [
        "https://",
        "https://user:password@example.invalid/uplink",
        "https://example.invalid/uplink?token=unused",
        "https://example.invalid/uplink#fragment",
    ] {
        let mut config: toml::Value = toml::from_str(example).unwrap();
        config["dock_base_url"] = toml::Value::String(address.into());
        assert!(
            Config::parse(&toml::to_string(&config).unwrap()).is_err(),
            "Ungültige Dockadresse darf keinen erfolgreichen Konfigurationscheck erhalten"
        );
    }
}

#[test]
fn parser_allocations_are_part_of_the_global_ingest_budget() {
    let mut config: toml::Value =
        toml::from_str(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    config["max_sessions"] = 64.into();
    config["media"]["max_event_bytes"] = 0xff_ffff.into();
    config["media"]["max_queued_bytes"] = (0xff_ffff + 15).into();
    assert_eq!(
        Config::parse(&toml::to_string(&config).unwrap()).err(),
        Some("Parser und Medienpuffer überschreiten das gemeinsame Eingangsbudget."),
        "Gültige Einzelpaketgrenzen dürfen das globale Parserbudget nicht umgehen"
    );
}

#[test]
fn root_only_destinations_have_no_publishable_app() {
    for address in [
        "rtmps://live.twitch.tv/",
        "rtmps://live.twitch.tv",
        "rtmp://example.invalid/",
    ] {
        assert!(uplink_service::destinations::public_endpoint(address).is_err());
    }
}

#[test]
fn media_engine_and_ingest_use_the_same_explicit_byte_and_event_limits() {
    let mut config = Config::parse(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    config.media.max_event_bytes = 4 * 1024 * 1024;
    config.media.max_queued_bytes = 12 * 1024 * 1024;
    config.media.max_queued_events = 1024;
    let media = config.media_limits();
    let ingest = config.ingest_limits().unwrap();
    assert_eq!(media.max_tag_bytes, ingest.max_event_bytes);
    assert_eq!(media.queue_bytes, ingest.max_queued_bytes);
    assert_eq!(media.queue_events, ingest.max_queued_events);
    assert_eq!(ingest.max_connections, config.max_sessions);
    assert_eq!(
        ingest.max_pending_connections,
        config.max_pending_connections
    );
}

fn assert_packet_limit_contract(tag_bytes: usize, queue_bytes: usize, accepted: bool) {
    let mut input: toml::Value =
        toml::from_str(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    input["max_sessions"] = 1.into();
    input["max_pending_connections"] = 1.into();
    input["media"]["max_event_bytes"] = (tag_bytes as i64).into();
    input["media"]["max_queued_bytes"] = (queue_bytes as i64).into();
    let parsed = Config::parse(&toml::to_string(&input).unwrap());
    assert_eq!(
        parsed.is_ok(),
        accepted,
        "Paketgrenze {tag_bytes}, Queuegrenze {queue_bytes}"
    );
    if let Ok(config) = parsed {
        let limits = config.media_limits();
        assert_eq!(limits.max_tag_bytes, tag_bytes);
        assert_eq!(
            limits.queue_bytes, queue_bytes,
            "Keine stille Budgeterhöhung"
        );
        let ingest = config.ingest_limits().unwrap();
        assert_eq!(ingest.max_event_bytes, tag_bytes);
        assert_eq!(ingest.max_queued_bytes, queue_bytes);
        ingest.rtmp.validate().unwrap();
        // Der Konstruktor startet keine Prozesse. Ein vorhandenes Testbinary
        // genügt für seine Dateiprüfung; kein lokaler FFmpeg-Pfad ist nötig.
        let executable = std::env::current_exe().unwrap();
        assert!(
            uplink_media::MediaEngine::new(uplink_media::EngineConfig {
                ffmpeg: executable.clone(),
                ffprobe: executable,
                work_directory: "/unused-uplink-limit-test".into(),
                limits,
            })
            .is_ok()
        );
    }
}

#[test]
fn packet_limits_require_room_for_the_complete_flv_frame() {
    let payload = 2 * 1024 * 1024;
    for overhead in [0, 14, 15, 16] {
        assert_packet_limit_contract(payload, payload + overhead, overhead >= 15);
    }
}

#[test]
fn packet_limits_keep_the_exact_24_bit_wire_boundary() {
    for tag in [0xff_fffe, 0xff_ffff, 0x100_0000] {
        assert_packet_limit_contract(tag, tag + 15, tag <= 0xff_ffff);
    }
}

#[test]
fn isolated_ingest_scope_requires_one_positive_explicit_identity() {
    let example = include_str!("../../../config/uplink-beispiel.toml");
    let valid = format!("{example}\n[test_ingest]\nallowed_streamer_ids = [11]\n");
    assert!(Config::parse(&valid).is_ok());
    for ids in ["[]", "[0]", "[11, 12]", "[18446744073709551615]"] {
        assert!(Config::parse(&valid.replace("[11]", ids)).is_err());
    }
}

#[test]
fn normal_binary_refuses_input_only_configuration_before_secret_access() {
    let example = include_str!("../../../config/uplink-beispiel.toml");
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).unwrap();
    let path =
        std::env::temp_dir().join(format!("uplink-normal-start-{}.toml", hex::encode(bytes)));
    std::fs::write(
        &path,
        format!("{example}\n[test_ingest]\nallowed_streamer_ids=[11]\n"),
    )
    .unwrap();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("uplink-service");
    let output = std::process::Command::new(binary)
        .arg("--config")
        .arg(&path)
        .output();
    std::fs::remove_file(path).unwrap();
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "Ein reiner Testeingang darf nicht als regulärer Uplink-Dienst starten."
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn public_ingest_never_accepts_private_test_roots_or_embedded_credentials() {
    let example = include_str!("../../../config/uplink-beispiel.toml");
    let local = format!("loopback_test_ca = \"/tmp/public-ca.pem\"\n{example}");
    assert!(Config::parse(&local).is_ok());
    let mut public: toml::Value = toml::from_str(&local).unwrap();
    public["ingest_bind"] = toml::Value::String("0.0.0.0:1935".into());
    assert!(Config::parse(&toml::to_string(&public).unwrap()).is_err());
    for bad in [
        "rtmps://synthetic:key@example.org/live",
        "rtmps://example.org/live?key=synthetic",
        "rtmps://example.org/live/synthetic",
    ] {
        let mut config: toml::Value = toml::from_str(example).unwrap();
        config["public_ingest_url"] = toml::Value::String(bad.into());
        assert!(Config::parse(&toml::to_string(&config).unwrap()).is_err());
    }
}

#[test]
fn rejects_public_controlplane_and_unknown_configuration() {
    let example = include_str!("../../../config/uplink-beispiel.toml");
    assert!(Config::parse(example).is_ok());
    assert!(Config::parse(&example.replace("127.0.0.1:8892", "0.0.0.0:8892")).is_err());
    assert!(Config::parse(&format!("unknown = true\n{example}")).is_err());
}

#[test]
fn per_tenant_reservation_releases_on_drop_and_bounds_global_work() {
    let registry = Registry::new(2, 1).unwrap();
    let first = registry.reserve(11).unwrap();
    assert!(registry.reserve(11).is_err());
    let second = registry.reserve(12).unwrap();
    assert!(registry.reserve(13).is_err());
    drop(first);
    let replacement = registry.reserve(11).unwrap();
    assert_ne!(replacement.id(), second.id());
    drop(second);
    drop(replacement);
    assert_eq!(registry.active_count(), 0);
}

#[test]
fn destination_change_excludes_admission_until_every_database_owner_releases_it() {
    let registry = Registry::new(2, 1).unwrap();
    let change = registry.begin_change(11).unwrap();
    let database_owner = change.clone();
    assert!(registry.reserve(11).is_err());
    assert!(registry.reserve(12).is_ok());
    drop(change);
    assert!(registry.reserve(11).is_err());
    drop(database_owner);
    let active = registry.reserve(11).unwrap();
    assert!(registry.begin_change(11).is_err());
    drop(active);
    assert!(registry.begin_change(11).is_ok());
}

#[test]
fn authorization_comparison_is_exact_and_debug_does_not_expose_secret() {
    let secret = Secret::new(b"test-secret-material".to_vec());
    assert!(secret.matches(b"test-secret-material"));
    assert!(!secret.matches(b"test-secret-material-extra"));
    assert!(!format!("{secret:?}").contains("test-secret-material"));
}

#[test]
fn encrypted_destination_is_bound_to_tenant_and_platform() {
    let key = Secret::new(vec![4; 32]);
    let sealed = key
        .seal(b"synthetic-publish-key", "destination:11:twitch")
        .unwrap();
    assert_eq!(
        key.open(&sealed, "destination:11:twitch").unwrap().expose(),
        b"synthetic-publish-key"
    );
    assert!(key.open(&sealed, "destination:12:twitch").is_err());
    assert!(key.open(&sealed, "destination:11:kick").is_err());
    assert!(key.open(&sealed[..13], "destination:11:twitch").is_err());
}

#[test]
fn probe_dump_scope_is_optional_and_rejects_invalid_account_ids() {
    let mut config: toml::Value =
        toml::from_str(include_str!("../../../config/uplink-beispiel.toml")).unwrap();
    assert_eq!(
        Config::parse(&toml::to_string(&config).unwrap())
            .unwrap()
            .media
            .probe_dump_streamer_id,
        None
    );
    for id in [0, -1] {
        config["media"]
            .as_table_mut()
            .unwrap()
            .insert("probe_dump_streamer_id".into(), toml::Value::Integer(id));
        assert!(Config::parse(&toml::to_string(&config).unwrap()).is_err());
    }
    config["media"].as_table_mut().unwrap().insert(
        "probe_dump_streamer_id".into(),
        toml::Value::Integer(538636411),
    );
    assert_eq!(
        Config::parse(&toml::to_string(&config).unwrap())
            .unwrap()
            .media
            .probe_dump_streamer_id,
        Some(538636411)
    );
}

#[test]
fn normal_ingest_stops_keep_active_and_recent_status_free_of_errors() {
    use uplink_ingest::EndReason;
    for reason in [EndReason::ExplicitStop, EndReason::PeerClosed] {
        let registry = Registry::new(1, 1).unwrap();
        let reservation = registry.reserve(11).unwrap();
        reservation.record(123);
        reservation.ingest_ended(&reason);
        let status = &registry.status(11)[0];
        assert!(status.active);
        assert!(status.error.is_none(), "{reason:?} ist ein normaler Stopp");
        assert_ne!(status.state, "Fehler");
        assert_eq!(status.ingest_end_reason, Some(format!("{reason:?}")));
        reservation.ended();
        assert_eq!(registry.status(11)[0].state, "Beendet");
        drop(reservation);
        let status = &registry.status(11)[0];
        assert!(!status.active);
        assert!(status.error.is_none());
        assert_eq!(status.state, "Beendet");
        let json = serde_json::to_value(status).unwrap();
        assert!(json["error"].is_null());
        assert_eq!(json["state"], "Beendet");
        assert_eq!(json["ingest_end_reason"], format!("{reason:?}"));
    }
}

#[test]
fn media_rejection_and_existing_errors_survive_session_completion() {
    use uplink_ingest::{EndReason, MediaError};
    let registry = Registry::new(1, 1).unwrap();
    let reservation = registry.reserve(11).unwrap();
    reservation.ingest_ended(&EndReason::MediaRejected(MediaError::TimestampRegression));
    reservation.ended();
    assert_eq!(registry.status(11)[0].state, "Fehler");
    drop(reservation);
    let status = &registry.status(11)[0];
    assert!(!status.active);
    assert_eq!(status.state, "Fehler");
    assert!(status.error.is_some());
    assert_eq!(
        status.ingest_end_reason.as_deref(),
        Some("MediaRejected(TimestampRegression)")
    );
    let reservation = registry.reserve(11).unwrap();
    reservation.fail("Ausgang abgewiesen");
    reservation.ingest_ended(&EndReason::PeerClosed);
    reservation.ended();
    drop(reservation);
    let status = &registry.status(11)[0];
    assert_eq!(status.state, "Fehler");
    assert_eq!(status.error, Some("Ausgang abgewiesen"));
}
