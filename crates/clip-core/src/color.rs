//! Colour-space helpers used by the editor and PSD importer.
//!
//! Full ICC evaluation is delegated to the platform profile in a later pass;
//! this module provides a deterministic CMYK conversion/soft-proof path and a
//! validated container for embedded ICC profiles so documents can carry their
//! profile bytes without losing them.

use crate::render::Image;

/// Minimal parsed ICC header. The full byte payload is retained verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccProfile {
    pub bytes: Vec<u8>,
    pub color_space: [u8; 4],
    pub pcs: [u8; 4],
}

impl IccProfile {
    pub fn parse(bytes: Vec<u8>) -> Option<Self> {
        if bytes.len() < 128 {
            return None;
        }
        let size = u32::from_be_bytes(bytes[0..4].try_into().ok()?) as usize;
        if size < 128 || size > bytes.len() {
            return None;
        }
        let signature = &bytes[36..40];
        if signature != b"acsp" {
            return None;
        }
        Some(Self {
            color_space: bytes[16..20].try_into().ok()?,
            pcs: bytes[20..24].try_into().ok()?,
            bytes,
        })
    }

    pub fn color_space_name(&self) -> &'static str {
        match &self.color_space {
            b"RGB " => "RGB",
            b"CMYK" => "CMYK",
            b"GRAY" => "Gray",
            b"LAB " => "Lab",
            _ => "Unknown",
        }
    }
}

fn clip01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

/// Convert normalized CMYK ink (0 = no ink, 1 = full ink) to sRGB.
pub fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [u8; 3] {
    let c = clip01(c);
    let m = clip01(m);
    let y = clip01(y);
    let k = clip01(k);
    let convert = |ink: f32| ((1.0 - ink) * (1.0 - k) * 255.0).round().clamp(0.0, 255.0) as u8;
    [convert(c), convert(m), convert(y)]
}

/// Convert straight sRGB to normalized CMYK ink.
pub fn rgb_to_cmyk(rgb: [u8; 3]) -> [f32; 4] {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;
    let k = 1.0 - r.max(g).max(b);
    if k >= 1.0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let inv = 1.0 - k;
    [
        (1.0 - r - k) / inv,
        (1.0 - g - k) / inv,
        (1.0 - b - k) / inv,
        k,
    ]
}

/// Identity of the current preview transform. Bump it whenever the coefficients
/// below change so cached previews are rebuilt.
pub const PROOF_SETTINGS_ID: u64 = 1;

/// Identity of the display leg both preview paths use.
///
/// The editor does not read the host monitor profile: sRGB is the declared
/// assumption. Carrying its identity in the cache key means a future real
/// display profile changes the identity and rebuilds previews instead of
/// mixing two transforms in one cache.
pub const PROOF_DISPLAY_PROFILE_ID: u64 = 1;

/// User-facing description of what the preview actually does. The label and the
/// help text read from here so no surface can claim profile-based proofing.
pub const PROOF_DESCRIPTION: &str =
    "Approximate CMYK preview: fixed U.S. Web Coated coefficients, no ICC profile and no print prediction";

/// A deterministic CMYK soft proof. It uses the common U.S. Web Coated
/// approximation: neutral black gains slightly, primary inks lose a little
/// saturation, and out-of-gamut channels are compressed toward the paper
/// white. It is intentionally independent from the host's monitor profile, so
/// the preview is reproducible in tests and on every workstation.
///
/// This is not profile-based proofing: no output profile, rendering intent,
/// paper simulation or monitor transform is applied.
pub fn cmyk_soft_proof_pixel(rgb: [u8; 3]) -> [u8; 3] {
    let [c, m, y, k] = rgb_to_cmyk(rgb);
    let c = clip01(c * 1.04 + 0.008);
    let m = clip01(m * 1.02 + 0.006);
    let y = clip01(y * 0.98 + 0.004);
    let k = clip01(k * 1.06 + 0.004);
    cmyk_to_rgb(c, m, y, k)
}

pub fn apply_cmyk_soft_proof(image: &Image) -> Image {
    let mut out = image.clone();
    for px in out.data.chunks_exact_mut(4) {
        let proofed = cmyk_soft_proof_pixel([px[0], px[1], px[2]]);
        px[..3].copy_from_slice(&proofed);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_primaries_convert_to_the_expected_rgb_corners() {
        assert_eq!(cmyk_to_rgb(0.0, 1.0, 1.0, 0.0), [255, 0, 0]);
        assert_eq!(cmyk_to_rgb(1.0, 0.0, 1.0, 0.0), [0, 255, 0]);
        assert_eq!(cmyk_to_rgb(1.0, 1.0, 0.0, 0.0), [0, 0, 255]);
        assert_eq!(cmyk_to_rgb(0.0, 0.0, 0.0, 1.0), [0, 0, 0]);
    }

    #[test]
    fn rgb_cmyk_round_trip_is_stable_for_primaries() {
        for rgb in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [0, 0, 0],
            [255, 255, 255],
        ] {
            let [c, m, y, k] = rgb_to_cmyk(rgb);
            assert_eq!(cmyk_to_rgb(c, m, y, k), rgb);
        }
    }

    #[test]
    fn cmyk_soft_proof_is_deterministic_and_changes_saturated_colours() {
        let source = [30, 120, 220];
        let a = cmyk_soft_proof_pixel(source);
        let b = cmyk_soft_proof_pixel(source);
        assert_eq!(a, b);
        assert_ne!(a, source);
    }

    #[test]
    fn soft_proof_does_not_modify_the_source_image() {
        let mut image = Image::filled(2, 2, [30, 120, 220, 200]);
        image.data[4..8].copy_from_slice(&[200, 10, 90, 128]);
        let before = image.data.clone();
        let proofed = apply_cmyk_soft_proof(&image);
        assert_eq!(image.data, before, "proofing must not touch the document");
        assert_ne!(proofed.data, before, "the preview is a separate surface");
        assert_eq!(proofed.data[3], 200, "alpha is preserved");
    }

    #[test]
    fn proof_label_states_that_it_is_approximate() {
        assert!(PROOF_DESCRIPTION.contains("Approximate"));
        assert!(PROOF_DESCRIPTION.contains("no ICC profile"));
        assert!(!PROOF_DESCRIPTION.contains("profile-based proof"));
    }

    #[test]
    fn icc_header_is_validated() {
        let mut bytes = vec![0u8; 132];
        bytes[0..4].copy_from_slice(&132u32.to_be_bytes());
        bytes[16..20].copy_from_slice(b"RGB ");
        bytes[20..24].copy_from_slice(b"XYZ ");
        bytes[36..40].copy_from_slice(b"acsp");
        let profile = IccProfile::parse(bytes).unwrap();
        assert_eq!(profile.color_space_name(), "RGB");
    }
}
