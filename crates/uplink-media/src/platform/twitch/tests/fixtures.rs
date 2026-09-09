use crate::platform::twitch::{Canvas, Preferences, Rational};
use serde_json::{Value, json};

pub fn fps(numerator: u32) -> Rational {
    Rational {
        numerator,
        denominator: 1,
    }
}

pub fn preferences_with(maximum_aggregate_bitrate: u64, maximum_video_tracks: u32) -> Preferences {
    Preferences {
        maximum_aggregate_bitrate,
        maximum_video_tracks,
        vod_track_audio: true,
        audio_samples_per_sec: 48000,
        audio_channels: 2,
        audio_max_buffering_ms: 200,
        audio_fixed_buffering: true,
        canvases: vec![Canvas {
            width: 2560,
            height: 1440,
            canvas_width: 2560,
            canvas_height: 1440,
            framerate: fps(60),
        }],
    }
}

pub fn preferences() -> Preferences {
    preferences_with(8_000, 3)
}

pub fn dual_canvas_preferences() -> Preferences {
    let mut value = preferences_with(12_000, 4);
    value.canvases.push(Canvas {
        width: 1080,
        height: 1920,
        canvas_width: 1080,
        canvas_height: 1920,
        framerate: fps(30),
    });
    value
}

pub fn x264_quality(
    width: u32,
    height: u32,
    canvas_index: usize,
    framerate: Rational,
    bitrate_kbps: u32,
) -> Value {
    json!({
        "type": "obs_x264",
        "width": width,
        "height": height,
        "framerate": {
            "numerator": framerate.numerator,
            "denominator": framerate.denominator
        },
        "canvas_index": canvas_index,
        "bitrate_interpolation_points": [0, bitrate_kbps / 2, bitrate_kbps],
        "gpu_scale_type": "OBS_SCALE_BICUBIC",
        "settings": {
            "rate_control": "CBR",
            "bitrate": bitrate_kbps,
            "keyint_sec": 2,
            "profile": "high",
            "bf": 0,
            "preset": "veryfast",
            "tune": "zerolatency",
            "x264opts": "",
            "lookahead": false,
            "adaptive_quantization": false,
            "multipass": "qres",
            "opts": ""
        }
    })
}

pub fn hardware_quality(encoder: &str, width: u32, height: u32, bitrate_kbps: u32) -> Value {
    json!({
        "type": encoder,
        "width": width,
        "height": height,
        "framerate": {"numerator": 60, "denominator": 1},
        "canvas_index": 0,
        "bitrate_interpolation_points": [0, bitrate_kbps / 2, bitrate_kbps],
        "gpu_scale_type": "OBS_SCALE_BICUBIC",
        "settings": {
            "rate_control": "CBR",
            "bitrate": bitrate_kbps,
            "keyint_sec": 2,
            "profile": "high",
            "bf": 0,
            "preset": "veryfast",
            "tune": "zerolatency",
            "lookahead": true,
            "adaptive_quantization": true,
            "multipass": "qres"
        }
    })
}

pub fn audio_track(track_id: u32, bitrate_kbps: u32) -> Value {
    json!({
        "codec": "aac",
        "track_id": track_id,
        "channels": 2,
        "settings": {"bitrate": bitrate_kbps}
    })
}

pub fn build_response(encoders: &[Value], live: &[Value], vod: &[Value]) -> Vec<u8> {
    let mut response = json!({
        "meta": {
            "service": "IVS",
            "schema_version": "2025-01-25",
            "config_id": "synthetic-config",
            "required_encode_resource_estimate_percent": 40
        },
        "status": {"result": "success"},
        "ingest_endpoints": [{
            "protocol": "RTMPS",
            "url_template": "rtmps://test.example/app/{stream_key}",
            "authentication": "synthetic-temporary"
        }],
        "audio_configurations": {"live": [], "vod": []}
    });
    response["encoder_configurations"] = Value::Array(encoders.to_vec());
    response["audio_configurations"]["live"] = Value::Array(live.to_vec());
    response["audio_configurations"]["vod"] = Value::Array(vod.to_vec());
    response.to_string().into_bytes()
}

pub fn response() -> Vec<u8> {
    br#"{"meta":{"service":"IVS","schema_version":"2025-01-25","config_id":"synthetic-config"},"status":{"result":"success"},"ingest_endpoints":[{"protocol":"RTMPS","url_template":"rtmps://test.example/app/{stream_key}","authentication":"synthetic-temporary"}],"encoder_configurations":[{"type":"obs_x264","width":1920,"height":1080,"framerate":{"numerator":60,"denominator":1},"canvas_index":0,"settings":{"rate_control":"CBR","bitrate":6000,"keyint_sec":2,"profile":"high","bf":0}}],"audio_configurations":{"live":[{"codec":"aac","track_id":0,"channels":2,"settings":{"bitrate":160}}],"vod":[{"codec":"aac","track_id":1,"channels":2,"settings":{"bitrate":160}}]}}"#.to_vec()
}

pub fn key() -> crate::PublishSecret {
    crate::PublishSecret::new(b"synthetic-original?bandwidthtest=true".to_vec()).unwrap()
}

pub fn hosts() -> Vec<String> {
    vec!["test.example".into()]
}
