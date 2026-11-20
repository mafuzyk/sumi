// Rewrite one column of one Layer row, to test which flag makes CLIP STUDIO
// PAINT honour a mask written by this crate.
//
// usage: patch_layer <input.clip> <output.clip> <main_id> <column> <value>
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap();
    let output = args.next().unwrap();
    let layer: i64 = args.next().unwrap().parse()?;
    let column = args.next().unwrap();
    let value: i64 = args.next().unwrap().parse()?;
    let mut writer = ClipWriter::load(&input)?;
    // Only integer columns are needed here; the name is checked against the
    // table so a typo cannot silently do nothing.
    let exists: i64 = {
        let db = writer.db();
        db.query_row(
            "select count(*) from pragma_table_info('Layer') where name=?1",
            [&column],
            |r| r.get(0),
        )?
    };
    if exists == 0 {
        // Older CSP documents (the bundled template among them) predate some
        // columns. Adding one is how a document written from that template can
        // still carry the fields current CSP reads.
        writer.db.execute(
            &format!("alter table Layer add column \"{column}\" integer"),
            [],
        )?;
        println!("added missing column {column}");
    }
    writer.db.execute(
        &format!("update Layer set {column}=?1 where MainId=?2"),
        rusqlite::params![value, layer],
    )?;
    writer.dirty = true;
    writer.save(&output)?;
    println!("wrote {output}: Layer {column}={value} for MainId {layer}");
    Ok(())
}
