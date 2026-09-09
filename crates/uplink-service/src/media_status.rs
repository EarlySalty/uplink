//! Öffentliche Darstellung der tatsächlich laufenden Medienverarbeitung.
use crate::registry::SessionStatus;
use serde_json::{Value, json};

pub fn failure_reason(reason: &Value) -> &'static str {
    match reason.as_str() {
        Some("missing_track") => {
            "Eine für dieses Ziel benötigte Medienspur fehlt. Live- und VOD-Audiospuren in OBS prüfen."
        }
        Some("unsupported_profile" | "invalid_plan") => {
            "Dieses Ausgabeprofil passt nicht zur erkannten Quelle oder ist hier noch nicht freigegeben. Zieleinstellungen prüfen."
        }
        Some("endpoint_rejected") => "Diese Plattformadresse ist hier nicht freigegeben.",
        Some("tls_rejected") => {
            "Das Zertifikat des Plattformausgangs konnte nicht bestätigt werden."
        }
        Some("publish_rejected") => {
            "Die Plattform hat den Streamstart abgelehnt. Zugang und Ausgabeprofil prüfen."
        }
        Some("backpressure") => {
            "Dieser Ausgang nimmt die Medien nicht schnell genug an und wurde angehalten."
        }
        Some("start_timeout") => "Dieser Plattformausgang konnte nicht rechtzeitig starten.",
        Some("resource_limit") => "Für diesen Ausgang wurde eine Verarbeitungsgrenze erreicht.",
        _ => "Ausgang wurde abgewiesen oder unterbrochen.",
    }
}

fn laufende_stufe(eintrag: &Value) -> Option<Value> {
    let profile = &eintrag["profile"];
    let positive = |field: &str| profile[field].as_u64().filter(|value| *value > 0);
    let (Some(width), Some(height), Some(numerator), Some(denominator), Some(bitrate)) = (
        positive("width"),
        positive("height"),
        positive("fps_numerator"),
        positive("fps_denominator"),
        positive("target_bitrate_kbps"),
    ) else {
        return None;
    };
    if eintrag["mode"] != "encode"
        || !matches!(profile["codec"].as_str(), Some("h264" | "av1" | "hevc"))
    {
        return None;
    }
    Some(json!({
        "width":width,
        "height":height,
        "fps":numerator as f64 / denominator as f64,
        "codec":profile["codec"],
        "bitrate_kbps":bitrate,
        "canvas_index":eintrag["canvas_index"].as_u64().unwrap_or(0),
        "encoder":eintrag["encoder"],
        "layout_id":eintrag["layout_id"],
        "layout_revision":eintrag["layout_revision"],
        "profile_origin":"running_graph",
    }))
}

fn laufender_graph<'a>(
    session: Option<&'a SessionStatus>,
    platform: &str,
) -> Option<&'a Vec<Value>> {
    session
        .filter(|session| session.active)
        .and_then(|session| session.outputs.as_ref())
        .and_then(|s| s["graph"].as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["id"] == platform && item["profile_origin"] == "running_graph")
        })
        .and_then(|graph| graph["video"].as_array())
}

pub fn active_profile(session: Option<&SessionStatus>, platform: &str, state: &str) -> Value {
    let Some(graph) = session
        .filter(|session| session.active && state == "sending")
        .and_then(|session| laufender_graph(Some(session), platform))
    else {
        return Value::Null;
    };
    let video = graph
        .iter()
        .find(|eintrag| eintrag["canvas_index"].as_u64().unwrap_or(0) == 0);
    match video.and_then(laufende_stufe) {
        Some(stufe) => stufe,
        None => Value::Null,
    }
}

pub fn active_profiles(session: Option<&SessionStatus>, platform: &str, state: &str) -> Value {
    let Some(graph) = session
        .filter(|session| session.active && state == "sending")
        .and_then(|session| laufender_graph(Some(session), platform))
    else {
        return json!([]);
    };
    let stufen: Vec<Value> = graph.iter().filter_map(laufende_stufe).collect();
    json!(stufen)
}

/// Lokales Audio-Routing, keine Bestätigung eines Plattform-VODs.
pub fn active_audio_mode(session: Option<&SessionStatus>, platform: &str, state: &str) -> Value {
    let Some(session) =
        session.filter(|session| session.active && state == "sending" && platform == "twitch")
    else {
        return Value::Null;
    };
    let Some(audio) = session
        .outputs
        .as_ref()
        .and_then(|status| status["graph"].as_array())
        .and_then(|graphs| {
            graphs
                .iter()
                .find(|graph| graph["id"] == platform && graph["profile_origin"] == "running_graph")
        })
        .and_then(|graph| graph["audio"].as_array())
    else {
        return Value::Null;
    };
    match audio.as_slice() {
        [live] if live["destination_wire_track"] == 0 => json!("live"),
        [live, vod]
            if live["destination_wire_track"] == 0
                && vod["destination_wire_track"] == 1
                && live["source_wire_track"] != vod["source_wire_track"] =>
        {
            json!("separate_vod")
        }
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use serde_json::json;

    #[test]
    fn profile_requires_running_graph_and_media_flow_not_saved_wishes() {
        let registry = Registry::new(2, 1).unwrap();
        let reservation = registry.reserve(11).unwrap();
        reservation.media_status(json!({"graph":[{"id":"youtube","profile_origin":"running_graph","video":[{"mode":"encode","profile":{"width":1280,"height":720,"fps_numerator":30000,"fps_denominator":1001,"codec":"h264","target_bitrate_kbps":4500}}]}]}));
        let sessions = registry.status(11);
        assert!(active_profile(sessions.first(), "youtube", "starting").is_null());
        assert!(active_profile(sessions.first(), "twitch", "sending").is_null());
        let profile = active_profile(sessions.first(), "youtube", "sending");
        assert_eq!(profile["width"], 1280);
        assert_eq!(profile["bitrate_kbps"], 4500);
        assert_eq!(profile["profile_origin"], "running_graph");
        assert_eq!(profile["fps"], 30000.0 / 1001.0);
        drop(reservation);
        assert!(active_profile(registry.status(11).first(), "youtube", "sending").is_null());
    }

    #[test]
    fn active_profiles_liefert_jede_laufende_stufe_ohne_revision_ohne_layout() {
        let registry = Registry::new(2, 1).unwrap();
        let reservation = registry.reserve(21).unwrap();
        reservation.media_status(json!({"graph":[{"id":"twitch","profile_origin":"running_graph","video":[
            {"mode":"encode","canvas_index":0,"encoder":"libx264","profile":{"width":1920,"height":1080,"fps_numerator":60,"fps_denominator":1,"codec":"h264","target_bitrate_kbps":6000}},
            {"mode":"encode","canvas_index":1,"encoder":"libx264","layout_id":7,"layout_revision":3,"profile":{"width":1080,"height":1920,"fps_numerator":60,"fps_denominator":1,"codec":"h264","target_bitrate_kbps":3000}}
        ],"audio":[]}]}));
        let sessions = registry.status(21);
        let stufen = active_profiles(sessions.first(), "twitch", "sending");
        let stufen = stufen.as_array().unwrap();
        assert_eq!(stufen.len(), 2);
        assert_eq!(stufen[0]["canvas_index"], 0);
        assert_eq!(stufen[0]["layout_revision"], serde_json::Value::Null);
        assert_eq!(stufen[0]["layout_id"], serde_json::Value::Null);
        assert_eq!(stufen[0]["bitrate_kbps"], 6000);
        assert_eq!(stufen[1]["canvas_index"], 1);
        assert_eq!(stufen[1]["layout_id"], 7);
        assert_eq!(stufen[1]["layout_revision"], 3);
        let einzeln = active_profile(sessions.first(), "twitch", "sending");
        assert_eq!(einzeln["width"], 1920);
        assert_eq!(
            active_profile(sessions.first(), "twitch", "starting"),
            Value::Null
        );
        drop(reservation);
        let sessions = registry.status(21);
        assert_eq!(
            active_profiles(sessions.first(), "twitch", "sending"),
            json!([])
        );
    }
}
