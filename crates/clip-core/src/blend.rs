//! Blend modes measured from CLIP STUDIO PAINT 5.x previews.

pub const MODE_NORMAL: i64 = 0;
pub const MODE_DARKEN: i64 = 1;
pub const MODE_MULTIPLY: i64 = 2;
pub const MODE_COLOR_BURN: i64 = 3;
pub const MODE_LINEAR_BURN: i64 = 4;
pub const MODE_SUBTRACT: i64 = 5;
pub const MODE_DARKER_COLOR: i64 = 6;
pub const MODE_LIGHTEN: i64 = 7;
pub const MODE_SCREEN: i64 = 8;
pub const MODE_COLOR_DODGE: i64 = 9;
pub const MODE_COLOR_DODGE_GLOW: i64 = 10;
pub const MODE_ADD: i64 = 11;
pub const MODE_ADD_GLOW: i64 = 12;
pub const MODE_LIGHTER_COLOR: i64 = 13;
pub const MODE_OVERLAY: i64 = 14;
pub const MODE_SOFT_LIGHT: i64 = 15;
pub const MODE_HARD_LIGHT: i64 = 16;
pub const MODE_VIVID_LIGHT: i64 = 17;
pub const MODE_LINEAR_LIGHT: i64 = 18;
pub const MODE_PIN_LIGHT: i64 = 19;
pub const MODE_HARD_MIX: i64 = 20;
pub const MODE_DIFFERENCE: i64 = 21;
pub const MODE_EXCLUSION: i64 = 22;
pub const MODE_HUE: i64 = 23;
pub const MODE_SATURATION: i64 = 24;
pub const MODE_COLOR: i64 = 25;
pub const MODE_LUMINOSITY: i64 = 26;
pub const MODE_PASS_THROUGH: i64 = 30;
pub const MODE_DIVIDE: i64 = 36;

pub fn luma(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip(c: f32) -> f32 {
    c.clamp(0.0, 1.0)
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - luma(c);
    [clip(c[0] + d), clip(c[1] + d), clip(c[2] + d)]
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (mut mn, mut md, mut mx) = (0usize, 1usize, 2usize);
    if c[mn] > c[md] {
        std::mem::swap(&mut mn, &mut md);
    }
    if c[md] > c[mx] {
        std::mem::swap(&mut md, &mut mx);
    }
    if c[mn] > c[md] {
        std::mem::swap(&mut mn, &mut md);
    }
    let mut out = [0.0f32; 3];
    if c[mx] > c[mn] {
        out[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        out[mx] = s;
    }
    out[mn] = 0.0;
    out
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        1.0 - (1.0 - b) * 2.0 * (1.0 - s)
    }
}

fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b - (1.0 - 2.0 * s) * b * (1.0 - b)
    } else {
        let d = if b <= 0.25 {
            ((16.0 * b - 12.0) * b + 4.0) * b
        } else {
            b.sqrt()
        };
        b + (2.0 * s - 1.0) * (d - b)
    }
}

/// W3C compositing blend function for opaque colours.
pub fn blend(mode: i64, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    match mode {
        MODE_NORMAL | MODE_PASS_THROUGH => return cs,
        MODE_DARKEN => {
            for i in 0..3 {
                out[i] = cb[i].min(cs[i]);
            }
        }
        MODE_MULTIPLY => {
            for i in 0..3 {
                out[i] = cb[i] * cs[i];
            }
        }
        MODE_COLOR_BURN => {
            for i in 0..3 {
                out[i] = if cs[i] >= 1.0 { 1.0 } else { 1.0 - ((1.0 - cb[i]) / cs[i]).min(1.0) };
            }
        }
        MODE_LINEAR_BURN => {
            for i in 0..3 {
                out[i] = clip(cb[i] + cs[i] - 1.0);
            }
        }
        MODE_SUBTRACT => {
            for i in 0..3 {
                out[i] = clip(cb[i] - cs[i]);
            }
        }
        MODE_DARKER_COLOR => {
            out = if luma(cs) < luma(cb) { cs } else { cb };
        }
        MODE_LIGHTEN => {
            for i in 0..3 {
                out[i] = cb[i].max(cs[i]);
            }
        }
        MODE_SCREEN => {
            for i in 0..3 {
                out[i] = cb[i] + cs[i] - cb[i] * cs[i];
            }
        }
        MODE_COLOR_DODGE | MODE_COLOR_DODGE_GLOW => {
            for i in 0..3 {
                out[i] = if cs[i] <= 0.0 { 0.0 } else { (cb[i] / (1.0 - cs[i])).min(1.0) };
            }
        }
        MODE_ADD | MODE_ADD_GLOW => {
            for i in 0..3 {
                out[i] = cb[i] + cs[i];
            }
        }
        MODE_LIGHTER_COLOR => {
            out = if luma(cs) > luma(cb) { cs } else { cb };
        }
        MODE_OVERLAY => {
            for i in 0..3 {
                out[i] = hard_light(cs[i], cb[i]);
            }
        }
        MODE_SOFT_LIGHT => {
            for i in 0..3 {
                out[i] = soft_light(cb[i], cs[i]);
            }
        }
        MODE_HARD_LIGHT => {
            for i in 0..3 {
                out[i] = hard_light(cb[i], cs[i]);
            }
        }
        MODE_VIVID_LIGHT => {
            for i in 0..3 {
                out[i] = if cs[i] <= 0.5 {
                    blend(MODE_COLOR_BURN, cb, [cs[i] * 2.0; 3])[i]
                } else {
                    blend(MODE_COLOR_DODGE, cb, [(cs[i] - 0.5) * 2.0; 3])[i]
                };
            }
        }
        MODE_LINEAR_LIGHT => {
            for i in 0..3 {
                out[i] = clip(cb[i] + 2.0 * cs[i] - 1.0);
            }
        }
        MODE_PIN_LIGHT => {
            for i in 0..3 {
                out[i] = if cs[i] <= 0.5 {
                    cb[i].min(cs[i] * 2.0)
                } else {
                    cb[i].max(cs[i] * 2.0 - 1.0)
                };
            }
        }
        MODE_HARD_MIX => {
            for i in 0..3 {
                out[i] = if cb[i] + cs[i] >= 1.0 { 1.0 } else { 0.0 };
            }
        }
        MODE_DIFFERENCE => {
            for i in 0..3 {
                out[i] = (cb[i] - cs[i]).abs();
            }
        }
        MODE_EXCLUSION => {
            for i in 0..3 {
                out[i] = cb[i] + cs[i] - 2.0 * cb[i] * cs[i];
            }
        }
        MODE_HUE => out = set_lum(set_sat(cs, sat(cb)), luma(cb)),
        MODE_SATURATION => out = set_lum(set_sat(cb, sat(cs)), luma(cb)),
        MODE_COLOR => out = set_lum(cs, luma(cb)),
        MODE_LUMINOSITY => out = set_lum(cb, luma(cs)),
        MODE_DIVIDE => {
            for i in 0..3 {
                out[i] = if cs[i] <= 0.0 { 1.0 } else { (cb[i] / cs[i]).min(1.0) };
            }
        }
        _ => return cs,
    }
    [clip(out[0]), clip(out[1]), clip(out[2])]
}

pub fn is_glow(mode: i64) -> bool {
    mode == MODE_COLOR_DODGE_GLOW || mode == MODE_ADD_GLOW
}

/// Composite one source pixel over a destination pixel (straight alpha, 0..1).
/// Returns the resulting straight RGB and alpha.
pub fn over(mode: i64, cb: [f32; 3], ab: f32, cs: [f32; 3], as_: f32) -> ([f32; 3], f32) {
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 1e-8 {
        return ([0.0; 3], 0.0);
    }
    if is_glow(mode) {
        let b = blend(mode, cb, [cs[0] * as_, cs[1] * as_, cs[2] * as_]);
        let mut out = [0.0f32; 3];
        for i in 0..3 {
            out[i] = ((1.0 - ab) * as_ * cs[i] + ab * b[i]) / ao;
        }
        ([clip(out[0]), clip(out[1]), clip(out[2])], ao)
    } else {
        let b = blend(mode, cb, cs);
        let mut out = [0.0f32; 3];
        for i in 0..3 {
            out[i] = ((1.0 - ab) * as_ * cs[i] + ab * as_ * b[i] + (1.0 - as_) * ab * cb[i]) / ao;
        }
        ([clip(out[0]), clip(out[1]), clip(out[2])], ao)
    }
}

pub fn mode_name(m: i64) -> &'static str {
    match m {
        0 => "Normal",
        1 => "Darken",
        2 => "Multiply",
        3 => "Color burn",
        4 => "Linear burn",
        5 => "Subtract",
        6 => "Darker color",
        7 => "Lighten",
        8 => "Screen",
        9 => "Color dodge",
        10 => "Color dodge (glow)",
        11 => "Add",
        12 => "Add (glow)",
        13 => "Lighter color",
        14 => "Overlay",
        15 => "Soft light",
        16 => "Hard light",
        17 => "Vivid light",
        18 => "Linear light",
        19 => "Pin light",
        20 => "Hard mix",
        21 => "Difference",
        22 => "Exclusion",
        23 => "Hue",
        24 => "Saturation",
        25 => "Color",
        26 => "Luminosity",
        30 => "Pass through",
        36 => "Divide",
        _ => "Unknown",
    }
}
