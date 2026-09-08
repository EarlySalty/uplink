//! Chat und OBS-Docks. OAuth und persistente Dockidentitäten werden injiziert.
pub mod adapter;
mod bus;
pub mod ereignis;
pub mod helix;
pub mod hervorhebung;
mod http;
mod hub;
pub mod kennzahlen;
pub mod kick;
pub mod kick_webhook;
pub mod nachricht;
pub mod punkte;
mod router;
pub mod streaminfo;
mod supervisor;
pub mod token;
pub mod twitch;
pub mod twitch_activity;
pub mod youtube;
pub use hub::{ChatConfig, ChatHub, DockIdentity, DockUser, Status as ChatStatus};
pub use router::router;
pub use token::{BrokerError, Grant, PlatformBroker};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Twitch,
    Kick,
    YouTube,
    TikTok,
}
impl Platform {
    pub const ALL: [Self; 4] = [Self::Twitch, Self::Kick, Self::YouTube, Self::TikTok];
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Twitch => "twitch",
            Self::Kick => "kick",
            Self::YouTube => "youtube",
            Self::TikTok => "tiktok",
        }
    }
    pub fn anzeige(self) -> &'static str {
        match self {
            Self::Twitch => "Twitch",
            Self::Kick => "Kick",
            Self::YouTube => "YouTube",
            Self::TikTok => "TikTok",
        }
    }
    pub fn rang(self) -> usize {
        Self::ALL
            .iter()
            .position(|p| *p == self)
            .expect("Plattform")
    }
}
impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.anzeige())
    }
}
impl std::str::FromStr for Platform {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or("Unbekannte Plattform")
    }
}
