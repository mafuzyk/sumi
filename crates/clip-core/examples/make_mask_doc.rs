// Write a document whose layer mask is a grayscale gradient in the RGB
// channels (alpha stays opaque), to learn how CSP reads mask coverage.
// usage: make_mask_doc <out.clip> [alpha|gray]
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).expect("usage: make_mask_doc <out.clip> [alpha|gray]");
    let mode = std::env::args().nth(2).unwrap_or_else(|| "gray".to_string());
    let (width, height) = (800u32, 600u32);
    let mut writer = ClipWriter::blank(width, height)?;
    let paint = writer.db().query_row(
        "select MainId from Layer where not (LayerFolder & 1) and LayerType not in (256, 1584) limit 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let red: Vec<u8> = [230u8, 90, 60, 255].repeat((width * height) as usize);
    writer.set_pixels(paint, &red, width, height)?;
    writer.add_layer_mask(paint)?;
    let mut mask = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let v = ((y * 255) / (height - 1)) as u8;
        for _ in 0..width {
            if mode == "alpha" {
                mask.extend_from_slice(&[255, 255, 255, v]);
            } else {
                mask.extend_from_slice(&[v, v, v, 255]);
            }
        }
    }
    writer.set_mask_pixels(paint, &mask, width, height)?;
    writer.save(&out)?;
    println!("wrote {out} mask in RGB grayscale, layer {paint}");
    Ok(())
}
