use serde::Deserialize;
use zeroize::Zeroize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationUpdate {
    pub platform: String,
    pub rtmp_url: Option<String>,
    pub stream_key: Option<String>,
    pub enabled: Option<bool>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<i32>,
    pub bitrate_kbps: Option<i32>,
}
impl Drop for DestinationUpdate {
    fn drop(&mut self) {
        if let Some(key) = &mut self.stream_key {
            key.zeroize();
        }
    }
}
impl DestinationUpdate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(
            self.platform.as_str(),
            "twitch" | "kick" | "youtube" | "tiktok"
        ) || self.width.is_some_and(|v| v <= 0 || v > 8192 || v % 2 != 0)
            || self
                .height
                .is_some_and(|v| v <= 0 || v > 8192 || v % 2 != 0)
            || self.fps.is_some_and(|v| v <= 0 || v > 240)
            || self.bitrate_kbps.is_some_and(|v| v <= 0 || v > 100_000)
            || self.stream_key.as_ref().is_some_and(|key| {
                key.len() > 4096 || key.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
            })
        {
            return Err("Ziel oder gewünschtes Profil ist ungültig.");
        }
        if let Some(endpoint) = &self.rtmp_url {
            let url =
                reqwest::Url::parse(endpoint).map_err(|_| "Plattformadresse ist ungültig.")?;
            if endpoint.len() > 2048
                || !matches!(url.scheme(), "rtmp" | "rtmps")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("Plattformadresse darf keine Zugangsdaten enthalten.");
            }
        }
        Ok(())
    }
}
