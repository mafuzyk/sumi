// Patch the init colour of a layer's 100% render and mask offscreens to opaque
// white, the state CSP writes for fresh correction surfaces.
// usage: patch_offscreen_init <in.clip> <out.clip> <layer_main_id>
use clip_core::attr::Attr;
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap();
    let output = args.next().unwrap();
    let layer: i64 = args.next().unwrap().parse()?;
    let mut writer = ClipWriter::load(&input)?;
    let offs: Vec<(i64, Vec<u8>)> = {
        let db = writer.db();
        let mut st = db.prepare(
            "select o.MainId, o.Attribute from Offscreen o \
             join MipmapInfo mi on mi.Offscreen = o.MainId \
             where mi.LayerId = ?1 and mi.ThisScale = 100",
        )?;
        let rows = st.query_map([layer], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.filter_map(Result::ok).collect()
    };
    for (off, blob) in &offs {
        let mut attr = Attr::decode(blob)?;
        attr.init.has_color = true;
        attr.init.rgba = u32::MAX;
        if attr.channels != 0 {
            attr.init.extra = vec![u32::MAX; 4];
        }
        writer.db.execute(
            "update Offscreen set Attribute=?1 where MainId=?2",
            rusqlite::params![attr.encode(), off],
        )?;
        println!("offscreen {off}: init white, channels {}", attr.channels);
    }
    writer.dirty = true;
    writer.save(&output)?;
    println!("wrote {output}");
    Ok(())
}
