//! Renders sample text layouts so the shapes can be inspected as PNGs.

use clip_core::textlayout::{render, Fonts, TextAlign, TextSpec};
use std::path::Path;

fn save(name: &str, spec: TextSpec, fonts: &Fonts) {
    let Some((image, origin)) = render(&spec, fonts) else {
        println!("{name}: nothing rendered");
        return;
    };
    println!("{name}: {}x{} origin={origin:?}", image.width, image.height);
    let file = std::fs::File::create(Path::new("/tmp").join(name)).unwrap();
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(&image.data).unwrap();
}

fn main() {
    let fonts = Fonts::new();
    let mut spec = TextSpec::new("Sumi text\nsecond line");
    spec.size = 48.0;
    spec.color = [20, 20, 20, 255];
    save("text-horizontal.png", spec.clone(), &fonts);

    let mut vertical = TextSpec::new("縦書きテキスト");
    vertical.size = 40.0;
    vertical.vertical = true;
    vertical.family = "Noto Sans CJK JP".to_string();
    save("text-vertical.png", vertical, &fonts);

    let mut ruby = TextSpec::new("漢字");
    ruby.size = 56.0;
    ruby.family = "Noto Sans CJK JP".to_string();
    ruby.ruby = vec![(0, 6, "かんじ".to_string())];
    ruby.align = TextAlign::Center;
    save("text-ruby.png", ruby, &fonts);

    let mut path = TextSpec::new("text on a curve");
    path.size = 32.0;
    path.on_path = Some(vec![(0.0, 120.0), (120.0, 40.0), (240.0, 120.0)]);
    save("text-on-path.png", path, &fonts);
}
