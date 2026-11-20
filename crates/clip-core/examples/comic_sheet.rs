//! Renders a balloon/frame/panel/tone sample sheet for visual inspection.

use clip_core::comic::{
    draw_balloon, draw_frame, draw_screentone, Balloon, BalloonShape, Rect,
};
use clip_core::render::Image;

fn save(name: &str, image: &Image) {
    let file = std::fs::File::create(std::path::Path::new("/tmp").join(name)).unwrap();
    let mut encoder = png::Encoder::new(file, image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(&image.data).unwrap();
}

fn main() {
    let mut image = Image::filled(600, 400, [251, 200, 220, 255]);
    let bounds = Rect::new(60.0, 40.0, 300.0, 160.0);
    draw_balloon(
        &mut image,
        &Balloon {
            shape: BalloonShape::Rounded,
            bounds,
            tail: Some((bounds.x + bounds.w * 1.1, bounds.y + bounds.h * 1.5)),
            fill: [255, 255, 255, 255],
            stroke: Some(([20, 20, 20, 255], 3.0)),
        },
    );
    draw_frame(&mut image, Rect::new(400.0, 40.0, 160.0, 120.0), 4.0, [20, 20, 20, 255]);
    draw_screentone(
        &mut image,
        Rect::new(400.0, 200.0, 160.0, 160.0),
        45.0,
        8.0,
        0.8,
        96,
        [20, 20, 20, 255],
    );
    save("comic-sheet.png", &image);
    println!("comic-sheet.png written");
}
