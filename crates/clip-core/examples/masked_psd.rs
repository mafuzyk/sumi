//! Write a small PSD with a user layer mask so an independent reader can check
//! that the mask block and its -2 channel are well formed.
//!
//! Usage: cargo run -p clip-core --example masked_psd -- /tmp/masked.psd [psb]

use clip_core::psd::{write_psd_layers, PsdImageRef, PsdLayerData};
use clip_core::render::Image;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "masked.psd".to_string()),
    );
    let image = Image::filled(8, 6, [30, 60, 90, 255]);
    let mut mask = Image::filled(4, 3, [255, 255, 255, 255]);
    for y in 0..3 {
        for x in 2..4 {
            mask.data[((y * 4 + x) * 4 + 3) as usize] = 0;
        }
    }
    let layers = [PsdLayerData {
        name: "Masked",
        image: PsdImageRef {
            width: 8,
            height: 6,
            data: &image.data,
        },
        offset: (0, 0),
        opacity: 1.0,
        visible: true,
        blend: 0,
        clip: false,
        mask: Some(PsdImageRef {
            width: 4,
            height: 3,
            data: &mask.data,
        }),
        mask_offset: (2, 1),
    }];
    let psb = std::env::args().any(|arg| arg == "psb");
    write_psd_layers(&path, 8, 6, &layers, &image, psb)?;
    println!("wrote {}", path.display());
    Ok(())
}
