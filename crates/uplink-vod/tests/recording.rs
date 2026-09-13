use std::sync::Arc;
use uplink_ingest::{EventKind, MediaKind, WireCodec, WireTrack};
use uplink_media::flv::FlvTag;
use uplink_vod::{recording::*, *};
fn packet(generation: u64, kind: MediaKind, track: u8, header: bool) -> RecordingPacket {
    let body = if kind == MediaKind::Audio {
        if header {
            vec![0xaf, 0, 0x12, 0x10]
        } else {
            vec![0xaf, 1, 1, 2, 3]
        }
    } else if header {
        vec![0x17, 0, 0, 0, 0, 1]
    } else {
        vec![0x17, 1, 0, 0, 0, 1, 2, 3]
    };
    let tag = FlvTag::new(
        if kind == MediaKind::Audio { 8 } else { 9 },
        if header { 0 } else { 40 },
        body.into(),
        4096,
    )
    .unwrap();
    let tag = if kind == MediaKind::Audio {
        tag.with_audio_track(track, 4096).unwrap()
    } else {
        tag
    };
    RecordingPacket {
        generation,
        track: WireTrack {
            kind,
            wire_id: track,
        },
        codec: if kind == MediaKind::Audio {
            WireCodec::Aac
        } else {
            WireCodec::H264
        },
        event_kind: if header {
            EventKind::SequenceHeader
        } else {
            EventKind::Frame
        },
        configuration_revision: 1,
        pts_ms: 40,
        tag: Arc::new(tag),
    }
}
fn fixture() -> (tempfile::TempDir, Storage, RecordingSpec, RecordingConfig) {
    let tmp = tempfile::tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let storage = Storage::new(tmp.path().to_owned(), 32 * 1024 * 1024).unwrap();
    (
        tmp,
        storage,
        RecordingSpec {
            session_id: 1,
            streamer_id: 7,
            live_audio_track: 0,
            vod_audio_track: Some(7),
            metadata: serde_json::json!({"layout_revision":2}),
        },
        RecordingConfig {
            max_events: 32,
            max_queue_bytes: 4096,
            max_recording_bytes: 1024 * 1024,
            segment_bytes: 1024 * 256,
            max_segments: 16,
        },
    )
}
#[tokio::test]
async fn sparse_vod_track_bleibt_erhalten_und_fehlt_nie_still() {
    let (_tmp, storage, spec, cfg) = fixture();
    let running = Recorder::start(storage.clone(), spec, cfg).await.unwrap();
    let sink = running.sink();
    for p in [
        packet(1, MediaKind::Video, 0, true),
        packet(1, MediaKind::Audio, 7, true),
        packet(1, MediaKind::Video, 0, false),
        packet(1, MediaKind::Audio, 7, false),
    ] {
        sink.reserve(p.tag.wire_len()).unwrap().submit(p).unwrap()
    }
    let manifest = running.finish().await.unwrap();
    assert!(manifest.complete);
    assert!(manifest.has_vod_audio());
    assert_eq!(manifest.segments.len(), 1);
    let digest = storage
        .digest(&manifest.object_id, &manifest.segments[0].file)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&manifest.segments[0])
            .unwrap()
            .get("sha256")
            .and_then(serde_json::Value::as_str),
        Some(digest.sha256.as_str())
    );
    let mut reader = uplink_media::flv::FlvReader::new(
        storage
            .open(&manifest.object_id, &manifest.segments[0].file)
            .await
            .unwrap(),
        4096,
    );
    let mut audio = vec![];
    while let Some(tag) = reader.next().await.unwrap() {
        if let Some(track) = tag.audio_track().unwrap() {
            audio.push(track)
        }
    }
    assert_eq!(audio, vec![7, 7]);
    manifest.verify_segments(&storage).await.unwrap();
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    let mut corrupted = tokio::fs::OpenOptions::new()
        .write(true)
        .open(
            storage
                .path(&manifest.object_id, &manifest.segments[0].file)
                .unwrap(),
        )
        .await
        .unwrap();
    corrupted.seek(std::io::SeekFrom::Start(20)).await.unwrap();
    corrupted.write_all(&[0xff]).await.unwrap();
    corrupted.sync_all().await.unwrap();
    assert_eq!(
        manifest.verify_segments(&storage).await,
        Err(Error::Incomplete)
    );
}
#[tokio::test]
async fn volle_queue_und_fehlender_mix_sind_unvollstaendig() {
    let (_tmp, storage, spec, mut cfg) = fixture();
    cfg.max_queue_bytes = 64;
    let running = Recorder::start(storage, spec, cfg).await.unwrap();
    assert!(running.sink().reserve(65).is_err());
    let manifest = running.finish().await.unwrap();
    assert!(!manifest.complete);
    assert!(!manifest.has_vod_audio());
}
#[tokio::test]
async fn generationen_zeichnen_luecke_aus() {
    let (_tmp, storage, spec, cfg) = fixture();
    let running = Recorder::start(storage, spec, cfg).await.unwrap();
    let sink = running.sink();
    for p in [
        packet(1, MediaKind::Video, 0, true),
        packet(1, MediaKind::Video, 0, false),
        packet(2, MediaKind::Video, 0, true),
        packet(2, MediaKind::Video, 0, false),
    ] {
        sink.reserve(p.tag.wire_len()).unwrap().submit(p).unwrap()
    }
    let manifest = running.finish().await.unwrap();
    assert!(!manifest.complete);
    assert!(manifest.has_gaps);
    assert_eq!(manifest.segments.len(), 2);
}

#[tokio::test]
async fn abgebrochene_paketkopie_und_offenes_permit_verhindern_vollstaendigkeit() {
    for keep_open in [false, true] {
        let (_tmp, storage, spec, cfg) = fixture();
        let running = Recorder::start(storage, spec, cfg).await.unwrap();
        let sink = running.sink();
        for p in [
            packet(1, MediaKind::Video, 0, true),
            packet(1, MediaKind::Audio, 7, true),
            packet(1, MediaKind::Video, 0, false),
            packet(1, MediaKind::Audio, 7, false),
        ] {
            sink.reserve(p.tag.wire_len()).unwrap().submit(p).unwrap();
        }
        let pending = sink.reserve(16).unwrap();
        if keep_open {
            let manifest = running.finish().await.unwrap();
            assert!(!manifest.complete);
            drop(pending)
        } else {
            drop(pending);
            let manifest = running.finish().await.unwrap();
            assert!(!manifest.complete)
        }
    }
}

#[tokio::test]
async fn hevc_transport_metadaten_und_opaque_bytes_bleiben_erhalten() {
    let (_tmp, storage, spec, cfg) = fixture();
    let running = Recorder::start(storage.clone(), spec, cfg).await.unwrap();
    let sink = running.sink();
    // Künstliche E-FLV-Tags prüfen nur Speicherung und Codecbezeichnung.
    // Dies ist kein gültiger HEVC-Decoder-/YouTube-Exportnachweis.
    let bodies = [
        vec![0x90, b'h', b'v', b'c', b'1', 1],
        vec![0x93, b'h', b'v', b'c', b'1', 1, 2, 3],
    ];
    for (index, body) in bodies.iter().enumerate() {
        let mut p = packet(1, MediaKind::Video, 0, index == 0);
        p.codec = WireCodec::Hevc;
        p.tag = Arc::new(FlvTag::new(9, (index * 40) as u32, body.clone().into(), 4096).unwrap());
        sink.reserve(p.tag.wire_len()).unwrap().submit(p).unwrap();
    }
    let manifest = running.finish().await.unwrap();
    assert_eq!(manifest.tracks.len(), 1);
    assert_eq!(manifest.tracks[0].codec, "hevc");
    assert_eq!(manifest.tracks[0].headers, 1);
    assert_eq!(manifest.tracks[0].frames, 1);
    assert!(!manifest.has_vod_audio());
    let mut reader = uplink_media::flv::FlvReader::new(
        storage
            .open(&manifest.object_id, &manifest.segments[0].file)
            .await
            .unwrap(),
        4096,
    );
    for body in bodies {
        assert_eq!(reader.next().await.unwrap().unwrap().body(), body);
    }
    assert!(reader.next().await.unwrap().is_none());
    manifest.verify_segments(&storage).await.unwrap();
}
