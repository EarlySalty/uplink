use uplink_service::{config::Config, crypto::Secret, registry::Registry};

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
