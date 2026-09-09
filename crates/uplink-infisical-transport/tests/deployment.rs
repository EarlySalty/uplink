use std::collections::BTreeMap;

#[test]
fn userunit_keeps_host_root_visible_to_the_socket_validator() {
    // Effektive geerbte Werte der rs-relay-Userunit bei der Umschaltung.
    // systemd 255 erzeugt dafür eine Usernamespace mit nur UID/GID 1000.
    let mut service = BTreeMap::from([
        ("PrivateTmp", "yes"),
        ("ProtectSystem", "strict"),
        ("ProtectProc", "invisible"),
        ("PrivateUsers", "no"),
    ]);
    let mut in_service = false;
    for line in include_str!("../../../deployment/rs-relay-override.conf").lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_service = line == "[Service]";
        } else if in_service && let Some((key, value)) = line.split_once('=') {
            service.insert(key, value);
        }
    }
    for (key, expected) in [
        ("PrivateTmp", "no"),
        ("ProtectSystem", "no"),
        ("ProtectProc", "default"),
        ("PrivateUsers", "no"),
    ] {
        assert_eq!(
            service.get(key),
            Some(&expected),
            "{key} verdeckt Host-UID 0"
        );
    }
    assert_eq!(service.get("NoNewPrivileges"), Some(&"true"));
    assert_eq!(service.get("UMask"), Some(&"0077"));
}
