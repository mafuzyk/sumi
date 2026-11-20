//! Software compositor: layer tree, mip selection, masks, clipping and blend modes.

use crate::blend;
use crate::model::{ClipFile, Layer, Offscreen};
use crate::Result;

#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Straight (un-premultiplied) RGBA8.
    pub data: Vec<u8>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Image { width, height, data: vec![0; width as usize * height as usize * 4] }
    }
    pub fn filled(width: u32, height: u32, rgba: [u8; 4]) -> Self {
        let mut im = Self::new(width, height);
        for p in im.data.chunks_exact_mut(4) {
            p.copy_from_slice(&rgba);
        }
        im
    }
    pub fn is_empty(&self) -> bool {
        self.data.iter().all(|&b| b == 0)
    }
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Percent: 100 = native canvas resolution.
    pub scale: f64,
    /// Flatten onto this opaque background. `None` keeps alpha.
    pub background: Option<[u8; 4]>,
    /// Include layers whose visibility bit is clear (for diagnostics).
    pub include_hidden: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { scale: 100.0, background: None, include_hidden: false }
    }
}

pub fn render(file: &ClipFile, opts: &RenderOptions) -> Result<Image> {
    let (pw, ph) = file.canvas.pixel_size();
    let w = ((pw as f64) * opts.scale / 100.0).round().max(1.0) as u32;
    let h = ((ph as f64) * opts.scale / 100.0).round().max(1.0) as u32;
    let mut buf = Image::new(w, h);
    let root = file.canvas.root_folder;
    render_children(file, root, &mut buf, opts, true, &mut None)?;
    if let Some(bg) = opts.background {
        flatten(&mut buf, bg);
    }
    Ok(buf)
}

/// Render a single layer in isolation (diagnostics, thumbnails, tests).
pub fn render_layer(file: &ClipFile, main_id: i64, scale: f64) -> Result<Image> {
    let Some(layer) = file.layers.get(&main_id) else {
        return Err(crate::Error::format(format!("no layer {main_id}")));
    };
    let (pw, ph) = file.canvas.pixel_size();
    let w = ((pw as f64) * scale / 100.0).round().max(1.0) as u32;
    let h = ((ph as f64) * scale / 100.0).round().max(1.0) as u32;
    let mut buf = Image::new(w, h);
    if let Some(surface) = layer_surface(file, layer, scale)? {
        apply_surface(file, layer, &surface.image, &mut buf, &RenderOptions { scale, background: None, include_hidden: true }, surface.offset, None, None)?;
    }
    Ok(buf)
}

pub fn flatten(img: &mut Image, bg: [u8; 4]) {
    for p in img.data.chunks_exact_mut(4) {
        let a = p[3] as f32 / 255.0;
        for i in 0..3 {
            p[i] = (p[i] as f32 * a + bg[i] as f32 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        }
        p[3] = 255;
    }
}

fn render_children(
    file: &ClipFile,
    folder: i64,
    buf: &mut Image,
    opts: &RenderOptions,
    parent_visible: bool,
    clip_alpha: &mut Option<Vec<u8>>,
) -> Result<()> {
    for child_id in file.children(folder) {
        let Some(layer) = file.layers.get(&child_id) else { continue };
        let visible = parent_visible && (layer.is_visible() || opts.include_hidden);
        if layer.is_folder() {
            if layer.composite == blend::MODE_PASS_THROUGH {
                // Pass-through folders composite straight into the parent, so
                // their children's clipping state stays in effect. A folder
                // mask or opacity, however, must attenuate the group's net
                // contribution after the children have seen the real backdrop.
                let mask = if layer.mask_mipmap != 0 {
                    layer_mask(file, layer, opts.scale)?
                } else {
                    None
                };
                if mask.is_none() && layer.alpha() >= 0.999 {
                    render_children(file, child_id, buf, opts, visible, clip_alpha)?;
                } else if visible {
                    let backdrop = buf.clone();
                    let mut rendered = buf.clone();
                    let mut inner_clip = clip_alpha.clone();
                    render_children(file, child_id, &mut rendered, opts, true, &mut inner_clip)?;
                    let factor = opts.scale / 100.0;
                    let mask_ox = (layer.mask_offset_x as f64 * factor).round() as i32;
                    let mask_oy = (layer.mask_offset_y as f64 * factor).round() as i32;
                    apply_group_contribution(
                        &backdrop,
                        &mut rendered,
                        mask.as_ref(),
                        (mask_ox, mask_oy),
                        layer.alpha(),
                    );
                    *buf = rendered;
                    *clip_alpha = inner_clip;
                } else {
                    *clip_alpha = None;
                }
            } else {
                if visible {
                    let mut tmp = Image::new(buf.width, buf.height);
                    let mut inner_clip = None;
                    render_children(file, child_id, &mut tmp, opts, true, &mut inner_clip)?;
                    let mask = if layer.mask_mipmap != 0 {
                        layer_mask(file, layer, opts.scale)?
                    } else {
                        None
                    };
                    let clip = if layer.clip != 0 { clip_alpha.as_deref() } else { None };
                    apply_surface(file, layer, &tmp, buf, opts, (0, 0), clip, mask.as_ref())?;
                    // CSP lets a layer (or a whole folder) be clipped to the
                    // composite below it. A clipped folder keeps the current
                    // clipping base; an unclipped one becomes the new base.
                    if layer.clip == 0 {
                        *clip_alpha = Some(folder_alpha(&mut tmp, mask.as_ref(), layer, opts.scale));
                    } else {
                        // The folder's own alpha is part of the clipped result.
                        let own = folder_alpha(&mut tmp, mask.as_ref(), layer, opts.scale);
                        if let Some(base) = clip_alpha.as_deref_mut() {
                            for (b, o) in base.chunks_exact_mut(4).zip(own.chunks_exact(4)) {
                                b[3] = ((b[3] as u32 * o[3] as u32) / 255) as u8;
                            }
                        }
                    }
                } else {
                    *clip_alpha = None;
                }
            }
            continue;
        }
        if layer.is_adjustment() {
            if visible {
                let clip = if layer.clip != 0 { clip_alpha.as_deref() } else { None };
                apply_adjustment(file, layer, buf, opts, clip)?;
            }
            if layer.clip == 0 {
                *clip_alpha = None;
            }
            continue;
        }
        if !visible {
            continue;
        }
        if layer.render_mipmap == 0 {
            continue;
        }
        let Some(surface) = layer_surface(file, layer, opts.scale)? else { continue };
        let clip = if layer.clip != 0 { clip_alpha.as_deref() } else { None };
        let mask = layer_mask(file, layer, opts.scale)?;
        apply_surface(file, layer, &surface.image, buf, opts, surface.offset, clip, mask.as_ref())?;
        if layer.clip == 0 {
            let alpha = final_alpha(layer, &surface.image, &surface.offset, mask.as_ref(), buf.width, buf.height, opts.scale);
            *clip_alpha = Some(alpha);
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LayerSurface {
    pub image: Image,
    /// Canvas coordinates in the destination image.
    pub offset: (i32, i32),
}

/// Decode a layer's render surface at the requested scale.
pub fn layer_surface(file: &ClipFile, layer: &Layer, scale: f64) -> Result<Option<LayerSurface>> {
    let chain = file.mip_chain(layer.render_mipmap)?;
    if chain.is_empty() {
        return Ok(None);
    }
    // CSP stores many lower mips as empty size tables; the 100% surface is
    // the authoritative pixels. Pick the highest-scale level that carries
    // content and resample it to the requested scale.
    let mut candidates: Vec<&crate::model::MipLevel> = chain.iter().collect();
    // Prefer the mip closest above the requested scale: decoding a 100 %
    // surface only to average it down is far more expensive than using the
    // 25 % surface CSP already stored. Empty mips are skipped as before.
    let mut with_pixels: Vec<(&crate::model::MipLevel, crate::model::Offscreen)> = Vec::new();
    for level in &candidates {
        if let Some(off) = file.offscreen(level.offscreen)? {
            let has_pixels = off
                .external_id
                .as_deref()
                .map(|id| file.external_payload(id).is_some())
                .unwrap_or(false);
            if has_pixels || off.attribute.init.has_color {
                with_pixels.push((level, off));
            }
        }
    }
    with_pixels.sort_by(|a, b| a.0.scale.partial_cmp(&b.0.scale).unwrap());
    let chosen = with_pixels
        .iter()
        .find(|(l, _)| l.scale >= scale - 1e-6)
        .or_else(|| with_pixels.last())
        .map(|(_, o)| o.clone());
    candidates.clear();
    let Some(off) = chosen else {
        return vector_surface(file, layer, scale, chain.len());
    };
    // A non-paper layer whose mip chain has no pixel payload at all is a
    // placeholder CSP fills from its frame/background binding; painting the
    // init colour as a full canvas would cover every layer below it.
    if !layer.is_paper()
        && off
            .external_id
            .as_deref()
            .map(|id| file.external_payload(id).is_none())
            .unwrap_or(true)
    {
        return Ok(None);
    }
    let mut img = image_from_surface(file, &off, scale)?;
    // Text layers store their white speech-bubble shape as a 1-bit grey
    // surface; the text itself lives in `TextLayerString`. Paint the shape
    // with the bubble fill instead of the ink bit.
    if off.attribute.channels == 1
        && off.attribute.bit_depth == 1
        && file.has_text_layer(layer.main_id)
    {
        for px in img.data.chunks_exact_mut(4) {
            px[0] = 255;
            px[1] = 255;
            px[2] = 255;
        }
    }
    let factor = scale / 100.0;
    // `LayerOffsetX/Y` places the layer; `LayerRenderOffscrOffsetX/Y` records
    // the crop CSP applied when it stored a surface that does not start at the
    // canvas origin. The surface origin is the sum of both: using the layer
    // offset alone shifted partially cropped colour layers by up to 352 px
    // (measured on lolita-en_01, mean 33.2 -> 21.2 at 25 %).
    let (ox, oy) = (
        ((layer.offset_x + layer.render_offset_x) as f64 * factor).round() as i32,
        ((layer.offset_y + layer.render_offset_y) as f64 * factor).round() as i32,
    );
    let target_w = ((off.attribute.width as f64) * factor).round().max(1.0) as u32;
    let target_h = ((off.attribute.height as f64) * factor).round().max(1.0) as u32;
    let img = if target_w != img.width || target_h != img.height {
        resize(&img, target_w, target_h)
    } else {
        img
    };
    Ok(Some(LayerSurface { image: img, offset: (ox, oy) }))
}

/// Rasterise a vector layer (line art, frame borders) when the mip chain has
/// no pixels: CSP stores those strokes only in `VectorObjectList`.
fn vector_surface(
    file: &ClipFile,
    layer: &Layer,
    scale: f64,
    _chain_len: usize,
) -> Result<Option<LayerSurface>> {
    // Frame layers (`SpecialRenderType = 14`) store rectangles with their own
    // border style and need a dedicated renderer.
    if layer.special_render_type == 14 {
        return Ok(None);
    }
    let objects = file.vector_objects(layer.main_id)?;
    if objects.is_empty() {
        return Ok(None);
    }
    // Tone layers (`LayerType & 2`) store screentone fills: the strokes are
    // the shape of the tone area, not ink. Render them as a soft dark wash
    // (the measured average effect of a dot pattern) rather than solid black.
    let tone = layer.kind & 2 != 0;
    let color = if tone { [0, 0, 0, 255] } else { crate::vector::layer_color(layer) };
    let alpha_scale = if tone { 0.35 } else { 1.0 };
    let (cw, ch) = file.canvas.pixel_size();
    let w = ((cw as f64) * scale / 100.0).round().max(1.0) as u32;
    let h = ((ch as f64) * scale / 100.0).round().max(1.0) as u32;
    let img = crate::vector::rasterize(
        &objects,
        w,
        h,
        scale / 100.0,
        file.canvas.resolution / 25.4,
        color,
        alpha_scale,
    );
    Ok(Some(LayerSurface { image: img, offset: (0, 0) }))
}

fn image_from_surface(file: &ClipFile, off: &Offscreen, _scale: f64) -> Result<Image> {
    let native = file.decode_surface(off)?;
    Ok(Image { width: off.attribute.width, height: off.attribute.height, data: native })
}

pub fn layer_mask(file: &ClipFile, layer: &Layer, scale: f64) -> Result<Option<Image>> {
    if layer.mask_mipmap == 0 {
        return Ok(None);
    }
    let chain = file.mip_chain(layer.mask_mipmap)?;
    if chain.is_empty() {
        return Ok(None);
    }
    let Some(level) = file.mip_at(&chain, scale) else { return Ok(None) };

    let mut chosen: Option<(&crate::model::MipLevel, Image)> = None;
    if let Some(off) = file.offscreen(level.offscreen)? {
        if has_payload(file, &off) {
            chosen = Some((level, image_from_surface(file, &off, scale)?));
        }
    }
    if chosen.is_none() {
        // CSP often stores the low-resolution mask mips as empty placeholders
        // while the full-resolution mip still carries the real shape (panel
        // masks, old frame masks). Prefer the smallest higher mip with data.
        let mut candidates: Vec<&crate::model::MipLevel> = chain
            .iter()
            .filter(|m| m.scale > level.scale + 1e-6)
            .collect();
        candidates.sort_by(|a, b| a.scale.partial_cmp(&b.scale).unwrap());
        for cand in candidates {
            let Some(o) = file.offscreen(cand.offscreen)? else { continue };
            if !has_payload(file, &o) {
                continue;
            }
            let m = image_from_surface(file, &o, scale)?;
            if has_alpha(&m) {
                chosen = Some((cand, m));
                break;
            }
        }
    }
    let Some((level, mut img)) = chosen else {
        // No mask pixels anywhere: the mask cannot be applied. Treating it as
        // fully opaque would hide the layer; CSP shows its content.
        return Ok(None);
    };
    let mut chosen_scale = level.scale;
    if !has_alpha(&img) {
        let mut candidates: Vec<&crate::model::MipLevel> = chain
            .iter()
            .filter(|m| m.scale > chosen_scale + 1e-6)
            .collect();
        candidates.sort_by(|a, b| a.scale.partial_cmp(&b.scale).unwrap());
        for cand in candidates {
            let Some(o) = file.offscreen(cand.offscreen)? else { continue };
            if !has_payload(file, &o) {
                continue;
            }
            let m = image_from_surface(file, &o, scale)?;
            if has_alpha(&m) {
                chosen_scale = cand.scale;
                img = m;
                break;
            }
        }
    }
    let factor = scale / chosen_scale;
    let tw = ((img.width as f64) * factor).round().max(1.0) as u32;
    let th = ((img.height as f64) * factor).round().max(1.0) as u32;
    if tw != img.width || th != img.height {
        img = resize(&img, tw, th);
    }
    Ok(Some(img))
}

/// Effective alpha (0..255 per pixel, RGBA layout) of a folder composite.
///
/// Used as the clipping base when the layer above a folder is clipped to it.
fn folder_alpha(img: &mut Image, mask: Option<&Image>, layer: &Layer, scale: f64) -> Vec<u8> {
    let factor = scale / 100.0;
    let mask_ox = (layer.mask_offset_x as f64 * factor).round() as i32;
    let mask_oy = (layer.mask_offset_y as f64 * factor).round() as i32;
    if let Some(m) = mask {
        for y in 0..img.height {
            for x in 0..img.width {
                let mx = x as i32 - mask_ox;
                let my = y as i32 - mask_oy;
                let i = ((y * img.width + x) * 4) as usize;
                if mx >= 0 && my >= 0 && mx < m.width as i32 && my < m.height as i32 {
                    let mi = ((my as u32 * m.width + mx as u32) * 4) as usize;
                    let f = m.data[mi + 3] as u32;
                    img.data[i + 3] = ((img.data[i + 3] as u32 * f) / 255) as u8;
                } else {
                    img.data[i + 3] = 0;
                }
            }
        }
    }
    img.data.clone()
}

fn has_payload(file: &ClipFile, off: &Offscreen) -> bool {
    match off.external_id.as_deref() {
        Some(id) => file.external_payload(id).is_some(),
        None => off.attribute.init.has_color,
    }
}

fn has_alpha(img: &Image) -> bool {
    img.data.chunks_exact(4).any(|p| p[3] != 0)
}

/// Mask a pass-through folder's net contribution against its real backdrop.
/// Children see the actual canvas pixels; only the final group contribution is
/// interpolated by the folder opacity and mask. This preserves overlapping and
/// blended children instead of multiplying each child's opacity.
fn apply_group_contribution(
    backdrop: &Image,
    rendered: &mut Image,
    mask: Option<&Image>,
    mask_offset: (i32, i32),
    opacity: f32,
) {
    let (mask_ox, mask_oy) = mask_offset;
    let opacity = opacity.clamp(0.0, 1.0);
    for y in 0..rendered.height {
        for x in 0..rendered.width {
            let index = ((y * rendered.width + x) * 4) as usize;
            let bg_a = backdrop.data[index + 3] as f32 / 255.0;
            let fg_a = rendered.data[index + 3] as f32 / 255.0;
            let mut coverage = opacity;
            if let Some(mask) = mask {
                let mx = x as i32 - mask_ox;
                let my = y as i32 - mask_oy;
                if mx >= 0 && my >= 0 && mx < mask.width as i32 && my < mask.height as i32 {
                    let mi = ((my as u32 * mask.width + mx as u32) * 4) as usize;
                    coverage *= mask.data[mi + 3] as f32 / 255.0;
                } else {
                    coverage = 0.0;
                }
            }
            let out_a = bg_a + (fg_a - bg_a) * coverage;
            if out_a <= 1e-6 {
                rendered.data[index..index + 4].fill(0);
                continue;
            }
            for channel in 0..3 {
                let bg_p = (backdrop.data[index + channel] as f32 / 255.0) * bg_a;
                let fg_p = (rendered.data[index + channel] as f32 / 255.0) * fg_a;
                let out_p = bg_p + (fg_p - bg_p) * coverage;
                rendered.data[index + channel] =
                    ((out_p / out_a).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            rendered.data[index + 3] = (out_a * 255.0).round() as u8;
        }
    }
}

fn apply_surface(
    file: &ClipFile,
    layer: &Layer,
    src: &Image,
    dst: &mut Image,
    opts: &RenderOptions,
    offset: (i32, i32),
    clip: Option<&[u8]>,
    mask: Option<&Image>,
) -> Result<()> {
    let factor = opts.scale / 100.0;
    let ox = offset.0;
    let oy = offset.1;
    let mask_ox = (layer.mask_offset_x as f64 * factor).round() as i32;
    let mask_oy = (layer.mask_offset_y as f64 * factor).round() as i32;
    let opacity = layer.alpha();
    for sy in 0..src.height {
        let dy = oy + sy as i32;
        if dy < 0 || dy >= dst.height as i32 {
            continue;
        }
        for sx in 0..src.width {
            let dx = ox + sx as i32;
            if dx < 0 || dx >= dst.width as i32 {
                continue;
            }
            let si = ((sy * src.width + sx) * 4) as usize;
            let di = ((dy as u32 * dst.width + dx as u32) * 4) as usize;
            let mut a = src.data[si + 3] as f32 / 255.0 * opacity;
            if let Some(m) = mask {
                let mx = sx as i32 + ox - mask_ox;
                let my = sy as i32 + oy - mask_oy;
                if mx >= 0 && my >= 0 && mx < m.width as i32 && my < m.height as i32 {
                    let mi = ((my as u32 * m.width + mx as u32) * 4) as usize;
                    a *= m.data[mi + 3] as f32 / 255.0;
                } else {
                    a = 0.0;
                }
            }
            if let Some(c) = clip {
                let ci = (dy as usize * dst.width as usize + dx as usize) * 4;
                let cw = dst.width as usize;
                let _ = cw;
                if ci + 3 < c.len() {
                    a *= c[ci + 3] as f32 / 255.0;
                }
            }
            if a <= 0.0 {
                continue;
            }
            let cs = [
                src.data[si] as f32 / 255.0,
                src.data[si + 1] as f32 / 255.0,
                src.data[si + 2] as f32 / 255.0,
            ];
            let ab = dst.data[di + 3] as f32 / 255.0;
            let cb = [
                dst.data[di] as f32 / 255.0,
                dst.data[di + 1] as f32 / 255.0,
                dst.data[di + 2] as f32 / 255.0,
            ];
            let (rgb, ao) = blend::over(layer.composite, cb, ab, cs, a);
            dst.data[di] = (rgb[0] * 255.0).round().clamp(0.0, 255.0) as u8;
            dst.data[di + 1] = (rgb[1] * 255.0).round().clamp(0.0, 255.0) as u8;
            dst.data[di + 2] = (rgb[2] * 255.0).round().clamp(0.0, 255.0) as u8;
            dst.data[di + 3] = (ao * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    // Silence unused field warning while keeping the signature symmetric.
    let _ = file;
    Ok(())
}

fn final_alpha(
    layer: &Layer,
    src: &Image,
    offset: &(i32, i32),
    mask: Option<&Image>,
    dst_w: u32,
    dst_h: u32,
    scale: f64,
) -> Vec<u8> {
    let factor = scale / 100.0;
    let mask_ox = (layer.mask_offset_x as f64 * factor).round() as i32;
    let mask_oy = (layer.mask_offset_y as f64 * factor).round() as i32;
    let mut out = vec![0u8; dst_w as usize * dst_h as usize * 4];
    let opacity = layer.alpha();
    for sy in 0..src.height {
        let dy = offset.1 + sy as i32;
        if dy < 0 || dy >= dst_h as i32 {
            continue;
        }
        for sx in 0..src.width {
            let dx = offset.0 + sx as i32;
            if dx < 0 || dx >= dst_w as i32 {
                continue;
            }
            let si = ((sy * src.width + sx) * 4) as usize;
            let di = ((dy as u32 * dst_w + dx as u32) * 4) as usize;
            let mut a = src.data[si + 3] as f32 * opacity;
            if let Some(m) = mask {
                let mx = sx as i32 + offset.0 - mask_ox;
                let my = sy as i32 + offset.1 - mask_oy;
                if mx >= 0 && my >= 0 && mx < m.width as i32 && my < m.height as i32 {
                    let mi = ((my as u32 * m.width + mx as u32) * 4) as usize;
                    a *= m.data[mi + 3] as f32 / 255.0;
                } else {
                    a = 0.0;
                }
            }
            let a = a.round().clamp(0.0, 255.0) as u8;
            out[di] = 255;
            out[di + 1] = 255;
            out[di + 2] = 255;
            out[di + 3] = a;
        }
    }
    out
}

fn apply_adjustment(
    file: &ClipFile,
    layer: &Layer,
    buf: &mut Image,
    opts: &RenderOptions,
    clip: Option<&[u8]>,
) -> Result<()> {
    let Some(blob) = layer.filter_layer_info.as_deref() else { return Ok(()) };
    let Some(filter) = crate::adjust::Filter::parse(blob) else { return Ok(()) };
    let mask = layer_mask(file, layer, opts.scale)?;
    // The mask is stored as its own mip chain; bring it to the composite
    // resolution so both share a coordinate system.
    let mask = mask.map(|m| {
        if m.width != buf.width || m.height != buf.height {
            resize(&m, buf.width, buf.height)
        } else {
            m
        }
    });
    let factor = opts.scale / 100.0;
    let offset = (
        (layer.mask_offset_x as f64 * factor).round() as i32,
        (layer.mask_offset_y as f64 * factor).round() as i32,
    );
    crate::adjust::apply_to_image(buf, &filter, layer.alpha() as f64, mask.as_ref(), offset, clip);
    Ok(())
}

/// Area-average resize (box filter), used when a mip is missing.
pub fn resize(src: &Image, nw: u32, nh: u32) -> Image {
    if nw == src.width && nh == src.height {
        return src.clone();
    }
    let mut out = Image::new(nw, nh);
    let sx = src.width as f64 / nw as f64;
    let sy = src.height as f64 / nh as f64;
    for y in 0..nh {
        let y0 = (y as f64 * sy).floor() as u32;
        let y1 = (((y + 1) as f64 * sy).ceil() as u32).min(src.height).max(y0 + 1);
        for x in 0..nw {
            let x0 = (x as f64 * sx).floor() as u32;
            let x1 = (((x + 1) as f64 * sx).ceil() as u32).min(src.width).max(x0 + 1);
            let mut acc = [0f64; 4];
            let mut n = 0f64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = ((yy * src.width + xx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src.data[i + c] as f64;
                    }
                    n += 1.0;
                }
            }
            let o = ((y * nw + x) * 4) as usize;
            for c in 0..4 {
                out.data[o + c] = (acc[c] / n).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

pub fn to_png(img: &Image, path: &std::path::Path) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(f);
    let mut enc = png::Encoder::new(w, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(&img.data)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Image {
        let mut image = Image::new(width, height);
        for pixel in image.data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&rgba);
        }
        image
    }

    #[test]
    fn pass_through_group_mask_interpolates_net_contribution() {
        let backdrop = solid(3, 1, [255, 255, 255, 255]);
        let mut rendered = solid(3, 1, [220, 30, 30, 255]);
        let mut mask = Image::new(3, 1);
        mask.data[3] = 0;
        mask.data[7] = 128;
        mask.data[11] = 255;
        apply_group_contribution(&backdrop, &mut rendered, Some(&mask), (0, 0), 1.0);
        assert_eq!(&rendered.data[0..4], &[255, 255, 255, 255]);
        assert!(rendered.data[4].abs_diff(237) <= 1, "{:?}", rendered.data);
        assert!(rendered.data[5].abs_diff(142) <= 1, "{:?}", rendered.data);
        assert!(rendered.data[6].abs_diff(142) <= 1, "{:?}", rendered.data);
        assert_eq!(&rendered.data[8..12], &[220, 30, 30, 255]);
    }

    #[test]
    fn pass_through_group_mask_uses_native_mask_offset() {
        let backdrop = solid(4, 1, [255, 255, 255, 255]);
        let mut rendered = solid(4, 1, [220, 30, 30, 255]);
        let mut mask = Image::new(3, 1);
        mask.data[3] = 0;
        mask.data[7] = 128;
        mask.data[11] = 255;
        apply_group_contribution(&backdrop, &mut rendered, Some(&mask), (1, 0), 1.0);
        // The mask starts at document x=1: x=0 is outside and fully masked,
        // x=1 is transparent, x=2 is 50 %, and x=3 is opaque.
        assert_eq!(&rendered.data[0..4], &[255, 255, 255, 255]);
        assert_eq!(&rendered.data[4..8], &[255, 255, 255, 255]);
        assert!(rendered.data[8].abs_diff(237) <= 1, "{:?}", rendered.data);
        assert!(rendered.data[9].abs_diff(142) <= 1, "{:?}", rendered.data);
        assert_eq!(&rendered.data[12..16], &[220, 30, 30, 255]);
    }
}
