use uplink_core::{
    Chroma, Codec, Color, ColorPrimaries, ColorRange, FrameRate, Gop, LayoutRevision, Matrix,
    RateControl, RateMode, Transfer, VideoProfile,
};
use uplink_media::portrait::{PortraitError, compile_portrait};
use uplink_media::{Composition, Crop, LayoutSpec};

fn portrait_profile(width: u32, height: u32) -> VideoProfile {
    VideoProfile {
        width,
        height,
        fps: FrameRate::new(60, 1).expect("positive frame rate"),
        codec: Codec::Hevc,
        codec_profile: "main".into(),
        level: "5.1".into(),
        bit_depth: 8,
        chroma: Chroma::Yuv420,
        color: Color {
            primaries: ColorPrimaries::Bt709,
            transfer: Transfer::Bt709,
            matrix: Matrix::Bt709,
            range: ColorRange::Limited,
        },
        rate: RateControl {
            mode: RateMode::HqCbr,
            target_kbps: 10_000,
            max_kbps: 10_000,
            buffer_kbits: 20_000,
        },
        gop: Gop {
            keyframe_interval_frames: 120,
            closed: true,
        },
    }
}

fn revision() -> LayoutRevision {
    LayoutRevision { id: 7, revision: 3 }
}

fn compiled(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    composition: Composition,
) -> Result<LayoutSpec, PortraitError> {
    compile_portrait(
        source_width,
        source_height,
        &portrait_profile(target_width, target_height),
        revision(),
        composition,
    )
}

fn every_error() -> [PortraitError; 9] {
    [
        PortraitError::InvalidDimensions,
        PortraitError::NotPortrait,
        PortraitError::GameplayCropOutside,
        PortraitError::GameplayCropGeometry,
        PortraitError::CameraCropOutside,
        PortraitError::CameraCropGeometry,
        PortraitError::CameraBoxOutside,
        PortraitError::CameraBoxGeometry,
        PortraitError::CameraHeightInvalid,
    ]
}

#[test]
fn happy_paths_keep_every_chosen_value_for_both_representative_pairs() {
    for (source_width, source_height, target_width, target_height) in [
        (2560u32, 1440u32, 1080u32, 1920u32),
        (1920, 1080, 720, 1280),
    ] {
        let compositions = vec![
            Composition::Crop(Crop {
                x: 8,
                y: 4,
                width: 1080,
                height: 1072,
            }),
            Composition::Stacked {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: 1280,
                    y: 128,
                    width: 640,
                    height: 640,
                },
                camera_height: 360,
            },
            Composition::PictureInPicture {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: 1280,
                    y: 128,
                    width: 640,
                    height: 640,
                },
                camera_box: Crop {
                    x: 0,
                    y: 8,
                    width: target_width,
                    height: 640,
                },
            },
        ];
        for composition in compositions {
            let spec = compiled(
                source_width,
                source_height,
                target_width,
                target_height,
                composition.clone(),
            )
            .unwrap_or_else(|error| {
                panic!("{source_width}x{source_height} to {target_width}x{target_height}: {error}")
            });
            assert_eq!(
                spec,
                LayoutSpec {
                    revision: revision(),
                    composition,
                }
            );
        }
    }
}

#[test]
fn exact_source_and_target_edges_count_as_inside() {
    let (source_width, source_height, target_width, target_height) =
        (2560u32, 1440u32, 1080u32, 1920u32);
    compiled(
        source_width,
        source_height,
        target_width,
        target_height,
        Composition::Crop(Crop {
            x: 0,
            y: 0,
            width: source_width,
            height: source_height,
        }),
    )
    .unwrap();
    compiled(
        source_width,
        source_height,
        target_width,
        target_height,
        Composition::PictureInPicture {
            gameplay: Crop {
                x: source_width - 1080,
                y: source_height - 1072,
                width: 1080,
                height: 1072,
            },
            camera: Crop {
                x: 0,
                y: 0,
                width: 640,
                height: 640,
            },
            camera_box: Crop {
                x: target_width - 720,
                y: target_height - 640,
                width: 720,
                height: 640,
            },
        },
    )
    .unwrap();
}

#[test]
fn overflowing_coordinates_are_rejected_without_panic() {
    let maximum = u32::MAX;
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::Crop(Crop {
                x: maximum - 1,
                y: 0,
                width: 2,
                height: 2,
            }),
        ),
        Err(PortraitError::GameplayCropOutside)
    );
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::Stacked {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: maximum,
                    y: maximum,
                    width: 2,
                    height: 2,
                },
                camera_height: 360,
            },
        ),
        Err(PortraitError::CameraCropOutside)
    );
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::PictureInPicture {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: 0,
                    y: 0,
                    width: 640,
                    height: 640,
                },
                camera_box: Crop {
                    x: maximum,
                    y: 0,
                    width: 2,
                    height: 2,
                },
            },
        ),
        Err(PortraitError::CameraBoxOutside)
    );
}

#[test]
fn out_of_bounds_crops_name_the_failing_place() {
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::Crop(Crop {
                x: 2560 - 1078,
                y: 0,
                width: 1080,
                height: 1072,
            }),
        ),
        Err(PortraitError::GameplayCropOutside)
    );
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::Stacked {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: 0,
                    y: 1440 - 638,
                    width: 640,
                    height: 640,
                },
                camera_height: 360,
            },
        ),
        Err(PortraitError::CameraCropOutside)
    );
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::PictureInPicture {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: 0,
                    y: 0,
                    width: 640,
                    height: 640,
                },
                camera_box: Crop {
                    x: 1080 - 718,
                    y: 0,
                    width: 720,
                    height: 640,
                },
            },
        ),
        Err(PortraitError::CameraBoxOutside)
    );
}

fn standard_gameplay() -> Crop {
    Crop {
        x: 0,
        y: 0,
        width: 1080,
        height: 1072,
    }
}

#[test]
fn empty_and_odd_areas_are_rejected_as_geometry() {
    for gameplay in [
        Crop {
            width: 0,
            ..standard_gameplay()
        },
        Crop {
            height: 1,
            ..standard_gameplay()
        },
        Crop {
            x: 1,
            ..standard_gameplay()
        },
        Crop {
            y: 3,
            ..standard_gameplay()
        },
        Crop {
            width: 1081,
            ..standard_gameplay()
        },
        Crop {
            height: 1073,
            ..standard_gameplay()
        },
    ] {
        assert_eq!(
            compiled(2560, 1440, 1080, 1920, Composition::Crop(gameplay)),
            Err(PortraitError::GameplayCropGeometry),
            "crop {gameplay:?}"
        );
    }
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::PictureInPicture {
                gameplay: standard_gameplay(),
                camera: Crop {
                    x: 0,
                    y: 0,
                    width: 640,
                    height: 640,
                },
                camera_box: Crop {
                    x: 0,
                    y: 8,
                    width: 719,
                    height: 640,
                },
            },
        ),
        Err(PortraitError::CameraBoxGeometry)
    );
    assert_eq!(
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::Stacked {
                gameplay: standard_gameplay(),
                camera: Crop {
                    x: 0,
                    y: 0,
                    width: 641,
                    height: 640,
                },
                camera_height: 360,
            },
        ),
        Err(PortraitError::CameraCropGeometry)
    );
}

#[test]
fn camera_height_must_leave_a_valid_surface_for_both_sides() {
    for camera_height in [0u32, 1, 3, 1918, 1919, 1920, 1922, u32::MAX] {
        assert_eq!(
            compiled(
                2560,
                1440,
                1080,
                1920,
                Composition::Stacked {
                    gameplay: Crop {
                        x: 0,
                        y: 0,
                        width: 1080,
                        height: 1072,
                    },
                    camera: Crop {
                        x: 1280,
                        y: 128,
                        width: 640,
                        height: 640,
                    },
                    camera_height,
                },
            ),
            Err(PortraitError::CameraHeightInvalid),
            "camera_height {camera_height}"
        );
    }
    for camera_height in [2u32, 360, 1916] {
        compiled(
            2560,
            1440,
            1080,
            1920,
            Composition::Stacked {
                gameplay: Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                },
                camera: Crop {
                    x: 1280,
                    y: 128,
                    width: 640,
                    height: 640,
                },
                camera_height,
            },
        )
        .unwrap_or_else(|error| panic!("camera_height {camera_height}: {error}"));
    }
}

#[test]
fn landscape_and_square_targets_are_not_portrait() {
    for (target_width, target_height) in [(1920u32, 1080u32), (1000, 1000), (1921, 1920)] {
        assert_eq!(
            compiled(
                2560,
                1440,
                target_width,
                target_height,
                Composition::Crop(Crop {
                    x: 0,
                    y: 0,
                    width: 1080,
                    height: 1072,
                }),
            ),
            Err(PortraitError::NotPortrait),
            "target {target_width}x{target_height}"
        );
    }
}

#[test]
fn zero_dimensions_are_rejected_before_geometry() {
    for (source_width, source_height, target_width, target_height) in [
        (0u32, 1440u32, 1080u32, 1920u32),
        (2560, 0, 1080, 1920),
        (2560, 1440, 0, 1920),
        (2560, 1440, 1080, 0),
        (0, 0, 0, 0),
    ] {
        assert_eq!(
            compiled(
                source_width,
                source_height,
                target_width,
                target_height,
                Composition::Crop(Crop {
                    x: u32::MAX,
                    y: 0,
                    width: 0,
                    height: 0,
                }),
            ),
            Err(PortraitError::InvalidDimensions),
            "source {source_width}x{source_height} target {target_width}x{target_height}"
        );
    }
}

#[test]
fn non_portrait_sources_stay_usable() {
    compiled(
        1080,
        1920,
        1080,
        1920,
        Composition::Crop(Crop {
            x: 0,
            y: 0,
            width: 1080,
            height: 1920,
        }),
    )
    .unwrap();
    compiled(
        640,
        640,
        720,
        1280,
        Composition::Stacked {
            gameplay: Crop {
                x: 0,
                y: 0,
                width: 640,
                height: 512,
            },
            camera: Crop {
                x: 0,
                y: 512,
                width: 640,
                height: 128,
            },
            camera_height: 320,
        },
    )
    .unwrap();
}

#[test]
fn error_texts_name_reasons_without_coordinates_or_endpoints() {
    for error in every_error() {
        let text = error.to_string();
        assert!(!text.trim().is_empty());
        for coordinate in ["2560", "1440", "1920", "1080", "720", "1280", "://"] {
            assert!(!text.contains(coordinate), "{text:?} contains {coordinate}");
        }
        let debug = format!("{error:?}");
        assert!(!debug.contains("://"));
        let as_error: &dyn std::error::Error = &error;
        assert!(as_error.source().is_none());
    }
}
