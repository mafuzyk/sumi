use clip_core::write::ClipWriter;
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap();
    let id: i64 = args.next().unwrap().parse()?;
    let writer = ClipWriter::load(&path)?;
    let db = writer.db();
    let cols: Vec<String> = {
        let mut s = db.prepare("pragma table_info(Layer)")?;
        let out: Vec<String> = s.query_map([], |r| r.get::<_, String>(1))?.filter_map(Result::ok).collect();
        out
    };
    let select = cols.iter().map(|c| format!("quote({c})")).collect::<Vec<_>>().join(", ");
    let row: Vec<Option<String>> = db.query_row(
        &format!("select {select} from Layer where MainId={id}"),
        [],
        |r| Ok((0..cols.len()).map(|i| r.get::<_, Option<String>>(i).unwrap_or(None)).collect()),
    )?;
    for (c, v) in cols.iter().zip(row) {
        if let Some(v) = v { if v != "NULL" { println!("{c} = {v}"); } }
    }
    Ok(())
}
