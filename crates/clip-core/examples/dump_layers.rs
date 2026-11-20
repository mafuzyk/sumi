use clip_core::write::ClipWriter;

const COLS: &[&str] = &[
    "MainId",
    "LayerName",
    "LayerType",
    "LayerFolder",
    "LayerComposite",
    "LayerOpacity",
    "LayerVisibility",
    "LayerClip",
    "LayerMasking",
    "LayerRenderMipmap",
    "LayerRenderThumbnail",
    "LayerLayerMaskMipmap",
    "LayerLayerMaskThumbnail",
    "LayerNextIndex",
    "LayerFirstChildIndex",
    "DrawToRenderOffscreenType",
];

fn main() -> anyhow::Result<()> {
    for path in std::env::args().skip(1) {
        let writer = ClipWriter::load(&path)?;
        let db = writer.db();
        println!("== {path}");
        let select = COLS
            .iter()
            .map(|c| format!("ifnull(quote({c}),'NULL')"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut st = db.prepare(&format!("select {select} from Layer order by MainId"))?;
        let rows: Vec<Vec<String>> = st
            .query_map([], |r| {
                (0..COLS.len())
                    .map(|i| r.get::<_, String>(i))
                    .collect::<Result<Vec<_>, _>>()
            })?
            .filter_map(Result::ok)
            .collect();
        for row in rows {
            println!(
                "{}",
                COLS.iter()
                    .zip(row)
                    .map(|(c, v)| format!("{c}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }
    Ok(())
}
