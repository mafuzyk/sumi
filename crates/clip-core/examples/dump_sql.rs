use clip_core::write::ClipWriter;
use rusqlite::types::ValueRef;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap();
    let sql = args.next().unwrap();
    let writer = ClipWriter::load(&path)?;
    let db = writer.db();
    let mut st = db.prepare(&sql)?;
    let cols = st.column_count();
    let names: Vec<String> = st.column_names().iter().map(|c| c.to_string()).collect();
    println!("{}", names.join(" | "));
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        let mut cells = Vec::with_capacity(cols);
        for i in 0..cols {
            let cell = match row.get_ref(i)? {
                ValueRef::Null => "NULL".to_string(),
                ValueRef::Integer(x) => x.to_string(),
                ValueRef::Real(x) => x.to_string(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                ValueRef::Blob(b) => format!("<blob {}>", b.len()),
            };
            cells.push(cell);
        }
        println!("{}", cells.join(" | "));
    }
    Ok(())
}
