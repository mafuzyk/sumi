// Run one SQL statement against the embedded database of a document and save a
// copy. Diagnostic tool: the statement is data, not a template.
//
// usage: exec_sql <input.clip> <output.clip> "<sql>"
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap();
    let output = args.next().unwrap();
    let sql = args.next().unwrap();
    let mut writer = ClipWriter::load(&input)?;
    writer.db.execute_batch(&sql)?;
    writer.dirty = true;
    writer.save(&output)?;
    println!("wrote {output}");
    Ok(())
}
