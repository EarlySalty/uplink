use uplink_service::destinations::DestinationUpdate;

#[test]
fn rejects_secret_bearing_urls_and_unbounded_profiles() {
    for bad in [
        r#"{"platform":"twitch","rtmp_url":"rtmps://user:key@live.twitch.tv/app"}"#,
        r#"{"platform":"twitch","rtmp_url":"rtmps://live.twitch.tv/app?key=secret"}"#,
        r#"{"platform":"twitch","width":0}"#,
        r#"{"platform":"unknown"}"#,
    ] {
        let update: DestinationUpdate = serde_json::from_str(bad).unwrap();
        assert!(update.validate().is_err());
    }
}

#[test]
fn partial_profile_update_preserves_omitted_credentials() {
    let update: DestinationUpdate =
        serde_json::from_str(r#"{"platform":"twitch","width":1280,"height":720}"#).unwrap();
    assert!(update.validate().is_ok());
    assert!(update.rtmp_url.is_none());
    assert!(update.stream_key.is_none());
}

#[test]
fn endpoint_path_credentials_never_pass_as_public_server_addresses() {
    for platform in ["twitch", "kick", "youtube", "tiktok"] {
        let value = serde_json::json!({"platform":platform,"rtmp_url":"rtmps://publish.invalid/app/synthetic-private-publish-key"});
        let update: DestinationUpdate = serde_json::from_value(value).unwrap();
        assert!(update.validate().is_err());
    }
}
