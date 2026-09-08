use uplink_service::{config::Config, crypto::Secret, registry::Registry};

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
