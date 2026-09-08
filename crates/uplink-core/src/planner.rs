use crate::*;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStatus {
    PendingInput,
    Rejected,
    Planned,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoAction {
    Copy,
    Encode,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioAction {
    Copy,
    Encode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    TargetVideoUnsupported,
    TargetAudioUnsupported(AudioRole),
    MissingAudio(AudioRole),
    VideoWorkerUnavailable,
    AudioWorkerUnavailable(AudioRole),
    VideoDecoderUnavailable,
    AudioDecoderUnavailable(AudioRole),
    LayoutWorkerUnavailable,
    UpscaleNotApproved,
    FrameDuplicationNotApproved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPlan {
    pub source_track: u32,
    pub role: AudioRole,
    pub action: AudioAction,
}

#[derive(Debug, Clone)]
pub struct OutputPlan {
    pub id: String,
    pub platform: Platform,
    pub requested_video: VideoProfile,
    pub status: OutputStatus,
    pub reasons: Vec<Rejection>,
    pub video: Option<VideoAction>,
    pub live_audio: Option<AudioPlan>,
    pub vod_audio: Option<AudioPlan>,
}

/// Nur aus einem validierten Plan erzeugbar. Keine Audio- oder Plattformidentität:
/// diese beeinflussen den gemeinsamen Video-Encode nicht.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EncodeKey {
    scope: SessionScope,
    source_track: u32,
    layout: Option<LayoutRevision>,
    video: VideoProfile,
    encoder_preset_revision: String,
}

#[derive(Debug, Clone)]
pub struct EncodeGroup {
    pub key: EncodeKey,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub outputs: Vec<OutputPlan>,
    pub encode_groups: Vec<EncodeGroup>,
    /// Logische Anzahl benötigter Decoder. Kein Nachweis laufender Worker.
    pub shared_decode_count: usize,
}

fn validate(input: &PlanInput) -> Result<(), Error> {
    let limits = &input.limits;
    if limits.max_width == 0
        || limits.max_height == 0
        || limits.max_pixels == 0
        || limits.max_video_kbps == 0
        || limits.max_audio_kbps == 0
        || limits.max_outputs == 0
    {
        return Err(Error::Invalid("Ressourcengrenzen müssen positiv sein."));
    }
    if input.outputs.len() > limits.max_outputs {
        return Err(Error::Invalid("Zu viele gewünschte Ausgänge."));
    }
    if let Some(source) = &input.source {
        source.video.validate(limits)?;
        if source.video_track == 0 || source.audio.len() > limits.max_audio_tracks {
            return Err(Error::Invalid(
                "Eingangsspur fehlt oder es gibt zu viele Audiospuren.",
            ));
        }
        let mut tracks = HashSet::from([source.video_track]);
        let mut roles = HashSet::new();
        for track in &source.audio {
            track.profile.validate(limits)?;
            if track.track_id == 0 || !tracks.insert(track.track_id) || !roles.insert(track.role) {
                return Err(Error::Invalid(
                    "Track-IDs und Audio-Rollen müssen eindeutig sein.",
                ));
            }
        }
    }
    let mut ids = HashSet::new();
    for output in &input.outputs {
        if !label_valid(&output.id)
            || !ids.insert(&output.id)
            || !label_valid(&output.encoder_preset_revision)
        {
            return Err(Error::Invalid(
                "Ausgangs-ID oder Preset-Revision ist ungültig oder doppelt.",
            ));
        }
        if output
            .layout
            .as_ref()
            .is_some_and(|layout| layout.id == 0 || layout.revision == 0)
        {
            return Err(Error::Invalid(
                "Layout braucht eine Identität und Revision.",
            ));
        }
        output.video.validate(limits)?;
        output.live_audio.profile.validate(limits)?;
        if let Some(audio) = &output.vod_audio {
            audio.profile.validate(limits)?;
        }
        for video in &output.capabilities.video {
            video.validate(limits)?;
        }
        for audio in &output.capabilities.audio {
            audio.validate(limits)?;
        }
    }
    for video in &input.worker.encodable_video {
        video.validate(limits)?;
    }
    for video in &input.worker.decodable_video {
        video.validate(limits)?;
    }
    for audio in &input.worker.encodable_audio {
        audio.validate(limits)?;
    }
    for audio in &input.worker.decodable_audio {
        audio.validate(limits)?;
    }
    for layout in &input.worker.compositable_layouts {
        if layout.id == 0 || layout.revision == 0 {
            return Err(Error::Invalid(
                "Worker-Layout braucht eine gültige Revision.",
            ));
        }
    }
    Ok(())
}

fn audio_plan(
    source: &Source,
    request: &AudioRequest,
    worker: &WorkerCapabilities,
    reasons: &mut Vec<Rejection>,
) -> Option<AudioPlan> {
    let Some(track) = source.audio.iter().find(|track| track.role == request.role) else {
        reasons.push(Rejection::MissingAudio(request.role));
        return None;
    };
    let action = if track.profile == request.profile {
        AudioAction::Copy
    } else {
        AudioAction::Encode
    };
    if action == AudioAction::Encode && !worker.encodable_audio.contains(&request.profile) {
        reasons.push(Rejection::AudioWorkerUnavailable(request.role));
    }
    if action == AudioAction::Encode && !worker.decodable_audio.contains(&track.profile) {
        reasons.push(Rejection::AudioDecoderUnavailable(request.role));
    }
    Some(AudioPlan {
        source_track: track.track_id,
        role: track.role,
        action,
    })
}

/// Plant deklarierte Szenariodaten. Keine Netzwerkzugriffe, Reservierungen,
/// Codecprüfung oder automatische Anpassung gespeicherter Wünsche.
pub fn plan(input: &PlanInput) -> Result<Plan, Error> {
    validate(input)?;
    let mut result = Plan {
        outputs: Vec::new(),
        encode_groups: Vec::new(),
        shared_decode_count: 0,
    };
    let mut groups: HashMap<EncodeKey, usize> = HashMap::new();
    for request in &input.outputs {
        let mut output = OutputPlan {
            id: request.id.clone(),
            platform: request.platform,
            requested_video: request.video.clone(),
            status: OutputStatus::PendingInput,
            reasons: Vec::new(),
            video: None,
            live_audio: None,
            vod_audio: None,
        };
        if !request.capabilities.video.contains(&request.video) {
            output.reasons.push(Rejection::TargetVideoUnsupported);
        }
        for audio in std::iter::once(&request.live_audio).chain(request.vod_audio.as_ref()) {
            if !request.capabilities.audio.contains(&audio.profile) {
                output
                    .reasons
                    .push(Rejection::TargetAudioUnsupported(audio.role));
            }
        }
        if request
            .layout
            .as_ref()
            .is_some_and(|layout| !input.worker.compositable_layouts.contains(layout))
        {
            output.reasons.push(Rejection::LayoutWorkerUnavailable);
        }
        if let Some(source) = &input.source {
            if request.video.fps > source.video.fps {
                output.reasons.push(Rejection::FrameDuplicationNotApproved);
            }
            // Layout-Crops werden hier noch nicht aufgelöst. Dieser Flächencheck
            // beweist ausdrücklich nicht die Detailqualität des späteren Crops.
            if request.video.pixels() > source.video.pixels()
                || (request.layout.is_none()
                    && (request.video.width > source.video.width
                        || request.video.height > source.video.height))
            {
                output.reasons.push(Rejection::UpscaleNotApproved);
            }
            let action = if request.layout.is_none() && source.video == request.video {
                VideoAction::Copy
            } else {
                VideoAction::Encode
            };
            if action == VideoAction::Encode
                && !input.worker.encodable_video.contains(&request.video)
            {
                output.reasons.push(Rejection::VideoWorkerUnavailable);
            }
            if action == VideoAction::Encode
                && !input.worker.decodable_video.contains(&source.video)
            {
                output.reasons.push(Rejection::VideoDecoderUnavailable);
            }
            output.live_audio = audio_plan(
                source,
                &request.live_audio,
                &input.worker,
                &mut output.reasons,
            );
            if let Some(audio) = &request.vod_audio {
                output.vod_audio = audio_plan(source, audio, &input.worker, &mut output.reasons);
            }
            if output.reasons.is_empty() {
                output.status = OutputStatus::Planned;
                output.video = Some(action);
                if action == VideoAction::Encode {
                    let key = EncodeKey {
                        scope: source.scope,
                        source_track: source.video_track,
                        layout: request.layout.clone(),
                        video: request.video.clone(),
                        encoder_preset_revision: request.encoder_preset_revision.clone(),
                    };
                    if let Some(&index) = groups.get(&key) {
                        result.encode_groups[index].outputs.push(request.id.clone());
                    } else {
                        if result.encode_groups.len() >= input.limits.max_unique_encodes {
                            return Err(Error::EncodeCapacity);
                        }
                        groups.insert(key.clone(), result.encode_groups.len());
                        result.encode_groups.push(EncodeGroup {
                            key,
                            outputs: vec![request.id.clone()],
                        });
                    }
                }
            }
        }
        if !output.reasons.is_empty() {
            output.status = OutputStatus::Rejected;
            output.live_audio = None;
            output.vod_audio = None;
        }
        result.outputs.push(output);
    }
    result.shared_decode_count = usize::from(!result.encode_groups.is_empty());
    Ok(result)
}
