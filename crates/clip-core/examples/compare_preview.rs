// Compare the composite this crate renders with the preview CLIP STUDIO PAINT
// stored in the file. The preview is CSP's own composite, so the difference is
// the semantic gap between both renderers.
//
// usage: compare_preview <file.clip> [file.clip ...]
use clip_core::model::ClipFile;
use clip_core::render::{self, RenderOptions};

fn main() -> anyhow::Result<()> {
    let mut scale = 100.0f64;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--scale" {
            scale = args.next().expect("--scale value").parse()?;
        } else {
            paths.push(arg);
        }
    }
    for path in paths {
        let file = ClipFile::open(&path)?;
        let Some(preview) = file.preview_png() else {
            println!("{path}: no CanvasPreview stored");
            continue;
        };
        let decoder = png::Decoder::new(std::io::Cursor::new(preview));
        let mut reader = decoder.read_info()?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf)?;
        let (pw, ph) = (info.width, info.height);
        let want = &buf[..info.buffer_size()];
        let img = render::render(
            &file,
            &RenderOptions {
                scale,
                background: Some([255, 255, 255, 255]),
                include_hidden: false,
            },
        )?;
        if img.width != pw || img.height != ph {
            println!(
                "{path}: preview {pw}x{ph} but render {}x{}",
                img.width, img.height
            );
            continue;
        }
        let mut sum = 0u64;
        let mut max = 0u8;
        let mut changed = 0usize;
        for (got, exp) in img.data.chunks_exact(4).zip(want.chunks_exact(4)) {
            for channel in 0..3 {
                let diff = got[channel].abs_diff(exp[channel]);
                if diff > 0 {
                    changed += 1;
                }
                sum += u64::from(diff);
                max = max.max(diff);
            }
        }
        let pixels = (pw * ph) as f64;
        println!(
            "{path}: {pw}x{ph}, differing channel samples {changed} ({}%), mean {:.3}, max {max}",
            (changed as f64 * 100.0 / (pixels * 3.0)),
            sum as f64 / (pixels * 3.0),
        );
    }
    Ok(())
}
