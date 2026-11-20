//! Adjustment-layer filters.
//!
//! CSP stores the settings of a correction layer in the optional
//! `Layer.FilterLayerInfo` BLOB:
//!
//! ```text
//! u32 filter_type
//! u32 payload_size
//! u8  payload[payload_size]
//! ```
//!
//! The filter kinds recovered from the real documents, the RTTI in
//! `evidence/install/strings.txt` (`USAdjustmentLevels`, `USAdjustmentHSV`,
//! `USAdjustmentColorBalance`, ...) and the payload sizes are listed in
//! [`FilterKind`].

use crate::render::Image;

pub const TYPE_LEVELS: u32 = 2;
pub const TYPE_HSV: u32 = 4;
pub const TYPE_COLOR_BALANCE: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub in_black: f64,
    pub in_white: f64,
    pub out_black: f64,
    pub out_white: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterKind {
    /// Value correction. The 320-byte body starts with five big-endian
    /// 16-bit fixed-point values: input black, a middle marker, input white,
    /// output black and output white, all in 0..65535. The middle marker is
    /// not the gamma in the measured sample (a linear curve matches CSP to one
    /// level), so the port applies the linear part and leaves gamma at 1.
    Levels(Levels),
    /// Hue/Saturation/Luminosity, stored as three `i32`.
    Hsv {
        hue: f64,
        saturation: f64,
        luminosity: f64,
    },
    /// Color balance: `preserve_luminosity` plus shadows/midtones/highlights
    /// RGB shifts in -100..=100.
    ColorBalance(ColorBalance),
    /// A filter this port knows by number but cannot apply yet.
    Unknown(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColorBalance {
    pub preserve_luminosity: bool,
    pub shadows: [f64; 3],
    pub midtones: [f64; 3],
    pub highlights: [f64; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub kind: FilterKind,
    pub raw: Vec<u8>,
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_i32(bytes: &[u8], at: usize) -> Option<i32> {
    read_u32(bytes, at).map(|v| v as i32)
}

impl Filter {
    /// Parse a `FilterLayerInfo` blob. Returns `None` when the blob is too
    /// short to hold the header.
    pub fn parse(blob: &[u8]) -> Option<Filter> {
        let filter_type = read_u32(blob, 0)?;
        let size = read_u32(blob, 4)? as usize;
        let end = (8 + size).min(blob.len());
        let payload = blob.get(8..end).unwrap_or(&[]);
        let kind = match filter_type {
            TYPE_LEVELS => {
                if payload.len() >= 10 {
                    let at = |i: usize| {
                        u16::from_be_bytes([payload[i * 2], payload[i * 2 + 1]]) as f64 / 65535.0
                    };
                    FilterKind::Levels(Levels {
                        in_black: at(0),
                        in_white: at(2),
                        out_black: at(3),
                        out_white: at(4),
                    })
                } else {
                    FilterKind::Unknown(filter_type)
                }
            }
            TYPE_HSV => {
                if payload.len() >= 12 {
                    FilterKind::Hsv {
                        hue: read_i32(payload, 0)? as f64,
                        saturation: read_i32(payload, 4)? as f64,
                        luminosity: read_i32(payload, 8)? as f64,
                    }
                } else {
                    FilterKind::Unknown(filter_type)
                }
            }
            TYPE_COLOR_BALANCE => {
                if payload.len() >= 40 {
                    let mut values = [0f64; 9];
                    for (i, slot) in values.iter_mut().enumerate() {
                        *slot = read_i32(payload, 4 + i * 4)? as f64;
                    }
                    FilterKind::ColorBalance(ColorBalance {
                        preserve_luminosity: read_u32(payload, 0)? != 0,
                        shadows: [values[0], values[1], values[2]],
                        midtones: [values[3], values[4], values[5]],
                        highlights: [values[6], values[7], values[8]],
                    })
                } else {
                    FilterKind::Unknown(filter_type)
                }
            }
            other => FilterKind::Unknown(other),
        };
        Some(Filter {
            kind,
            raw: payload.to_vec(),
        })
    }
}

/// Serialize the three HSV values back into a payload.
pub fn hsv_payload(hue: i32, saturation: i32, luminosity: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    for v in [hue, saturation, luminosity] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

/// Serialize a Levels payload. CSP reserves a 320-byte body; the five
/// meaningful 16-bit big-endian values live at the start and the remainder is
/// zero padding.
pub fn levels_payload(in_black: f64, in_white: f64, out_black: f64, out_white: f64) -> Vec<u8> {
    let mut out = vec![0u8; 320];
    for (index, value) in [in_black, in_black, in_white, out_black, out_white]
        .into_iter()
        .enumerate()
    {
        let fixed = (value.clamp(0.0, 1.0) * 65535.0).round() as u16;
        out[index * 2..index * 2 + 2].copy_from_slice(&fixed.to_be_bytes());
    }
    out
}

/// Serialize a color-balance payload (`preserve_luminosity` + 9 values).
pub fn color_balance_payload(preserve_luminosity: bool, values: [i32; 9]) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&(preserve_luminosity as u32).to_be_bytes());
    for v in values {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out
}

impl Filter {
    /// Re-encode this filter as a complete `FilterLayerInfo` BLOB.
    pub fn to_blob(&self) -> Option<Vec<u8>> {
        let (filter_type, payload) = match &self.kind {
            FilterKind::Levels(levels) => (
                TYPE_LEVELS,
                levels_payload(
                    levels.in_black,
                    levels.in_white,
                    levels.out_black,
                    levels.out_white,
                ),
            ),
            FilterKind::Hsv {
                hue,
                saturation,
                luminosity,
            } => (
                TYPE_HSV,
                hsv_payload(*hue as i32, *saturation as i32, *luminosity as i32),
            ),
            FilterKind::ColorBalance(balance) => {
                let mut values = [0i32; 9];
                let mut at = 0;
                for range in [balance.shadows, balance.midtones, balance.highlights] {
                    for value in range {
                        values[at] = value.round() as i32;
                        at += 1;
                    }
                }
                (
                    TYPE_COLOR_BALANCE,
                    color_balance_payload(balance.preserve_luminosity, values),
                )
            }
            FilterKind::Unknown(kind) => {
                // A filter this build cannot model keeps its original payload:
                // re-synthesizing an empty body would erase settings only CSP
                // understands.
                if !self.raw.is_empty() {
                    let mut out = Vec::with_capacity(8 + self.raw.len());
                    out.extend_from_slice(&kind.to_be_bytes());
                    out.extend_from_slice(&(self.raw.len() as u32).to_be_bytes());
                    out.extend_from_slice(&self.raw);
                    return Some(out);
                }
                return Some(
                    [kind.to_be_bytes().as_slice(), 0u32.to_be_bytes().as_slice()].concat(),
                );
            }
        };
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&filter_type.to_be_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
        Some(out)
    }
}

fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d) % 6.0
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    ((h / 6.0).rem_euclid(1.0), s, l)
}

fn hue_channel(p: f64, q: f64, t: f64) -> f64 {
    let t = t.rem_euclid(1.0);
    if t < 1.0 / 6.0 {
        p + (q - p) * 6.0 * t
    } else if t < 0.5 {
        q
    } else if t < 2.0 / 3.0 {
        p + (q - p) * (2.0 / 3.0 - t) * 6.0
    } else {
        p
    }
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    if s <= 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    (
        hue_channel(p, q, h + 1.0 / 3.0),
        hue_channel(p, q, h),
        hue_channel(p, q, h - 1.0 / 3.0),
    )
}

fn apply_hsv(pixel: [f64; 3], hue: f64, saturation: f64, luminosity: f64) -> [f64; 3] {
    let (mut h, mut s, mut l) = rgb_to_hsl(pixel[0], pixel[1], pixel[2]);
    h = (h + hue / 360.0).rem_euclid(1.0);
    // Photoshop's saturation slider scales toward/away from grey.
    s = if saturation >= 0.0 {
        s + (1.0 - s) * saturation / 100.0
    } else {
        s * (1.0 + saturation / 100.0)
    };
    l = if luminosity >= 0.0 {
        l + (1.0 - l) * luminosity / 100.0
    } else {
        l * (1.0 + luminosity / 100.0)
    };
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    [r, g, b]
}

/// Gamma coefficients measured against CSP 5.1.5.
///
/// CSP's color balance is a **per-channel transfer curve**: the red output
/// depends only on the red input, the green output only on green, and so on.
/// For each tonal range the curve is a power law, `out = in^gamma`, and the
/// ranges compose by multiplying their gammas.
///
/// The coefficients below were fitted to a 256-level grey ramp rendered by
/// CSP. `a` applies to the slider of the channel being computed and `b` to the
/// sum of the other two sliders; `preserve_luminosity` is what makes `b`
/// non-zero. The measured LUTs are recorded in `docs/COLOR.md`.
struct BalanceCoefficients {
    /// Multiplier for the channel's own slider.
    a_pos: f64,
    a_neg: f64,
    /// Multiplier for the other two sliders.
    b: f64,
}

const MIDTONES_PRESERVE: BalanceCoefficients = BalanceCoefficients {
    a_pos: -0.00839,
    a_neg: -0.00988,
    b: 0.00484,
};
const MIDTONES_RAW: BalanceCoefficients = BalanceCoefficients {
    a_pos: -0.01106,
    a_neg: -0.01106,
    b: 0.0,
};

fn range_gamma(amount: [f64; 3], channel: usize, coef: &BalanceCoefficients) -> f64 {
    let own = amount[channel];
    let other: f64 = (0..3).filter(|c| *c != channel).map(|c| amount[c]).sum();
    let a = if own >= 0.0 { coef.a_pos } else { coef.a_neg };
    (a * own + coef.b * other).exp().clamp(0.2, 5.0)
}

fn apply_levels(pixel: [f64; 3], levels: &Levels) -> [f64; 3] {
    let span = (levels.in_white - levels.in_black).abs().max(1e-6);
    let mut out = pixel;
    for c in 0..3 {
        let x = ((pixel[c] - levels.in_black) / span).clamp(0.0, 1.0);
        out[c] = (levels.out_black + (levels.out_white - levels.out_black) * x).clamp(0.0, 1.0);
    }
    out
}

fn apply_color_balance(pixel: [f64; 3], cb: &ColorBalance) -> [f64; 3] {
    let coef = if cb.preserve_luminosity {
        MIDTONES_PRESERVE
    } else {
        MIDTONES_RAW
    };
    let mut out = pixel;
    for c in 0..3 {
        // Shadows. CSP stores the shadow range as a levels curve: a negative
        // slider lifts the black point (2.4 levels per slider unit, measured
        // at -25 -> black 60) and a positive slider is a gamma lift when
        // "preserve luminosity" is off. With preserve on, a positive shadow
        // slider leaves its own channel at identity; the cross-channel crush
        // CSP adds there is not modelled (it costs up to 10 levels on the
        // real tsuaii triple).
        let shadow = cb.shadows[c];
        if shadow < 0.0 {
            let black = (2.4 * -shadow / 255.0).min(1.0 - 1e-9);
            out[c] = ((out[c] - black) / (1.0 - black)).clamp(0.0, 1.0);
        } else if shadow > 0.0 && !cb.preserve_luminosity {
            let g = (-0.0043 * shadow).exp();
            out[c] = out[c].clamp(0.0, 1.0).powf(g);
        }

        // Midtones: power law, validated for all 256 levels.
        let g = range_gamma(cb.midtones, c, &coef);
        out[c] = out[c].clamp(0.0, 1.0).powf(g);

        // Highlights. A positive slider lowers the white point; the measured
        // response is a 2/3-power saturation, `white = 1 - 160/255 * t`,
        // with `t = (slider/100)^(2/3)`. Preserve off also applies a gamma
        // lift. Negative highlight sliders measured as identity on the real
        // document (the blue -7 channel is exactly identity); the green -1
        // sample differs by 13 levels, recorded in docs/COLOR.md.
        let highlight = cb.highlights[c];
        if highlight > 0.0 {
            let t = (highlight / 100.0).powf(2.0 / 3.0);
            if cb.preserve_luminosity {
                let white = 1.0 - (160.0 / 255.0) * t;
                out[c] = (out[c] / white).clamp(0.0, 1.0);
            } else {
                let white = 1.0 - (170.0 / 255.0) * t;
                let g = 1.0 - 0.5 * t;
                out[c] = (out[c] / white).clamp(0.0, 1.0).powf(g);
            }
        }
    }
    out
}

impl Filter {
    /// Apply the filter to one pixel. `strength` scales the whole effect
    /// (adjustment-layer opacity times the mask alpha).
    pub fn apply_pixel(&self, pixel: [f64; 3], strength: f64) -> [f64; 3] {
        let s = strength.clamp(0.0, 1.0);
        if s <= 0.0 {
            return pixel;
        }
        let out = match &self.kind {
            FilterKind::Hsv {
                hue,
                saturation,
                luminosity,
            } => apply_hsv(pixel, *hue, *saturation, *luminosity),
            FilterKind::Levels(levels) => apply_levels(pixel, levels),
            FilterKind::ColorBalance(cb) => apply_color_balance(pixel, cb),
            // Unknown filters are deliberately left alone rather than guessed
            // at.
            FilterKind::Unknown(_) => return pixel,
        };
        let mut mixed = pixel;
        for c in 0..3 {
            mixed[c] = pixel[c] + (out[c] - pixel[c]) * s;
        }
        mixed
    }
}

/// Apply a filter to the whole composite. `mask` is an alpha-only surface
/// aligned with the canvas (0 hides, 255 applies fully).
pub fn apply_to_image(
    image: &mut Image,
    filter: &Filter,
    opacity: f64,
    mask: Option<&Image>,
    mask_offset: (i32, i32),
    clip: Option<&[u8]>,
) {
    if opacity <= 0.0 {
        return;
    }
    let w = image.width as usize;
    for y in 0..image.height as usize {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let mut strength = opacity;
            if let Some(c) = clip {
                strength *= c[i + 3] as f64 / 255.0;
            }
            if let Some(m) = mask {
                let mx = x as i32 - mask_offset.0;
                let my = y as i32 - mask_offset.1;
                let alpha = if mx >= 0 && my >= 0 && mx < m.width as i32 && my < m.height as i32 {
                    m.data[((my as usize * m.width as usize + mx as usize) * 4 + 3)] as f64 / 255.0
                } else {
                    0.0
                };
                strength *= alpha;
            }
            if strength <= 0.0 {
                continue;
            }
            let px = [
                image.data[i] as f64 / 255.0,
                image.data[i + 1] as f64 / 255.0,
                image.data[i + 2] as f64 / 255.0,
            ];
            let out = filter.apply_pixel(px, strength);
            for c in 0..3 {
                image.data[i + c] = (out[c] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_color_balance_of_a_real_document() {
        // tsuaii.clip, layer 37 "Color Balance".
        let blob = [
            0, 0, 0, 5, 0, 0, 0, 40, 0, 0, 0, 1, 0xff, 0xff, 0xff, 0xe7, 0, 0, 0, 2, 0, 0, 0, 7, 0,
            0, 0, 10, 0, 0, 0, 2, 0, 0, 0, 9, 0, 0, 0, 9, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xf9,
        ];
        let f = Filter::parse(&blob).unwrap();
        assert_eq!(
            f.kind,
            FilterKind::ColorBalance(ColorBalance {
                preserve_luminosity: true,
                shadows: [-25.0, 2.0, 7.0],
                midtones: [10.0, 2.0, 9.0],
                highlights: [9.0, -1.0, -7.0],
            })
        );
    }

    #[test]
    fn parses_the_hsv_of_a_real_document() {
        // lolita-en_01.clip, layer 466.
        let blob = [
            0, 0, 0, 4, 0, 0, 0, 12, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xd8, 0, 0, 0, 33,
        ];
        let f = Filter::parse(&blob).unwrap();
        assert_eq!(
            f.kind,
            FilterKind::Hsv {
                hue: 0.0,
                saturation: -40.0,
                luminosity: 33.0
            }
        );
    }

    fn color_balance_payload(preserve: bool, midtones: [i32; 3]) -> Vec<u8> {
        let mut blob = vec![0, 0, 0, 5, 0, 0, 0, 40];
        blob.extend_from_slice(&(preserve as u32).to_be_bytes());
        for v in [0i32, 0, 0] {
            blob.extend_from_slice(&v.to_be_bytes());
        }
        for v in midtones {
            blob.extend_from_slice(&v.to_be_bytes());
        }
        for v in [0i32, 0, 0] {
            blob.extend_from_slice(&v.to_be_bytes());
        }
        blob
    }

    #[test]
    fn color_balance_midtones_match_csp_with_preserved_luminosity() {
        // CSP 5.1.5, midtones +100 red, "Manter brilho" on, grey 51 renders
        // (126, 20, 20). The transfer is a per-channel power law: the red
        // gamma is 0.432 and the green/blue gamma is 1.623.
        let f = Filter::parse(&color_balance_payload(true, [100, 0, 0])).unwrap();
        let out = f.apply_pixel([51.0 / 255.0; 3], 1.0);
        assert!((out[0] * 255.0 - 126.0).abs() < 2.0, "red: {out:?}");
        assert!((out[1] * 255.0 - 20.0).abs() < 2.0, "green: {out:?}");
        assert!((out[2] * 255.0 - 20.0).abs() < 2.0, "blue: {out:?}");
    }

    #[test]
    fn color_balance_midtone_triple_matches_tsuaii() {
        // tsuaii.clip: midtones (10, 2, 9), preserve on. The CSP ramp renders
        // 51 -> (53, 45, 53), 102 -> (106, 96, 104), 204 -> (206, 202, 206).
        let f = Filter::parse(&color_balance_payload(true, [10, 2, 9])).unwrap();
        for (v, expected) in [
            (51.0, [53.0, 45.0, 53.0]),
            (102.0, [106.0, 96.0, 104.0]),
            (204.0, [206.0, 202.0, 206.0]),
        ] {
            let out = f.apply_pixel([v / 255.0; 3], 1.0);
            for c in 0..3 {
                assert!(
                    (out[c] * 255.0 - expected[c]).abs() < 2.0,
                    "input {v}: got {out:?}, want {expected:?}"
                );
            }
        }
    }

    #[test]
    fn color_balance_midtones_without_preserve_only_change_one_channel() {
        // Same slider with "Manter brilho" off: (51, 51, 51) -> (150, 51, 51).
        let f = Filter::parse(&color_balance_payload(false, [100, 0, 0])).unwrap();
        let out = f.apply_pixel([51.0 / 255.0; 3], 1.0);
        assert!((out[0] * 255.0 - 150.0).abs() < 2.0, "red: {out:?}");
        assert!((out[1] * 255.0 - 51.0).abs() < 1.0, "green: {out:?}");
        assert!((out[2] * 255.0 - 51.0).abs() < 1.0, "blue: {out:?}");
    }

    fn levels_payload(values: [u16; 5]) -> Vec<u8> {
        let mut blob = vec![0, 0, 0, 2, 0, 0, 1, 64];
        let mut body = vec![0u8; 320];
        for (i, v) in values.iter().enumerate() {
            body[i * 2..i * 2 + 2].copy_from_slice(&v.to_be_bytes());
        }
        blob.extend_from_slice(&body);
        blob
    }

    #[test]
    fn levels_match_the_csp_ramp_measurement() {
        // Measured on levels_ramp.clip: input black 15801/65535, input white
        // 65535, output 0..65535. CSP's preview maps 96 -> 45, 144 -> 109 and
        // 191 -> 171.
        let f = Filter::parse(&levels_payload([15801, 40667, 65535, 0, 65535])).unwrap();
        for (input, expected) in [(96.0, 45.0), (144.0, 109.0), (191.0, 171.0)] {
            let out = f.apply_pixel([input / 255.0; 3], 1.0);
            assert!(
                (out[0] * 255.0 - expected).abs() < 2.0,
                "input {input}: got {}, want {expected}",
                out[0] * 255.0
            );
        }
    }

    #[test]
    fn levels_decode_the_jose_black_lift() {
        // jose-en_01.clip, "Level Correction 1": input black 33.26,
        // input white/output 0..255.
        let f = Filter::parse(&levels_payload([8548, 41186, 65535, 0, 65535])).unwrap();
        let out = f.apply_pixel([128.0 / 255.0; 3], 1.0);
        assert!(
            (out[0] * 255.0 - 109.0).abs() < 2.0,
            "got {}",
            out[0] * 255.0
        );
    }

    #[test]
    fn levels_apply_the_output_range() {
        let f = Filter::parse(&levels_payload([0, 32767, 65535, 8030, 41704])).unwrap();
        assert!((f.apply_pixel([0.0; 3], 1.0)[0] * 255.0 - 31.0).abs() < 2.0);
        assert!((f.apply_pixel([1.0; 3], 1.0)[0] * 255.0 - 162.0).abs() < 2.0);
        assert!((f.apply_pixel([0.5; 3], 1.0)[0] * 255.0 - 97.0).abs() < 2.0);
    }

    fn ground_truth() -> serde_json::Value {
        serde_json::from_str(include_str!("../testdata/tsuaii_ranges.json")).unwrap()
    }

    fn assert_lut_matches(cb: ColorBalance, key: &str, tolerance: i32) {
        let json = ground_truth();
        let lut = json[key]["lut"].as_array().unwrap();
        let filter = Filter {
            kind: FilterKind::ColorBalance(cb),
            raw: Vec::new(),
        };
        let mut worst = 0;
        for v in 0..256 {
            let out = filter.apply_pixel([v as f64 / 255.0; 3], 1.0);
            for c in 0..3 {
                let got = (out[c] * 255.0).round() as i32;
                let want = lut[v][c].as_i64().unwrap() as i32;
                worst = worst.max((got - want).abs());
            }
        }
        assert!(worst <= tolerance, "{key}: max error {worst} > {tolerance}");
    }

    #[test]
    fn unknown_filter_blob_is_rebuilt_byte_for_byte() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&999u32.to_be_bytes());
        blob.extend_from_slice(&8u32.to_be_bytes());
        blob.extend_from_slice(b"abcdefgh");
        let filter = Filter::parse(&blob).expect("parse");
        assert!(matches!(filter.kind, FilterKind::Unknown(999)));
        assert_eq!(
            filter.to_blob().expect("re-encode"),
            blob,
            "an unmodelled filter must keep its payload"
        );

        // A known type with a payload too short to decode is also preserved.
        let mut short = Vec::new();
        short.extend_from_slice(&TYPE_HSV.to_be_bytes());
        short.extend_from_slice(&4u32.to_be_bytes());
        short.extend_from_slice(b"wxyz");
        let filter = Filter::parse(&short).expect("parse");
        assert_eq!(filter.to_blob().expect("re-encode"), short);
    }

    #[test]
    fn color_balance_shadow_range_matches_csp() {
        // tsuaii.clip shadows (-25, 2, 7), preserve on, measured in isolation.
        // The red channel is a levels black lift; the small positive green
        // and blue sliders contribute a cross-channel crush that is not
        // modelled, hence the 11-level tolerance.
        assert_lut_matches(
            ColorBalance {
                preserve_luminosity: true,
                shadows: [-25.0, 2.0, 7.0],
                midtones: [0.0; 3],
                highlights: [0.0; 3],
            },
            "rtsu_sh",
            11,
        );
    }

    #[test]
    fn color_balance_midtone_range_matches_csp() {
        assert_lut_matches(
            ColorBalance {
                preserve_luminosity: true,
                shadows: [0.0; 3],
                midtones: [10.0, 2.0, 9.0],
                highlights: [0.0; 3],
            },
            "rtsu_mid",
            4,
        );
    }

    #[test]
    fn color_balance_highlight_range_matches_csp() {
        // The green -1 sample is 13 levels off (see docs/COLOR.md); positive
        // highlights are a 2/3-power white-point compression.
        assert_lut_matches(
            ColorBalance {
                preserve_luminosity: true,
                shadows: [0.0; 3],
                midtones: [0.0; 3],
                highlights: [9.0, -1.0, -7.0],
            },
            "rtsu_hi",
            14,
        );
    }

    #[test]
    fn color_balance_all_ranges_compose_like_csp() {
        // Shadows, then midtones, then highlights is the order that matches
        // the combined tsuaii ramp within 14 levels.
        assert_lut_matches(
            ColorBalance {
                preserve_luminosity: true,
                shadows: [-25.0, 2.0, 7.0],
                midtones: [10.0, 2.0, 9.0],
                highlights: [9.0, -1.0, -7.0],
            },
            "rtsu_all",
            16,
        );
    }

    #[test]
    fn color_balance_preserve_off_uses_gamma_lifts() {
        // CSP: shadows +100 red, preserve off -> (128, 128, 128) becomes
        // (164, 128, 128); highlights +100 red, preserve off -> 64 becomes
        // 223 on red.
        let shadows = Filter::parse(&color_balance_payload(false, [0, 0, 0])).unwrap();
        let mut cb = match shadows.kind {
            FilterKind::ColorBalance(cb) => cb,
            _ => unreachable!(),
        };
        cb.shadows = [100.0, 0.0, 0.0];
        let f = Filter {
            kind: FilterKind::ColorBalance(cb),
            raw: Vec::new(),
        };
        let out = f.apply_pixel([128.0 / 255.0; 3], 1.0);
        assert!((out[0] * 255.0 - 164.0).abs() < 3.0, "shadow red: {out:?}");
        assert!(
            (out[1] * 255.0 - 128.0).abs() < 2.0,
            "shadow green: {out:?}"
        );

        cb.shadows = [0.0; 3];
        cb.highlights = [100.0, 0.0, 0.0];
        let f = Filter {
            kind: FilterKind::ColorBalance(cb),
            raw: Vec::new(),
        };
        let out = f.apply_pixel([64.0 / 255.0; 3], 1.0);
        assert!(
            (out[0] * 255.0 - 223.0).abs() < 3.0,
            "highlight red: {out:?}"
        );
        assert!(
            (out[1] * 255.0 - 64.0).abs() < 2.0,
            "highlight green: {out:?}"
        );
    }

    #[test]
    fn hsv_desaturates_and_brightens() {
        let f = Filter::parse(&[
            0, 0, 0, 4, 0, 0, 0, 12, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xd8, 0, 0, 0, 33,
        ])
        .unwrap();
        let out = f.apply_pixel([0.8, 0.2, 0.2], 1.0);
        let (_, s_before, l_before) = rgb_to_hsl(0.8, 0.2, 0.2);
        let (_, s_after, l_after) = rgb_to_hsl(out[0], out[1], out[2]);
        assert!(
            s_after < s_before,
            "saturation must drop: {s_before} -> {s_after}"
        );
        assert!(
            l_after > l_before,
            "luminosity +33 must brighten: {l_before} -> {l_after}"
        );
    }

    #[test]
    fn mask_and_opacity_blend_the_effect() {
        let f = Filter::parse(&color_balance_payload(false, [-100, 0, 0])).unwrap();
        let mut img = Image::filled(2, 1, [64, 64, 64, 255]);
        let mut mask = Image::filled(2, 1, [255, 255, 255, 0]);
        mask.data[3] = 255; // first pixel reveals
        mask.data[7] = 0; // second hides
        apply_to_image(&mut img, &f, 1.0, Some(&mask), (0, 0), None);
        assert!(img.data[0] < 64, "masked-in pixel is filtered");
        assert_eq!(img.data[4], 64, "masked-out pixel is untouched");
    }
    #[test]
    fn known_filters_round_trip_through_filter_layer_info_blobs() {
        for kind in [
            FilterKind::Levels(Levels {
                in_black: 0.1,
                in_white: 0.9,
                out_black: 0.0,
                out_white: 1.0,
            }),
            FilterKind::Hsv {
                hue: 12.0,
                saturation: -20.0,
                luminosity: 8.0,
            },
            FilterKind::ColorBalance(ColorBalance {
                preserve_luminosity: true,
                shadows: [1.0, -2.0, 3.0],
                midtones: [4.0, 5.0, -6.0],
                highlights: [-7.0, 8.0, 9.0],
            }),
        ] {
            let filter = Filter {
                kind,
                raw: Vec::new(),
            };
            let blob = filter.to_blob().unwrap();
            let parsed = Filter::parse(&blob).unwrap();
            match (&filter.kind, &parsed.kind) {
                (FilterKind::Levels(a), FilterKind::Levels(b)) => {
                    assert!((a.in_black - b.in_black).abs() < 2e-4);
                    assert!((a.in_white - b.in_white).abs() < 2e-4);
                    assert!((a.out_black - b.out_black).abs() < 2e-4);
                    assert!((a.out_white - b.out_white).abs() < 2e-4);
                }
                _ => assert_eq!(parsed.kind, filter.kind),
            }
        }
    }
}
