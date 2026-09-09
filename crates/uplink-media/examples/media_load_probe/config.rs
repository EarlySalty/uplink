use super::ProbeResult;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};
use uplink_core::{
    Chroma, Codec, Color, ColorPrimaries, ColorRange, FrameRate, Gop, LayoutRevision, Matrix,
    RateControl, RateMode, Transfer, VideoProfile,
};
use uplink_media::{Composition, Crop, ProgramVideo, portrait::compile_portrait};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub synthetic_source: bool,
    pub source: PathBuf,
    pub output_directory: PathBuf,
    pub sample_seconds: u32,
    pub source_sha256: String,
    pub source_width: u32,
    pub source_height: u32,
    pub source_fps: u32,
    pub repeat_ms: u32,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub sessions: usize,
    pub paced: bool,
    pub media_seconds: u32,
    pub warmup_seconds: u32,
    pub hard_timeout_seconds: u32,
    pub receiver_delay_ms: u64,
    pub queue_events: usize,
    pub queue_bytes: usize,
    pub worker_threads: usize,
    pub audio_encoding: Option<Audio>,
    pub profiles: Vec<Profile>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub wire_track: u8,
    pub canvas_index: u8,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub gop_frames: u32,
    pub level: String,
    pub synthetic_portrait: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    pub channels: u8,
    pub bitrate_kbps: u32,
}

impl Config {
    pub fn parse(value: &str) -> ProbeResult<Self> {
        let config: Self = toml::from_str(value).map_err(|_| "Ungültige Lastkonfiguration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> ProbeResult<()> {
        if self.audio_encoding.is_some_and(|audio| {
            !(1..=2).contains(&audio.channels) || !(32..=320).contains(&audio.bitrate_kbps)
        }) {
            return Err("Synthetisches AAC-Encoding benötigt 1–2 Kanäle und 32–320 kbit/s");
        }
        if !self.synthetic_source
            || !self.source.is_absolute()
            || !self.output_directory.is_absolute()
            || self.sample_seconds > 10
            || !self.ffmpeg.is_absolute()
            || !self.ffprobe.is_absolute()
            || self.source_sha256.len() != 64
            || !self.source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !(1..=4).contains(&self.sessions)
            || !(1..=1_800).contains(&self.media_seconds)
            || self.warmup_seconds >= self.media_seconds
            || !(5..=1_920).contains(&self.hard_timeout_seconds)
            || self.hard_timeout_seconds <= self.media_seconds
            || !(100..=120_000).contains(&self.repeat_ms)
            || !(16..=512).contains(&self.queue_events)
            || !(4_194_304..=33_554_432).contains(&self.queue_bytes)
            || !(1..=4).contains(&self.worker_threads)
            || self.receiver_delay_ms > 1_000
            || !matches!(
                (self.source_width, self.source_height),
                (256, 144) | (1920, 1080) | (2560, 1440)
            )
            || !matches!(self.source_fps, 25 | 30 | 60)
            || self.profiles.is_empty()
            || self.profiles.len() > 8
        {
            return Err("Lastkonfiguration überschreitet die festen Grenzen");
        }
        let mut tracks = BTreeSet::new();
        for profile in &self.profiles {
            if !tracks.insert(profile.wire_track)
                || profile.width < 16
                || profile.height < 16
                || profile.width > 2560
                || profile.height > 1920
                || !profile.width.is_multiple_of(2)
                || !profile.height.is_multiple_of(2)
                || !matches!(profile.fps, 25 | 30 | 60)
                || profile.fps > self.source_fps
                || !(64..=30_000).contains(&profile.bitrate_kbps)
                || !(1..=600).contains(&profile.gop_frames)
                || !matches!(
                    profile.level.as_str(),
                    "4.0" | "4.1" | "4.2" | "5.0" | "5.1" | "5.2"
                )
                || (profile.synthetic_portrait && (profile.width, profile.height) != (1080, 1920))
                || (!profile.synthetic_portrait
                    && (profile.canvas_index != 0 || profile.height > profile.width))
                || (profile.synthetic_portrait && profile.canvas_index != 1)
            {
                return Err("Synthetisches Ausgabeprofil ist ungültig");
            }
            profile.video(self)?;
        }
        Ok(())
    }
}

impl Profile {
    pub fn video(&self, config: &Config) -> ProbeResult<ProgramVideo> {
        let profile = VideoProfile {
            width: self.width,
            height: self.height,
            fps: FrameRate::new(self.fps, 1).map_err(|_| "Bildrate ist ungültig")?,
            codec: Codec::H264,
            codec_profile: "high".into(),
            level: self.level.clone(),
            bit_depth: 8,
            chroma: Chroma::Yuv420,
            color: Color {
                primaries: ColorPrimaries::Bt709,
                transfer: Transfer::Bt709,
                matrix: Matrix::Bt709,
                range: ColorRange::Limited,
            },
            rate: RateControl {
                mode: RateMode::Cbr,
                target_kbps: self.bitrate_kbps,
                max_kbps: self.bitrate_kbps,
                buffer_kbits: self.bitrate_kbps * 2,
            },
            gop: Gop {
                keyframe_interval_frames: self.gop_frames,
                closed: true,
            },
        };
        let layout = if self.synthetic_portrait {
            Some(
                compile_portrait(
                    config.source_width,
                    config.source_height,
                    &profile,
                    LayoutRevision { id: 1, revision: 1 },
                    Composition::Crop(Crop {
                        x: 0,
                        y: 0,
                        width: config.source_width,
                        height: config.source_height,
                    }),
                )
                .map_err(|_| "Synthetische Hochkantfläche ist ungültig")?,
            )
        } else {
            None
        };
        Ok(ProgramVideo {
            wire_track: self.wire_track,
            canvas_index: self.canvas_index,
            profile,
            layout,
        })
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn valid() -> Config {
        Config {
            synthetic_source: true,
            source: "/tmp/synthetic.flv".into(),
            source_sha256: "0".repeat(64),
            output_directory: "/tmp/synthetic-output".into(),
            sample_seconds: 3,
            source_width: 256,
            source_height: 144,
            source_fps: 25,
            repeat_ms: 2_000,
            ffmpeg: "/opt/ffmpeg".into(),
            ffprobe: "/opt/ffprobe".into(),
            sessions: 1,
            paced: true,
            media_seconds: 3,
            warmup_seconds: 0,
            hard_timeout_seconds: 40,
            receiver_delay_ms: 0,
            queue_events: 512,
            queue_bytes: 8_388_608,
            worker_threads: 2,
            audio_encoding: None,
            profiles: vec![Profile {
                wire_track: 0,
                canvas_index: 0,
                width: 256,
                height: 144,
                fps: 25,
                bitrate_kbps: 384,
                gop_frames: 50,
                level: "4.2".into(),
                synthetic_portrait: false,
            }],
        }
    }

    #[test]
    fn rejects_unbounded_runs_and_private_or_ambiguous_configuration() {
        let base = valid();
        assert!(base.validate().is_ok());
        for change in [0, 1, 2, 3, 4, 5, 6] {
            let mut config = base.clone();
            match change {
                0 => config.synthetic_source = false,
                1 => config.sessions = 5,
                2 => config.media_seconds = 1_801,
                3 => config.hard_timeout_seconds = 1,
                4 => config.profiles.push(config.profiles[0].clone()),
                5 => config.source_sha256 = "invalid".into(),
                _ => config.queue_bytes = usize::MAX,
            }
            assert!(config.validate().is_err());
        }
        assert!(Config::parse("endpoint = 'should-be-rejected'").is_err());
    }

    #[test]
    fn portrait_keeps_the_whole_source_and_requires_test_canvas() {
        let mut config = valid();
        config.profiles[0].synthetic_portrait = true;
        config.profiles[0].width = 1080;
        config.profiles[0].height = 1920;
        assert!(config.validate().is_err());
        config.profiles[0].canvas_index = 1;
        assert!(config.validate().is_ok());
        assert_eq!(
            config.profiles[0]
                .video(&config)
                .unwrap()
                .layout
                .unwrap()
                .composition,
            Composition::Crop(Crop {
                x: 0,
                y: 0,
                width: 256,
                height: 144
            })
        );
    }

    #[test]
    fn audio_encoding_uses_the_program_contract_bounds() {
        let mut config = valid();
        config.audio_encoding = Some(Audio {
            channels: 2,
            bitrate_kbps: 128,
        });
        assert!(config.validate().is_ok());
        config.audio_encoding = Some(Audio {
            channels: 3,
            bitrate_kbps: 128,
        });
        assert!(config.validate().is_err());
        config.audio_encoding = Some(Audio {
            channels: 2,
            bitrate_kbps: 321,
        });
        assert!(config.validate().is_err());
    }
}
