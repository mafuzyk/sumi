// Rewrite folder rows so they match the shape CLIP STUDIO PAINT writes
// (masking bits, composite mode, render mipmap/thumbnail copied from the first
// child). Used to bisect which column makes CSP reject a group document.
//
// usage: patch_folder <input.clip> <output.clip> [key=value ...]
//   masking=32        LayerMasking for folder rows (default: leave alone)
//   composite=30      LayerComposite for folder rows
//   mip=child|none    copy mip/thumbnail from the first child, or zero them
use clip_core::write::ClipWriter;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap();
    let output = args.next().unwrap();
    let mut masking: Option<i64> = None;
    let mut composite: Option<i64> = None;
    let mut mip: Option<String> = None;
    for arg in args {
        match arg.split_once('=') {
            Some(("masking", v)) => masking = Some(v.parse()?),
            Some(("composite", v)) => composite = Some(v.parse()?),
            Some(("mip", v)) => mip = Some(v.to_string()),
            _ => anyhow::bail!("unknown argument {arg}"),
        }
    }
    let mut writer = ClipWriter::load(&input)?;
    let folders: Vec<i64> = {
        let db = writer.db();
        let mut q = db.prepare("select MainId from Layer where LayerFolder & 1 and LayerName <> ''")?;
        let rows = q.query_map([], |r| r.get(0))?;
        let out: Vec<i64> = rows.filter_map(Result::ok).collect();
        out
    };
    for folder in folders {
        if let Some(m) = masking {
            writer.db.execute(
                "update Layer set LayerMasking=?1 where MainId=?2",
                rusqlite::params![m, folder],
            )?;
        }
        if let Some(c) = composite {
            writer.db.execute(
                "update Layer set LayerComposite=?1 where MainId=?2",
                rusqlite::params![c, folder],
            )?;
        }
        match mip.as_deref() {
            Some("child") => {
                let child: i64 = {
                    let db = writer.db();
                    db.query_row(
                        "select LayerFirstChildIndex from Layer where MainId=?1",
                        [folder],
                        |r| r.get(0),
                    )?
                };
                let (m, t): (i64, i64) = {
                    let db = writer.db();
                    db.query_row(
                        "select coalesce(LayerRenderMipmap,0), coalesce(LayerRenderThumbnail,0) \
                         from Layer where MainId=?1",
                        [child],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?
                };
                println!("folder {folder}: child {child} mip={m} thumb={t}");
                if m != 0 {
                    writer.set_layer_mips(folder, Some(m), None)?;
                }
                if t != 0 {
                    writer.set_layer_thumbnail_offscreen(folder, Some(t), None)?;
                }
            }
            Some("none") => {
                writer.set_layer_mips(folder, Some(0), None)?;
                writer.set_layer_thumbnail_offscreen(folder, Some(0), None)?;
            }
            Some("thumb") => {
                let child: i64 = {
                    let db = writer.db();
                    db.query_row(
                        "select LayerFirstChildIndex from Layer where MainId=?1",
                        [folder],
                        |r| r.get(0),
                    )?
                };
                let thumb: i64 = {
                    let db = writer.db();
                    db.query_row(
                        "select coalesce(LayerRenderThumbnail,0) from Layer where MainId=?1",
                        [child],
                        |r| r.get(0),
                    )?
                };
                let exists: i64 = {
                    let db = writer.db();
                    db.query_row("select count(*) from LayerThumbnail where MainId=?1", [thumb], |r| r.get(0))?
                };
                println!("folder {folder}: child {child} thumb={thumb} rows={exists}");
                if thumb != 0 && exists > 0 {
                    writer.db.execute(
                        "update Layer set LayerRenderThumbnail=?1 where MainId=?2",
                        rusqlite::params![thumb, folder],
                    )?;
                }
            }
            Some(other) => anyhow::bail!("mip={other} is not child|none|thumb"),
            None => {}
        }
    }
    writer.dirty = true;
    writer.save(&output)?;
    println!("wrote {output}");
    Ok(())
}
