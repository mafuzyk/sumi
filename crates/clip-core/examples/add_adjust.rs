// Add one HSV correction layer above a given layer, with the writer's own
// construction, so it can be compared with a correction CSP created itself.
// usage: add_adjust <in.clip> <out.clip> <above_main_id> <hue>
use clip_core::adjust::{Filter, FilterKind};
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap();
    let output = args.next().unwrap();
    let above: i64 = args.next().unwrap().parse()?;
    let hue: f64 = args.next().unwrap().parse()?;
    let mut writer = ClipWriter::load(&input)?;
    let filter = Filter {
        kind: FilterKind::Hsv { hue, saturation: 0.0, luminosity: 0.0 },
        raw: Vec::new(),
    };
    let blob = filter.to_blob().expect("HSV encodes");
    let id = writer.add_adjustment(above, "Sumi correction", &blob)?;
    writer.dirty = true;
    writer.save(&output)?;
    println!("wrote {output}: correction {id} above {above}, hue {hue}");
    Ok(())
}
