use crate::{Composition, Crop, LayoutSpec};
use serde::Serialize;
use std::fmt;
use uplink_core::{LayoutRevision, VideoProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortraitError {
    InvalidDimensions,
    NotPortrait,
    GameplayCropOutside,
    GameplayCropGeometry,
    CameraCropOutside,
    CameraCropGeometry,
    CameraBoxOutside,
    CameraBoxGeometry,
    CameraHeightInvalid,
}

impl fmt::Display for PortraitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidDimensions => "Quell- oder Zielabmessung hat keine Fläche",
            Self::NotPortrait => "Zielabmessung ist nicht Hochkant",
            Self::GameplayCropOutside => {
                "Gameplay-Ausschnitt liegt nicht vollständig in der Quelle"
            }
            Self::GameplayCropGeometry => "Gameplay-Ausschnitt hat keine gültige YUV420-Fläche",
            Self::CameraCropOutside => "Kamera-Ausschnitt liegt nicht vollständig in der Quelle",
            Self::CameraCropGeometry => "Kamera-Ausschnitt hat keine gültige YUV420-Fläche",
            Self::CameraBoxOutside => "Kamera-Ausgabebox liegt nicht vollständig im Ziel",
            Self::CameraBoxGeometry => "Kamera-Ausgabebox hat keine gültige YUV420-Fläche",
            Self::CameraHeightInvalid => {
                "Kamerahöhe lässt Gameplay und Kamera keinen gültigen Platz im Ziel"
            }
        })
    }
}

impl std::error::Error for PortraitError {}

pub fn compile_portrait(
    source_width: u32,
    source_height: u32,
    output: &VideoProfile,
    revision: LayoutRevision,
    composition: Composition,
) -> Result<LayoutSpec, PortraitError> {
    if source_width == 0 || source_height == 0 || output.width == 0 || output.height == 0 {
        return Err(PortraitError::InvalidDimensions);
    }
    if output.height <= output.width {
        return Err(PortraitError::NotPortrait);
    }
    let source = (source_width, source_height);
    let target = (output.width, output.height);
    match composition {
        Composition::Crop(gameplay) => {
            check(CropRole::Gameplay, gameplay, source)?;
        }
        Composition::Stacked {
            gameplay,
            camera,
            camera_height,
        } => {
            check(CropRole::Gameplay, gameplay, source)?;
            check(CropRole::CameraSource, camera, source)?;
            check_camera_height(target.1, camera_height)?;
        }
        Composition::PictureInPicture {
            gameplay,
            camera,
            camera_box,
        } => {
            check(CropRole::Gameplay, gameplay, source)?;
            check(CropRole::CameraSource, camera, source)?;
            check(CropRole::CameraBox, camera_box, target)?;
        }
    }
    Ok(LayoutSpec {
        revision,
        composition,
    })
}

enum CropFault {
    Outside,
    Geometry,
}

enum CropRole {
    Gameplay,
    CameraSource,
    CameraBox,
}

fn crop_fault(crop: Crop, bound: (u32, u32)) -> Option<CropFault> {
    let inside = crop
        .x
        .checked_add(crop.width)
        .is_some_and(|end| end <= bound.0)
        && crop
            .y
            .checked_add(crop.height)
            .is_some_and(|end| end <= bound.1);
    if !inside {
        return Some(CropFault::Outside);
    }
    let even = crop.x.is_multiple_of(2)
        && crop.y.is_multiple_of(2)
        && crop.width.is_multiple_of(2)
        && crop.height.is_multiple_of(2);
    if even && crop.width >= 2 && crop.height >= 2 {
        None
    } else {
        Some(CropFault::Geometry)
    }
}

fn crop_error(role: CropRole, fault: CropFault) -> PortraitError {
    match (role, fault) {
        (CropRole::Gameplay, CropFault::Outside) => PortraitError::GameplayCropOutside,
        (CropRole::Gameplay, CropFault::Geometry) => PortraitError::GameplayCropGeometry,
        (CropRole::CameraSource, CropFault::Outside) => PortraitError::CameraCropOutside,
        (CropRole::CameraSource, CropFault::Geometry) => PortraitError::CameraCropGeometry,
        (CropRole::CameraBox, CropFault::Outside) => PortraitError::CameraBoxOutside,
        (CropRole::CameraBox, CropFault::Geometry) => PortraitError::CameraBoxGeometry,
    }
}

fn check(role: CropRole, crop: Crop, bound: (u32, u32)) -> Result<(), PortraitError> {
    match crop_fault(crop, bound) {
        Some(fault) => Err(crop_error(role, fault)),
        None => Ok(()),
    }
}

fn check_camera_height(target_height: u32, camera_height: u32) -> Result<(), PortraitError> {
    let gameplay_room = target_height
        .checked_sub(2)
        .ok_or(PortraitError::CameraHeightInvalid)?;
    if camera_height < 2 || camera_height >= gameplay_room || !camera_height.is_multiple_of(2) {
        return Err(PortraitError::CameraHeightInvalid);
    }
    Ok(())
}
