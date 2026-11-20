// Fill a layer's mask with opaque white, the state CSP writes for a fresh
// correction mask.
// usage: fill_mask_white <in.clip> <out.clip> <layer_main_id>
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap();
    let output = args.next().unwrap();
    let layer: i64 = args.next().unwrap().parse()?;
    let mut writer = ClipWriter::load(&input)?;
    let (width, height) = writer
        .mask_surface_size(layer)?
        .ok_or_else(|| anyhow::anyhow!("layer {layer} has no mask surface"))?;
    let white: Vec<u8> = [255u8, 255, 255, 255].repeat((width * height) as usize);
    writer.set_mask_pixels(layer, &white, width, height)?;
    writer.dirty = true;
    writer.save(&output)?;
    println!("wrote {output}: mask {width}x{height} filled white");
    Ok(())
}
