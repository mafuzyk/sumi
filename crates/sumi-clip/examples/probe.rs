//! Reads a `.clip`/`.kra` file and prints what the adapter makes of it, for manual checks.

use std::path::Path;

fn main() {
    let Some(path) = std::env::args_os().nth(1).map(std::path::PathBuf::from) else {
        eprintln!("usage: probe <file.clip|file.kra>");
        std::process::exit(2);
    };
    match sumi_clip::import(Path::new(&path)) {
        Ok(imported) => {
            println!("{}", path.display());
            println!(
                "  {} x {}, dpi {:.0}, background visible {}",
                imported.document.width(),
                imported.document.height(),
                imported.document.dpi,
                imported.document.background_visible
            );
            for layer in imported.document.layers() {
                println!(
                    "  current={:?} id={:?} name={:?} kind={:?} parent={:?} visible={} opacity={:.3} blend={:?} clipped={}",
                    imported.document.current(),
                    layer.id,
                    layer.name,
                    layer.kind,
                    layer.parent,
                    layer.visible,
                    layer.opacity,
                    layer.blend,
                    layer.clipped
                );
            }
            for (id, pixels) in &imported.pixels {
                let painted = pixels.chunks_exact(4).filter(|px| px.get(3).is_some_and(|a| *a != 0)).count();
                println!("  pixels {:?}: {} painted of {}", id, painted, pixels.len() / 4);
            }
            for warning in &imported.warnings {
                println!("  warning: {}", warning.message());
            }
        }
        Err(error) => {
            println!("{}: {error}", path.display());
            std::process::exit(1);
        }
    }
}
