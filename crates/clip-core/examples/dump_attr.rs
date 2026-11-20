// Print the decoded attribute of every offscreen that belongs to a mip chain.
// usage: dump_attr <file.clip>
use clip_core::attr::Attr;
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dump_attr <file.clip>");
    let writer = ClipWriter::load(&path)?;
    let db = writer.db();
    let mut st = db.prepare(
        "select o.MainId, o.LayerId, length(o.Attribute), o.Attribute \
         from Offscreen o order by o.MainId",
    )?;
    let rows: Vec<(i64, i64, i64, Vec<u8>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(Result::ok)
        .collect();
    for (id, layer, len, bytes) in rows {
        let attr = Attr::decode(&bytes)?;
        println!(
            "offscreen {id} layer {layer} attr {len}B: {}x{} blocks {}x{} of {}x{} mode={} alpha={} channels={} depth={} init={:?}",
            attr.width,
            attr.height,
            attr.cols,
            attr.rows,
            attr.block_w,
            attr.block_h,
            attr.color_mode,
            attr.alpha_flag,
            attr.channels,
            attr.bit_depth,
            attr.init
        );
    }
    Ok(())
}
