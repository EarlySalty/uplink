use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TwitchOutputMode {
    #[default]
    Single,
    Enhanced,
    /// Echter Twitch-2K-Pfad: 2560×1440@60 HEVC kommt bereits vom Quellrechner
    /// und wird unverändert an Twitch weitergereicht; nur niedrigere H.264-Stufen
    /// werden auf dem Uplink-Server erzeugt.
    #[serde(rename = "native_2k")]
    Native2k,
    /// Experimenteller 2K-Pfad: 2560×1440@60 AV1 kommt vom Quellrechner;
    /// Uplink decodiert AV1 und erzeugt die von Twitch geforderte HEVC-Topspur
    /// sowie die niedrigeren H.264-Stufen. Produktion bleibt bis zur Lastmessung
    /// über ein eigenes Kapazitätsbudget gesperrt.
    #[serde(rename = "native_2k_av1")]
    Native2kAv1,
}

impl TwitchOutputMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Enhanced => "enhanced",
            Self::Native2k => "native_2k",
            Self::Native2kAv1 => "native_2k_av1",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationUpdate {
    pub platform: String,
    /// Generation 0 gilt ausschließlich für den noch unveränderten Bestand.
    #[serde(default)]
    pub connection_generation: i64,
    pub rtmp_url: Option<String>,
    pub stream_key: Option<String>,
    pub enabled: Option<bool>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    /// Legacy-Feld für ältere Dashboard-Versionen. Twitch trennt Live und VOD
    /// automatisch; der Wert wird validiert, aber nicht mehr gespeichert oder
    /// für den Mediengraph ausgewertet.
    pub twitch_audio_mode: Option<String>,
    /// Auslassen erhält auch bei einem OAuth-Refresh die gespeicherte Betriebsart.
    pub twitch_output_mode: Option<TwitchOutputMode>,
}
impl Drop for DestinationUpdate {
    fn drop(&mut self) {
        if let Some(endpoint) = &mut self.rtmp_url {
            endpoint.zeroize();
        }
        if let Some(key) = &mut self.stream_key {
            key.zeroize();
        }
    }
}
impl DestinationUpdate {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.connection_generation < 0
            || !matches!(
                self.platform.as_str(),
                "twitch" | "kick" | "youtube" | "tiktok"
            )
            || self.width.is_some_and(|v| v <= 0 || v > 8192 || v % 2 != 0)
            || self
                .height
                .is_some_and(|v| v <= 0 || v > 8192 || v % 2 != 0)
            || self.fps.is_some_and(|v| v <= 0 || v > 240)
            || self.bitrate_kbps.is_some_and(|v| v <= 0 || v > 100_000)
            || (self.twitch_output_mode.is_some() && self.platform != "twitch")
            || self.twitch_audio_mode.as_deref().is_some_and(|mode| {
                self.platform != "twitch" || !matches!(mode, "live" | "separate_vod")
            })
            || self.stream_key.as_ref().is_some_and(|key| {
                key.len() > 4096 || key.bytes().any(|b| b == 0 || b == b'\r' || b == b'\n')
            })
        {
            return Err("Ziel oder gewünschtes Profil ist ungültig.");
        }
        if let Some(endpoint) = &self.rtmp_url {
            public_endpoint(endpoint)?;
        }
        Ok(())
    }

    pub fn validate_policy(&self, config: &crate::config::Config) -> Result<(), &'static str> {
        let policy = config
            .platforms
            .iter()
            .find(|policy| policy.name == self.platform)
            .ok_or("Für das Ziel fehlt die geprüfte Plattformkonfiguration.")?;
        if let Some(endpoint) = &self.rtmp_url {
            runtime_endpoint(&self.platform, endpoint, policy)?;
        }
        Ok(())
    }
}

/// Einheitliche Syntax-/Host-/TLS-Policy für Speichern und tatsächlichen Start.
/// DNS-/IP-Pinning findet weiterhin unmittelbar beim Verbindungsaufbau statt.
pub fn runtime_endpoint(
    platform: &str,
    endpoint: &str,
    policy: &crate::config::PlatformConfig,
) -> Result<String, &'static str> {
    public_endpoint(endpoint)?;
    let prepared = crate::media::secure_default(platform, endpoint.to_owned());
    let url = reqwest::Url::parse(&prepared).map_err(|_| "Plattformadresse ist ungültig.")?;
    if !url.host_str().is_some_and(|host| {
        policy
            .allowed_hosts
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(host))
    }) || (url.scheme() != "rtmps" && !policy.allow_unencrypted)
        || url.port() == Some(0)
    {
        return Err("Serveradresse oder Transport ist für diese Plattform nicht freigegeben.");
    }
    Ok(prepared)
}

/// Ausschließlich feste öffentliche RTMP-Appnamen; ein beliebiger Pfad ist
/// potenziell selbst ein Zugang. Abweichende Provider benötigen einen geprüften
/// Adapter und dürfen nicht als öffentliche URL gespeichert werden.
pub fn public_endpoint(endpoint: &str) -> Result<(), &'static str> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| "Plattformadresse ist ungültig.")?;
    let raw_path = endpoint
        .split_once("://")
        .and_then(|(_, authority)| authority.find('/').map(|index| &authority[index..]))
        .unwrap_or("");
    if endpoint.len() > 2048
        || !matches!(url.scheme(), "rtmp" | "rtmps")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(
            url.path(),
            "/app" | "/app/" | "/live" | "/live/" | "/live2" | "/live2/" | "/game" | "/game/"
        )
        || !matches!(
            raw_path,
            "/app" | "/app/" | "/live" | "/live/" | "/live2" | "/live2/" | "/game" | "/game/"
        )
        || endpoint.contains('%')
        || endpoint.contains('\\')
        || endpoint.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("Plattformadresse muss eine öffentliche Serveradresse ohne Zugangsdaten sein.");
    }
    Ok(())
}
