//! Profile-based soft proofing through LittleCMS.
//!
//! The document is sRGB. A proof simulates an output profile on screen:
//! document sRGB -> output profile -> display sRGB, with an explicit rendering
//! intent. This is real colour management, unlike the fixed-coefficient
//! approximation in `color.rs`, and it never touches the document pixels.

use anyhow::{anyhow, Result};
use lcms2::{Flags, Intent as LcmsIntent, PixelFormat, Profile, Transform};

use crate::render::Image;

/// ICC rendering intents the editor exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

impl Intent {
    fn lcms(self) -> LcmsIntent {
        match self {
            Intent::Perceptual => LcmsIntent::Perceptual,
            Intent::RelativeColorimetric => LcmsIntent::RelativeColorimetric,
            Intent::Saturation => LcmsIntent::Saturation,
            Intent::AbsoluteColorimetric => LcmsIntent::AbsoluteColorimetric,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Intent::Perceptual => "perceptual",
            Intent::RelativeColorimetric => "relative colorimetric",
            Intent::Saturation => "saturation",
            Intent::AbsoluteColorimetric => "absolute colorimetric",
        }
    }
}

/// One proofing simulation. Build it once and reuse it for every repaint; the
/// transform is not cheap to create.
pub struct Proof {
    transform: Transform<u8, u8>,
    pub intent: Intent,
    /// The proof-to-display leg uses absolute colorimetric.
    pub simulate_paper: bool,
}

impl Proof {
    /// Build the simulation from the output profile's bytes. An unreadable or
    /// invalid profile is an error, never a silent fallback: the caller shows
    /// the problem instead of a fabricated print preview.
    pub fn new(output_profile: &[u8], intent: Intent) -> Result<Self> {
        Self::new_with_source(None, output_profile, intent)
    }

    /// Build the simulation with an explicit document profile.
    ///
    /// `None` keeps the historical assumption that the document is sRGB, which
    /// is what the editor writes. A document opened from a PSD with an
    /// embedded profile passes that profile so the proof starts from the real
    /// source colour space. An unreadable source profile is an error, not a
    /// silent sRGB substitution.
    pub fn new_with_source(
        source_profile: Option<&[u8]>,
        output_profile: &[u8],
        intent: Intent,
    ) -> Result<Self> {
        Self::new_with_options(source_profile, output_profile, intent, false)
    }

    /// Same as [`Proof::new_with_source`], with an explicit paper simulation.
    ///
    /// Paper simulation means the proof-to-display leg uses absolute
    /// colorimetric, so the simulated paper white is shown instead of being
    /// normalised back to the display white. That is the difference between
    /// "how the inks look" and "how the printed sheet looks in this room".
    pub fn new_with_options(
        source_profile: Option<&[u8]>,
        output_profile: &[u8],
        intent: Intent,
        simulate_paper: bool,
    ) -> Result<Self> {
        let proof = Profile::new_icc(output_profile)
            .map_err(|error| anyhow!("invalid output profile: {error}"))?;
        let source = match source_profile {
            Some(bytes) => {
                let profile = Profile::new_icc(bytes)
                    .map_err(|error| anyhow!("invalid document profile: {error}"))?;
                // The document pixels reach this transform as 8-bit RGB, so a
                // grey or CMYK source profile would need a separate input
                // path; refuse it instead of feeding it the wrong samples.
                if profile.color_space() != lcms2::ColorSpaceSignature::RgbData {
                    return Err(anyhow!(
                        "unsupported document profile colour space {:?}",
                        profile.color_space()
                    ));
                }
                profile
            }
            None => Profile::new_srgb(),
        };
        let display = Profile::new_srgb();
        let proof_intent = if simulate_paper {
            Intent::AbsoluteColorimetric
        } else {
            intent
        };
        let transform = Transform::new_proofing(
            &source,
            PixelFormat::RGB_8,
            &display,
            PixelFormat::RGB_8,
            &proof,
            intent.lcms(),
            proof_intent.lcms(),
            Flags::SOFT_PROOFING,
        )
        .map_err(|error| anyhow!("could not build the proof transform: {error}"))?;
        Ok(Self {
            transform,
            intent,
            simulate_paper,
        })
    }

    /// Proven RGB result for one colour, in display space.
    pub fn proof_pixel(&self, rgb: [u8; 3]) -> [u8; 3] {
        let mut out = [0u8; 3];
        self.transform.transform_pixels(&rgb, &mut out);
        out
    }

    /// A proofed copy of `image`. Alpha is carried through untouched.
    pub fn apply(&self, image: &Image) -> Image {
        let pixels = (image.data.len() / 4).min(
            u32::MAX as usize,
        );
        let mut rgb = Vec::with_capacity(pixels * 3);
        for pixel in image.data.chunks_exact(4) {
            rgb.extend_from_slice(&pixel[..3]);
        }
        let mut translated = vec![0u8; pixels * 3];
        self.transform.transform_pixels(&rgb, &mut translated);
        let mut data = Vec::with_capacity(pixels * 4);
        for (index, pixel) in image.data.chunks_exact(4).enumerate() {
            data.extend_from_slice(&translated[index * 3..index * 3 + 3]);
            data.push(pixel[3]);
        }
        Image {
            width: image.width,
            height: image.height,
            data,
        }
    }
}

/// A profile the tests and the UI can use without shipping data files: sRGB
/// serialized by LittleCMS.
pub fn srgb_profile_bytes() -> Vec<u8> {
    Profile::new_srgb()
        .icc()
        .expect("LittleCMS can always serialize sRGB")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_of(pixels: &[[u8; 4]]) -> Image {
        Image {
            width: pixels.len() as u32,
            height: 1,
            data: pixels.iter().flatten().copied().collect(),
        }
    }

    #[test]
    fn srgb_output_profile_is_an_identity_proof() {
        let proof = Proof::new(&srgb_profile_bytes(), Intent::RelativeColorimetric).unwrap();
        for colour in [[30, 120, 220], [0, 0, 0], [255, 255, 255], [200, 40, 90]] {
            let proofed = proof.proof_pixel(colour);
            for channel in 0..3 {
                let difference = (i32::from(proofed[channel]) - i32::from(colour[channel])).abs();
                assert!(
                    difference <= 2,
                    "{colour:?} -> {proofed:?} moved {} levels",
                    difference
                );
            }
        }
    }

    #[test]
    fn proof_preserves_alpha_and_the_source_image() {
        let image = image_of(&[[30, 120, 220, 200], [10, 20, 30, 0]]);
        let before = image.data.clone();
        let proof = Proof::new(&srgb_profile_bytes(), Intent::Perceptual).unwrap();
        let proofed = proof.apply(&image);
        assert_eq!(image.data, before, "proofing must not touch the document");
        assert_eq!(proofed.data[3], 200);
        assert_eq!(proofed.data[7], 0);
        assert_eq!(proofed.data.len(), before.len());
    }

    #[test]
    fn an_invalid_profile_is_rejected_instead_of_approximated() {
        let error = Proof::new(b"not an ICC profile", Intent::Perceptual)
            .err()
            .expect("invalid profiles must fail");
        assert!(error.to_string().contains("invalid output profile"), "{error}");
    }

    /// An RGB profile with sRGB primaries but a linear tone curve: valid as a
    /// document profile and visibly different from sRGB.
    fn linear_rgb_profile_bytes() -> Vec<u8> {
        use lcms2::{CIExyY, CIExyYTRIPLE, ToneCurve};
        let white = CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.64,
                y: 0.33,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.30,
                y: 0.60,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.15,
                y: 0.06,
                Y: 1.0,
            },
        };
        let linear = ToneCurve::new(1.0);
        let curves = [&linear, &linear, &linear];
        Profile::new_rgb(&white, &primaries, &curves)
            .unwrap()
            .icc()
            .unwrap()
    }

    #[test]
    fn pillow_reference_patches_match_for_every_intent_and_paper_mode() {
        // testdata/proof_patches.json is produced by tests/proof_patches.py,
        // which drives Pillow's ImageCms with the same profile, colours and
        // intents. Agreement within one level is the measured bound.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let profile = std::fs::read(dir.join("proof_bluish_paper.icc"))
            .expect("the synthetic paper profile fixture");
        let fixture: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.join("proof_patches.json")).expect("the reference patches"),
        )
        .expect("valid patch fixture");
        let patches = fixture["patches"].as_array().expect("patches");
        assert!(!patches.is_empty());
        for patch in patches {
            let intent = match patch["intent"].as_str().expect("intent") {
                "perceptual" => Intent::Perceptual,
                "relative" => Intent::RelativeColorimetric,
                "saturation" => Intent::Saturation,
                "absolute" => Intent::AbsoluteColorimetric,
                other => panic!("unknown intent {other}"),
            };
            let paper = patch["paper"].as_bool().expect("paper flag");
            let proof = Proof::new_with_options(None, &profile, intent, paper)
                .expect("build the reference proof");
            let colours = patch["colours"].as_array().expect("colours");
            let values = patch["values"].as_array().expect("values");
            assert_eq!(colours.len(), values.len());
            for (colour, expected) in colours.iter().zip(values) {
                let rgb = [
                    colour[0].as_u64().unwrap() as u8,
                    colour[1].as_u64().unwrap() as u8,
                    colour[2].as_u64().unwrap() as u8,
                ];
                let want = [
                    expected[0].as_u64().unwrap() as u8,
                    expected[1].as_u64().unwrap() as u8,
                    expected[2].as_u64().unwrap() as u8,
                ];
                let got = proof.proof_pixel(rgb);
                for channel in 0..3 {
                    let difference =
                        (i32::from(got[channel]) - i32::from(want[channel])).abs();
                    assert!(
                        difference <= 1,
                        "{:?} with {:?} paper={} -> {got:?}, Pillow says {want:?}",
                        rgb,
                        intent.name(),
                        paper
                    );
                }
            }
        }
    }

    #[test]
    fn paper_simulation_shows_the_simulated_sheet() {
        let profile = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata/proof_bluish_paper.icc"),
        )
        .unwrap();
        let plain = Proof::new_with_options(
            None,
            &profile,
            Intent::RelativeColorimetric,
            false,
        )
        .unwrap();
        let paper =
            Proof::new_with_options(None, &profile, Intent::RelativeColorimetric, true).unwrap();
        assert!(!plain.simulate_paper && paper.simulate_paper);
        let white = [255u8, 255, 255];
        assert_eq!(
            plain.proof_pixel(white),
            [255, 255, 255],
            "without paper simulation the display white stays white"
        );
        let simulated = paper.proof_pixel(white);
        assert!(
            simulated[0] < 250 && simulated[2] > 250,
            "a D65-white sheet must look cooler than the display white: {simulated:?}"
        );
    }

    #[test]
    fn a_document_profile_is_used_as_the_proof_source() {
        // The document profile is the transform input: a linear document
        // profile brightens mid-tones before the output profile is simulated.
        let linear = linear_rgb_profile_bytes();
        let srgb = srgb_profile_bytes();
        let with_srgb_source =
            Proof::new_with_source(None, &srgb, Intent::RelativeColorimetric).unwrap();
        let with_linear_source =
            Proof::new_with_source(Some(&linear), &srgb, Intent::RelativeColorimetric).unwrap();
        let colour = [128u8, 128, 128];
        let plain = with_srgb_source.proof_pixel(colour);
        let from_linear = with_linear_source.proof_pixel(colour);
        assert!(
            (i32::from(plain[0]) - 128).abs() <= 2,
            "an sRGB source keeps the mid-grey: {plain:?}"
        );
        assert!(
            from_linear[0] > plain[0] + 20,
            "a linear document profile must brighten the proof: {plain:?} vs {from_linear:?}"
        );
    }

    #[test]
    fn a_non_rgb_document_profile_is_rejected() {
        use lcms2::{CIExyY, ToneCurve};
        let white = CIExyY {
            x: 0.3457,
            y: 0.3585,
            Y: 1.0,
        };
        let grey = Profile::new_gray(&white, &ToneCurve::new(2.2)).unwrap();
        let error = Proof::new_with_source(
            Some(&grey.icc().unwrap()),
            &srgb_profile_bytes(),
            Intent::Perceptual,
        )
        .err()
        .expect("a grey document profile cannot feed RGB samples");
        assert!(
            error.to_string().contains("colour space"),
            "{error}"
        );
    }

    #[test]
    fn an_invalid_document_profile_is_rejected() {
        let error = Proof::new_with_source(
            Some(b"not an ICC profile"),
            &srgb_profile_bytes(),
            Intent::Perceptual,
        )
        .err()
        .expect("an invalid document profile must fail");
        assert!(
            error.to_string().contains("invalid document profile"),
            "{error}"
        );
    }

    #[test]
    fn a_grey_output_profile_desaturates_the_preview() {
        // Proving a colour document through a grey profile collapses chroma:
        // that is a real profile transform, not a fixed-coefficient stand-in.
        use lcms2::{CIExyY, ToneCurve};
        let white = CIExyY {
            x: 0.3457,
            y: 0.3585,
            Y: 1.0,
        };
        let grey = Profile::new_gray(&white, &ToneCurve::new(2.2)).unwrap();
        let proof = Proof::new(&grey.icc().unwrap(), Intent::RelativeColorimetric).unwrap();
        let source = [30u8, 120, 220];
        let proofed = proof.proof_pixel(source);
        let chroma = |pixel: [u8; 3]| {
            let max = *pixel.iter().max().unwrap() as i32;
            let min = *pixel.iter().min().unwrap() as i32;
            max - min
        };
        assert!(
            chroma(proofed) < chroma(source) / 2,
            "a grey output profile must collapse chroma: {source:?} -> {proofed:?}"
        );
    }
}
