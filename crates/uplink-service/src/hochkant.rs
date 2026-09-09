use serde::Deserialize;
use uplink_media::{Composition, Crop};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Rahmen {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Band {
    pub hoehe: f64,
    pub lage: Lage,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Lage {
    Oben,
    Unten,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Modus {
    NurGameplay,
    Gestapelt,
    BildImBild,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HochkantLayout {
    pub version: u8,
    pub modus: Modus,
    pub gameplay: Rahmen,
    pub kamera: Option<Rahmen>,
    #[serde(default)]
    pub kamera_band: Option<Band>,
    #[serde(default)]
    pub kamera_box: Option<Rahmen>,
}

const KANTE: f64 = 0.05;

fn rahmen_in_flaeche(rahmen: &Rahmen) -> bool {
    rahmen.x.is_finite()
        && rahmen.y.is_finite()
        && rahmen.x >= 0.0
        && rahmen.y >= 0.0
        && rahmen.w.is_finite()
        && rahmen.h.is_finite()
        && rahmen.w >= KANTE
        && rahmen.h >= KANTE
        && rahmen.x + rahmen.w <= 1.0 + f64::EPSILON
        && rahmen.y + rahmen.h <= 1.0 + f64::EPSILON
}

fn gerade(wert: f64) -> u32 {
    let gerundet = wert.round();
    if gerundet < 0.0 || !gerundet.is_finite() {
        return 0;
    }
    (gerundet as u32) & !1
}

fn ausschnitt(
    rahmen_x: f64,
    rahmen_y: f64,
    rahmen_w: f64,
    rahmen_h: f64,
    flaeche_b: u32,
    flaeche_h: u32,
) -> Crop {
    let mut breite_pixel = gerade(rahmen_w * f64::from(flaeche_b))
        .max(2)
        .min(flaeche_b);
    let mut hoehe_pixel = gerade(rahmen_h * f64::from(flaeche_h))
        .max(2)
        .min(flaeche_h);
    if !breite_pixel.is_multiple_of(2) {
        breite_pixel -= 1;
    }
    if !hoehe_pixel.is_multiple_of(2) {
        hoehe_pixel -= 1;
    }
    breite_pixel = breite_pixel.max(2).min(flaeche_b);
    hoehe_pixel = hoehe_pixel.max(2).min(flaeche_h);
    let x_pixel = gerade(rahmen_x * f64::from(flaeche_b)).min(flaeche_b - breite_pixel);
    let y_pixel = gerade(rahmen_y * f64::from(flaeche_h)).min(flaeche_h - hoehe_pixel);
    Crop {
        x: x_pixel,
        y: y_pixel,
        width: breite_pixel,
        height: hoehe_pixel,
    }
}

impl HochkantLayout {
    pub fn pruefen(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("Hochkantformat wird in dieser Version nicht verstanden.");
        }
        if !rahmen_in_flaeche(&self.gameplay) {
            return Err("Der Gameplay-Ausschnitt ist unvollständig oder zu klein.");
        }
        match self.modus {
            Modus::NurGameplay => {
                if self.kamera.is_some() || self.kamera_band.is_some() || self.kamera_box.is_some()
                {
                    return Err("Kameraangaben gehören nicht zum Modus nur Gameplay.");
                }
            }
            Modus::Gestapelt => {
                let Some(kamera) = &self.kamera else {
                    return Err("Der Modus Gestapelt braucht eine Kamera.");
                };
                if self.kamera_box.is_some() {
                    return Err("Eine Kamerabox gehört nur zum Modus Bild im Bild.");
                }
                if !rahmen_in_flaeche(kamera) {
                    return Err("Der Kamera-Ausschnitt ist unvollständig oder zu klein.");
                }
                let Some(band) = self.kamera_band else {
                    return Err("Der Modus Gestapelt braucht eine Bandhöhe.");
                };
                if !(0.1..=0.5).contains(&band.hoehe) || band.lage != Lage::Unten {
                    return Err(
                        "Die Bandhöhe liegt außerhalb des erlaubten Bereichs oder die Bandlage wird noch nicht unterstützt.",
                    );
                }
            }
            Modus::BildImBild => {
                let Some(kamera) = &self.kamera else {
                    return Err("Der Modus Bild im Bild braucht eine Kamera.");
                };
                if self.kamera_band.is_some() {
                    return Err("Ein Kameraband gehört nur zum Modus Gestapelt.");
                }
                if !rahmen_in_flaeche(kamera) {
                    return Err("Der Kamera-Ausschnitt ist unvollständig oder zu klein.");
                }
                let Some(kamera_box) = &self.kamera_box else {
                    return Err("Der Modus Bild im Bild braucht eine Kamerabox.");
                };
                if !rahmen_in_flaeche(kamera_box) {
                    return Err("Die Kamerabox liegt außerhalb des Zielbilds oder ist zu klein.");
                }
            }
        }
        Ok(())
    }

    pub fn kompiliere(
        &self,
        quelle_breite: u32,
        quelle_hoehe: u32,
        ziel_breite: u32,
        ziel_hoehe: u32,
    ) -> Result<Composition, &'static str> {
        self.pruefen()?;
        if quelle_breite == 0 || quelle_hoehe == 0 || ziel_breite == 0 || ziel_hoehe == 0 {
            return Err("Quelle oder Hochkantziel hat keine Fläche.");
        }
        let gameplay = ausschnitt(
            self.gameplay.x,
            self.gameplay.y,
            self.gameplay.w,
            self.gameplay.h,
            quelle_breite,
            quelle_hoehe,
        );
        Ok(match self.modus {
            Modus::NurGameplay => Composition::Crop(gameplay),
            Modus::Gestapelt => {
                let kamera_rahmen = self
                    .kamera
                    .as_ref()
                    .ok_or("Der Modus Gestapelt braucht eine Kamera.")?;
                let kamera = ausschnitt(
                    kamera_rahmen.x,
                    kamera_rahmen.y,
                    kamera_rahmen.w,
                    kamera_rahmen.h,
                    quelle_breite,
                    quelle_hoehe,
                );
                let band = self
                    .kamera_band
                    .ok_or("Der Modus Gestapelt braucht eine Bandhöhe.")?;
                Composition::Stacked {
                    gameplay,
                    camera: kamera,
                    camera_height: gerade(band.hoehe * f64::from(ziel_hoehe)),
                }
            }
            Modus::BildImBild => {
                let kamera_rahmen = self
                    .kamera
                    .as_ref()
                    .ok_or("Der Modus Bild im Bild braucht eine Kamera.")?;
                let kamera = ausschnitt(
                    kamera_rahmen.x,
                    kamera_rahmen.y,
                    kamera_rahmen.w,
                    kamera_rahmen.h,
                    quelle_breite,
                    quelle_hoehe,
                );
                let box_rahmen = self
                    .kamera_box
                    .as_ref()
                    .ok_or("Der Modus Bild im Bild braucht eine Kamerabox.")?;
                Composition::PictureInPicture {
                    gameplay,
                    camera: kamera,
                    camera_box: ausschnitt(
                        box_rahmen.x,
                        box_rahmen.y,
                        box_rahmen.w,
                        box_rahmen.h,
                        ziel_breite,
                        ziel_hoehe,
                    ),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rahmen(x: f64, y: f64, w: f64, h: f64) -> Rahmen {
        Rahmen { x, y, w, h }
    }

    fn nur_gameplay() -> HochkantLayout {
        HochkantLayout {
            version: 1,
            modus: Modus::NurGameplay,
            gameplay: rahmen(0.05, 0.05, 0.9, 0.9),
            kamera: None,
            kamera_band: None,
            kamera_box: None,
        }
    }

    fn gestapelt() -> HochkantLayout {
        HochkantLayout {
            version: 1,
            modus: Modus::Gestapelt,
            gameplay: rahmen(0.0, 0.0, 1.0, 1.0),
            kamera: Some(rahmen(0.6, 0.05, 0.35, 0.2)),
            kamera_band: Some(Band {
                hoehe: 0.25,
                lage: Lage::Unten,
            }),
            kamera_box: None,
        }
    }

    fn bild_im_bild() -> HochkantLayout {
        HochkantLayout {
            version: 1,
            modus: Modus::BildImBild,
            gameplay: rahmen(0.0, 0.0, 1.0, 1.0),
            kamera: Some(rahmen(0.78, 0.05, 0.2, 0.1125)),
            kamera_band: None,
            kamera_box: Some(rahmen(0.62, 0.04, 0.35, 0.196)),
        }
    }

    #[test]
    fn version_und_modus_angaben_werden_geprueft() {
        assert!(nur_gameplay().pruefen().is_ok());
        let mut fremd = nur_gameplay();
        fremd.version = 2;
        assert!(fremd.pruefen().is_err());
        let mut mit_kamera = nur_gameplay();
        mit_kamera.kamera = Some(rahmen(0.1, 0.1, 0.2, 0.2));
        assert!(mit_kamera.pruefen().is_err());
    }

    #[test]
    fn json_mit_fremden_feldern_wird_abgewiesen() {
        let roh = json!({
            "version": 1, "modus": "nur_gameplay",
            "gameplay": {"x": 0.05, "y": 0.05, "w": 0.9, "h": 0.9},
            "kamera": null, "kameraBand": null, "kameraBox": null, "kommentar": "x"
        });
        let layout: Result<HochkantLayout, _> = serde_json::from_value(roh);
        assert!(layout.is_err());
    }

    #[test]
    fn kamera_angaben_je_modus_sind_gebunden() {
        let mut ohne = gestapelt();
        ohne.kamera = None;
        assert!(ohne.pruefen().is_err());
        let mut ohne_band = gestapelt();
        ohne_band.kamera_band = None;
        assert!(ohne_band.pruefen().is_err());
        let mut mit_box = gestapelt();
        mit_box.kamera_box = Some(rahmen(0.1, 0.1, 0.3, 0.3));
        assert!(mit_box.pruefen().is_err());
        let mut mit_band = bild_im_bild();
        mit_band.kamera_band = Some(Band {
            hoehe: 0.2,
            lage: Lage::Unten,
        });
        assert!(mit_band.pruefen().is_err());
    }

    #[test]
    fn bandlage_oben_und_falsche_hoehe_werden_abgewiesen() {
        let mut layout = gestapelt();
        layout.kamera_band = Some(Band {
            hoehe: 0.25,
            lage: Lage::Oben,
        });
        assert!(layout.pruefen().is_err());
        let mut klein = gestapelt();
        klein.kamera_band = Some(Band {
            hoehe: 0.05,
            lage: Lage::Unten,
        });
        assert!(klein.pruefen().is_err());
        let mut gross = gestapelt();
        gross.kamera_band = Some(Band {
            hoehe: 0.6,
            lage: Lage::Unten,
        });
        assert!(gross.pruefen().is_err());
    }

    #[test]
    fn rahmen_ausserhalb_oder_zu_klein_werden_abgewiesen() {
        let mut draussen = nur_gameplay();
        draussen.gameplay = rahmen(0.2, 0.2, 0.9, 0.9);
        assert!(draussen.pruefen().is_err());
        let mut klein = nur_gameplay();
        klein.gameplay = rahmen(0.05, 0.05, 0.01, 0.9);
        assert!(klein.pruefen().is_err());
        let mut unendlich = nur_gameplay();
        unendlich.gameplay = rahmen(f64::NAN, 0.05, 0.9, 0.9);
        assert!(unendlich.pruefen().is_err());
        let mut ohne_kamera_feld = bild_im_bild();
        ohne_kamera_feld.kamera = None;
        assert!(ohne_kamera_feld.pruefen().is_err());
        let mut ohne_box = bild_im_bild();
        ohne_box.kamera_box = None;
        assert!(ohne_box.pruefen().is_err());
    }

    #[test]
    fn nur_gameplay_rechnet_gerade_quellpixel() {
        let composition = nur_gameplay().kompiliere(1920, 1080, 1080, 1920).unwrap();
        let Composition::Crop(crop) = composition else {
            panic!("nur_gameplay ergibt einen Ausschnitt");
        };
        assert_eq!(
            crop,
            Crop {
                x: 96,
                y: 54,
                width: 1728,
                height: 972
            }
        );
    }

    #[test]
    fn gestapelt_rechnet_kamerahohe_auf_zielhoehe() {
        let composition = gestapelt().kompiliere(1920, 1080, 1080, 1920).unwrap();
        let Composition::Stacked { camera_height, .. } = composition else {
            panic!("gestapelt ergibt eine Stapelung");
        };
        assert_eq!(camera_height, 480);
    }

    #[test]
    fn bild_im_bild_rechnet_box_im_zielbild() {
        let composition = bild_im_bild().kompiliere(1920, 1080, 1080, 1920).unwrap();
        let Composition::PictureInPicture { camera_box, .. } = composition else {
            panic!("bild_im_bild ergibt Bild im Bild");
        };
        assert_eq!(camera_box.x % 2, 0);
        assert_eq!(camera_box.y % 2, 0);
        assert_eq!(camera_box.width % 2, 0);
        assert_eq!(camera_box.height % 2, 0);
        assert!(camera_box.x + camera_box.width <= 1080);
        assert!(camera_box.y + camera_box.height <= 1920);
    }

    #[test]
    fn positionen_werden_in_die_flaeche_zurueckgeschoben() {
        let mut layout = nur_gameplay();
        layout.gameplay = rahmen(0.9, 0.9, 0.1, 0.1);
        let composition = layout.kompiliere(1920, 1080, 1080, 1920).unwrap();
        let Composition::Crop(crop) = composition else {
            panic!("nur_gameplay ergibt einen Ausschnitt");
        };
        assert!(crop.x + crop.width <= 1920);
        assert!(crop.y + crop.height <= 1080);
        assert!(crop.width >= 2 && crop.height >= 2);
    }

    #[test]
    fn kompilieren_verlangt_geprueftes_layout() {
        let mut fremd = nur_gameplay();
        fremd.version = 9;
        assert!(fremd.kompiliere(1920, 1080, 1080, 1920).is_err());
        assert!(nur_gameplay().kompiliere(0, 1080, 1080, 1920).is_err());
    }
}
