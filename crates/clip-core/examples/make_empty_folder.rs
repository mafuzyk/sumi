// Write a minimal document that contains one empty layer folder, to test
// whether CLIP STUDIO PAINT accepts a folder row without children.
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).expect("usage: make_empty_folder <out.clip>");
    let mut writer = ClipWriter::blank(400, 300)?;
    let folder = writer.add_folder(0, "Grupo vazio")?;
    writer.save(&out)?;
    println!("wrote {out} folder={folder}");
    Ok(())
}
