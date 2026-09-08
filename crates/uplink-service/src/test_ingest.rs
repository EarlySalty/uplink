use std::collections::BTreeMap;
use uplink_ingest::{EventKind, MediaEvent, WireTrack};

#[derive(serde::Serialize)]
struct Track {
    media: String,
    wire_id: u8,
    codec: String,
    headers: u64,
    frames: u64,
    metadata: u64,
    ends: u64,
    bytes: u64,
    last_dts_ms: u32,
    last_pts_ms: i64,
    header_revision: u64,
}

/// Transport observation only: no codec decoding, persistence or platform output.
/// Each event is consumed and released immediately; the ingest owns all bounds.
pub async fn receive(
    first: MediaEvent,
    mut events: tokio::sync::mpsc::Receiver<MediaEvent>,
) -> Result<(), &'static str> {
    let identity = first.identity;
    let reservation = first
        .authorization_retention()
        .and_then(|value| value.downcast::<crate::registry::Reservation>().ok())
        .ok_or("Sessionreservierung fehlt.")?;
    reservation.media_status(
        serde_json::json!({"publishing":false,"message":"Testeingang: keine Plattformausgabe."}),
    );
    let mut tracks = BTreeMap::<WireTrack, Track>::new();
    let mut event = Some(first);
    while let Some(received) = event.take() {
        if received.identity.session != identity.session
            || received.identity.generation != identity.generation
        {
            return Err("Testeingang hat eine fremde Sessiongeneration empfangen.");
        }
        let track = tracks
            .entry(received.identity.track)
            .or_insert_with(|| Track {
                media: format!("{:?}", received.identity.track.kind),
                wire_id: received.identity.track.wire_id,
                codec: format!("{:?}", received.codec),
                headers: 0,
                frames: 0,
                metadata: 0,
                ends: 0,
                bytes: 0,
                last_dts_ms: 0,
                last_pts_ms: 0,
                header_revision: 0,
            });
        match received.event_kind {
            EventKind::SequenceHeader => track.headers = track.headers.saturating_add(1),
            EventKind::Frame => track.frames = track.frames.saturating_add(1),
            EventKind::Metadata => track.metadata = track.metadata.saturating_add(1),
            EventKind::SequenceEnd => track.ends = track.ends.saturating_add(1),
        }
        track.codec = format!("{:?}", received.codec);
        track.bytes = track
            .bytes
            .saturating_add(received.wire_body().len() as u64);
        track.last_dts_ms = received.dts_ms;
        track.last_pts_ms = received.pts_ms;
        track.header_revision = received.configuration_revision;
        reservation.observation(serde_json::json!({"mode":"ingest_test","generation":format!("{:?}",identity.generation),
            "tracks":tracks.values().collect::<Vec<_>>(),"decoded":false,"resolution":null,"fps":null}));
        drop(received);
        event = events.recv().await;
    }
    Ok(())
}
