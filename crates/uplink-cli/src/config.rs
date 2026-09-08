use serde::Deserialize;
use std::collections::BTreeMap;
use uplink_core::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    limits: Limits,
    video_profiles: BTreeMap<String, VideoProfile>,
    audio_profiles: BTreeMap<String, AudioProfile>,
    source: Option<ConfigSource>,
    worker: ConfigWorker,
    outputs: Vec<ConfigOutput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigSource {
    scope: SessionScope,
    video_track: u32,
    video_profile: String,
    audio: Vec<ConfigTrack>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigTrack {
    track_id: u32,
    role: AudioRole,
    profile: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigAudioRequest {
    role: AudioRole,
    profile: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigWorker {
    decodable_video: Vec<String>,
    encodable_video: Vec<String>,
    decodable_audio: Vec<String>,
    encodable_audio: Vec<String>,
    compositable_layouts: Vec<LayoutRevision>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigOutput {
    id: String,
    platform: Platform,
    video_profile: String,
    layout: Option<LayoutRevision>,
    encoder_preset_revision: String,
    live_audio: ConfigAudioRequest,
    vod_audio: Option<ConfigAudioRequest>,
    allowed_video_profiles: Vec<String>,
    allowed_audio_profiles: Vec<String>,
}

fn lookup<T: Clone>(profiles: &BTreeMap<String, T>, key: &str) -> Result<T, String> {
    profiles
        .get(key)
        .cloned()
        .ok_or_else(|| "Eine Profilreferenz ist unbekannt.".into())
}

fn resolve<T: Clone>(profiles: &BTreeMap<String, T>, keys: &[String]) -> Result<Vec<T>, String> {
    keys.iter().map(|key| lookup(profiles, key)).collect()
}

fn audio_request(
    profiles: &BTreeMap<String, AudioProfile>,
    request: &ConfigAudioRequest,
) -> Result<AudioRequest, String> {
    Ok(AudioRequest {
        role: request.role,
        profile: lookup(profiles, &request.profile)?,
    })
}

pub fn parse(text: &str) -> Result<PlanInput, String> {
    // Parserdiagnosen können den kompletten Eingabetext enthalten. Deshalb
    // keine ungefilterte Fehlermeldung, Werte oder unbekannten Feldnamen ausgeben.
    let config: Config = toml::from_str(text)
        .map_err(|_| "TOML-Syntax, Feldnamen oder Feldwerte sind ungültig.".to_string())?;
    if config.schema_version != 1 {
        return Err("Unbekannte Konfigurationsversion.".into());
    }
    let source = config
        .source
        .as_ref()
        .map(|source| -> Result<Source, String> {
            Ok(Source {
                scope: source.scope,
                video_track: source.video_track,
                video: lookup(&config.video_profiles, &source.video_profile)?,
                audio: source
                    .audio
                    .iter()
                    .map(|track| -> Result<AudioTrack, String> {
                        Ok(AudioTrack {
                            track_id: track.track_id,
                            role: track.role,
                            profile: lookup(&config.audio_profiles, &track.profile)?,
                        })
                    })
                    .collect::<Result<_, _>>()?,
            })
        })
        .transpose()?;
    let worker = WorkerCapabilities {
        decodable_video: resolve(&config.video_profiles, &config.worker.decodable_video)?,
        encodable_video: resolve(&config.video_profiles, &config.worker.encodable_video)?,
        decodable_audio: resolve(&config.audio_profiles, &config.worker.decodable_audio)?,
        encodable_audio: resolve(&config.audio_profiles, &config.worker.encodable_audio)?,
        compositable_layouts: config.worker.compositable_layouts,
    };
    let outputs = config
        .outputs
        .into_iter()
        .map(|output| -> Result<OutputRequest, String> {
            Ok(OutputRequest {
                id: output.id,
                platform: output.platform,
                video: lookup(&config.video_profiles, &output.video_profile)?,
                layout: output.layout,
                encoder_preset_revision: output.encoder_preset_revision,
                live_audio: audio_request(&config.audio_profiles, &output.live_audio)?,
                vod_audio: output
                    .vod_audio
                    .as_ref()
                    .map(|audio| audio_request(&config.audio_profiles, audio))
                    .transpose()?,
                capabilities: TargetCapabilities {
                    video: resolve(&config.video_profiles, &output.allowed_video_profiles)?,
                    audio: resolve(&config.audio_profiles, &output.allowed_audio_profiles)?,
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PlanInput {
        limits: config.limits,
        source,
        worker,
        outputs,
    })
}
