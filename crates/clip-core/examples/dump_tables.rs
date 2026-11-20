use clip_core::write::ClipWriter;
fn main() -> anyhow::Result<()> {
    for path in std::env::args().skip(1) {
        let writer = ClipWriter::load(&path)?;
        let db = writer.db();
        let mut st = db.prepare("select name from sqlite_master where type='table' order by name")?;
        let tables: Vec<String> = st.query_map([], |r| r.get(0))?.filter_map(Result::ok).collect();
        println!("== {path}: {} tables", tables.len());
        println!("{}", tables.join(", "));
        let folders: Vec<(i64, String, i64, i64, i64)> = {
            let mut q = db.prepare("select MainId, LayerName, LayerFolder, LayerNextIndex, LayerFirstChildIndex from Layer where LayerFolder & 1")?;
            let rows: Vec<(i64, String, i64, i64, i64)> = q
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .filter_map(Result::ok)
                .collect();
            rows
        };
        for row in folders.iter().take(4) { println!("folder {row:?}"); }
        for t in ["LayerFolder", "LayerTree", "LayerSelect"] {
            if tables.iter().any(|n| n == t) {
                let n: i64 = db.query_row(&format!("select count(*) from {t}"), [], |r| r.get(0)).unwrap_or(-1);
                let cols: Vec<String> = {
                    let mut s = db.prepare(&format!("pragma table_info({t})"))?;
                    let out: Vec<String> = s.query_map([], |r| r.get::<_, String>(1))?.filter_map(Result::ok).collect();
                    out
                };
                println!("  {t}: rows={n} cols={}", cols.join(","));
            }
        }
    }
    Ok(())
}
