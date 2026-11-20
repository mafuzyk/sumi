# `sumi-clip`

Reads CLIP STUDIO PAINT (`.clip`) and Krita (`.kra`) documents into a native artwork document,
and writes a native artwork back out as either of them. Importing only reads the original file;
exporting writes a new file at the destination the caller chooses and never touches the
document an import came from.

## Public API

```rust
sumi_clip::import(path: &Path) -> Result<ImportedArtwork, ImportError>

sumi_clip::export(
    path: &Path,
    format: ExportFormat,          // Clip | Kra
    document: &Document,
    pixels: &[(LayerId, Vec<u8>)], // canvas-sized premultiplied RGBA, masks opaque grey
    name: &str,
) -> Result<Vec<ExportWarning>, ExportError>
```

`ImportedArtwork` carries the engine `Document`, canvas-sized premultiplied pixel buffers by layer
id, the artwork's name (the file's stem) and `ImportWarning`s for what the native copy could not
keep. `export` takes the same shape back, so an import can be written out again unchanged.
Content that cannot be represented is never discarded in silence: it is either named in an
`ImportError::Unsupported` / `ExportError`, or listed in the warnings.

A `.kra` import copies the file once to a file of a private directory and removes it afterwards, so
the preflight and the reader describe the same bytes even if the original changes in between. The
original is never written.

## Mapping, both directions

| Foreign document | Native artwork |
| --- | --- |
| Canvas size and resolution | `Document` width, height and dpi |
| CSP paper layer | Canvas background colour and visibility; no paper layer in the stack |
| Raster layers, bottom first | Paint layers, own opacity, visibility, blend, offsets and clipping |
| CSP folder / Krita group | Group layer after its children, with the children linked by `parent` |
| CSP/Krita transparency mask | Mask layer immediately above its parent, opaque grey coverage |
| Vector/clone/text layers | Rasterized pixels with a warning (`Rasterized*`) |
| Glow blend modes | Nearest exact blend mode, with `ApproximatedBlend` |
| Clipping the engine cannot represent | Kept unclipped, with `Unclipped` |
| Embedded ICC profile (`.kra`) | Pixels read as sRGB; a profile that is not a name this importer treats as sRGB is reported in `ProfileNotApplied` |

### Export mappings

| Native artwork | `.clip` | `.kra` |
| --- | --- | --- |
| Canvas size and dpi | `Canvas` width, height and resolution | `IMAGE` width, height, `x-res`/`y-res` and `profile="sRGB built-in"` |
| Background colour, visibility | Paper layer's pixels and visibility | Not stored (Krita has no paper); `BackgroundNotStored` |
| Paint layer name, visibility, opacity | `Layer` name, visibility bit, opacity 0..=256 | `maindoc.xml` name, `visible`, `opacity` 0..=255 |
| Blend mode | CSP `LayerComposite`, the import table inverted | Krita `compositeop` (`kra::composite_name`) |
| Pass-through group | CSP mode 30 at full opacity | `passthrough="1"` |
| Isolated group | Its exact composite mode and opacity | `passthrough="0"` and the exact `compositeop` |
| Node order | Bottom-first sibling chains (`LayerFirstChildIndex`/`LayerNextIndex`) | Bottom-first children written top-first, as Krita lists them |
| Clipping | `LayerClip` | **Refused by name**: Krita's clipping is alpha inheritance, a different model (see below) |
| Mask | Layer mask, canvas-sized, offset 0, coverage in every channel and the alpha, its own visibility bit | `transparencymask` entry, `PIXELSIZE 1` coverage, `visible` from the mask; the reader honours it, so a hidden mask is stored and never applied |
| Current layer | `CanvasCurrentLayer` | `selected="true"` on the layer |
| Alpha lock, reference layer | `LayerLock` bits | Not stored |
| Layer lock | `StateNotStored` warning | `locked` attribute |
| Text, plugin layer | Pixels with `RasterizedText` / `RasterizedPlugin` | Same |

Pixels are unpremultiplied on the way out with exact integer rounding (`a = 0` gives `rgb = 0`);
`.clip` takes straight RGBA and Krita tiled streams take straight BGRA. Masks are written from
the channels `mask_grey` reads, which is the same rule the import uses.

`.kra` files carry `mimetype`, `maindoc.xml`, `documentinfo.xml`, one `<image>/layers/<name>`
tiled entry per paint layer (64 × 64 raw BGRA tiles), a `.pixelselection` per mask, and
`mergedimage.png` / `preview.png`. The merged preview is a straight-RGBA composite of the layer
tree: Krita has no paper, so the canvas background is not part of it.

## Explicitly refused (named, never silently dropped)

`.clip` text layers and adjustment/frame layers on import, group masks that are enabled, multiple
paper layers, unknown canvas units and blend modes, pass-through groups at partial opacity, `.kra`
file/generator layers without resolved pixels, and any document that is too large or has too many
painting layers for the device.

On export: the engine's `BlendMode::Shade` (no exact CLIP STUDIO PAINT mode), any clipped layer in
`.kra`, a clipped group, and an orphan mask. A text or plugin layer is written as its pixels with
a warning instead of pretending it stays editable.

## Reader-side limits

The parser is the GPL-3.0-or-later `clip-core` from the Sumi/CSP port, kept read-only here. A `.kra`
is preflighted before it is read: `maindoc.xml` is parsed on its own and each tiled layer's five
header rows are read, never a tile. A document is refused when the reader would lose or guess part
of it:

- a colour space other than `RGBA` (CMYK, grey, Lab, none) at document or layer level;
- a layer without an explicit `nodetype`, or a node type other than paint, group, shape or clone
  (file, generator, fill and filter layers);
- a layer without a non-empty, unique `filename` or `UUID`, or two elements sharing a `filename`;
- a clone whose `clonefromuuid` is absent, points at nothing, at a group, or at another clone;
- a `compositeop` the reader would silently turn into Normal (`erase`, `behind`, any name it does
  not map), or a layer that inherits alpha from the layer below (clipping, which the reader ignores);
- paint tiles that are not four bytes per pixel, or masks that are not one byte per pixel;
- a canvas whose `IMAGE` width or height is zero or larger than the device's largest edge, a tiled
  stream with `TILEWIDTH`/`TILEHEIGHT` outside 1..=1024, more tile rows than its own entry can hold
  (each row takes at least 8 bytes), or more declared uncompressed bytes than that entry's
  compressed size times 1032 for deflate, capped at `MAX_DOCUMENT_BYTES`. All are refused before
  the reader allocates a canvas or a tile;
- a visible transform or filter mask, a mask on a group, more than one visible transparency mask on
  a layer, a mask placed away from the origin, or a mask type the reader does not apply;
- a pass-through group (`passthrough="1"`) that also declares a blend mode or partial opacity;
- any layer the reader drops or reparents: the parsed tree is reconciled with the reader's by
  filename, kind and parent after `KraDocument::open`, so a stack is never quietly shorter.

Krita paints on an unbounded canvas, so a tile outside the canvas is not refused: the reader clips
it to the canvas as it decodes the layer.

A broken archive, broken `maindoc.xml` or a missing tile entry is `ImportError::Unreadable`; the
recognized-but-unsupported cases above are `ImportError::Unsupported`. Both name the layer or
feature. The remaining limits no second reading of the archive can settle:

- group pass-through is not exposed by the reader's `KraFolder`; the preflight reads Krita's
  `passthrough` attribute itself. An isolated group that blends normally is rendered pass-through
  by this engine, so when a direct child would show the difference the import keeps the pixels and
  reports `GroupIsolationLost`;
- the colour space name is checked, the embedded ICC profile is never read: the reader decodes every
  pixel as sRGB, and no colour conversion is attempted. A `profile` on the `IMAGE` or on a layer is
  compared against the three names this importer treats as sRGB (`sRGB built-in`,
  `sRGB-elle-V2-srgbtrc.icc`, `sRGB-elle-V4-srgbtrc.icc`). Any other value — a linear-gamma sibling,
  a foreign working space, an empty value, or a real sRGB file under a name this list does not know —
  is imported with `ProfileNotApplied`, whose message says every pixel was read as sRGB. That is a
  warning, never a refusal, and a document with no `profile` attribute contradicts nothing and stays
  silent;
- the tile, LZF and blend arithmetic is the reader's own. A failure there is caught as a dropped
  layer, not as a wrong pixel;
- shape rasterization and clone translation stay approximations, reported as `RasterizedVector` and
  `RasterizedClone`;
- a mask the file switched off (`visible="0"`) is skipped by the reader's own visibility filter
  (`kra.rs:1631` for transparency masks, the same test in its transform and filter paths), so it
  never attenuates anything on import. A switched-off transparency mask is the one mask this
  document model could have held, so its absence is announced with `MaskHiddenNotRead`; a
  switched-off transform or filter mask changes no pixel and is left out silently, as its visible
  form is refused outright. The writer keeps a hidden mask in the file (`visible="0"`, and out of
  the merged preview it writes beside the layers) rather than dropping it, so what the file shows
  survives the round trip; a mask on a group is skipped for the same reason: the reader applies none.

## Writer-side limits

* `.clip` is written by cloning the bundled minimal CSP template and replacing its layer stack, so
  the SQLite schema stays one CLIP STUDIO PAINT wrote.
* `.kra` clipping is refused by name, on purpose and fail-closed. Krita spells clipping as alpha
  inheritance (`inheritalpha`), which is not the engine's model — it takes alpha from every layer
  below inside the group, where the engine clips to the paint layer directly below — and this reader
  ignores it. Writing the layer as an ordinary `paintlayer` would come back as an unclipped layer,
  so the export stops instead of changing what the file shows; split the stack into a group with a
  mask instead.
* A `.kra` is written to a temporary file in the destination's directory and published through
  `clip-core`'s `publish_file`: the temporary is fsynced, the destination's permissions are kept,
  a symlink is followed to the file it names, and the rename is atomic, so a failed export leaves
  the document that was already there untouched.
* Mask names and offsets: CSP stores no mask name, so an exported mask comes back named `Mask`;
  native masks are canvas-sized and are written at offset 0.
* Partial alpha is rounded through straight RGBA: alpha 0 and 255 round-trip exactly, other alphas
  are within one 8-bit step.
* A `.kra` always declares `profile="sRGB built-in"`. A document imported from a foreign profile was
  read as sRGB (with `ProfileNotApplied`), so exporting keeps the pixel numbers and relabels them
  sRGB: the colour the file claims changes, and no export warning says so yet.
* An isolated Normal group is written one opacity step under full, so a reader of the file does not
  take it for the pass-through group that full opacity and Normal mean in the engine.
* Editor-only state (undo history, solo/picked layers, drawing assist) is not written; layer locks
  and drawing assist are named in `StateNotStored` warnings where the format cannot hold them.

## Tests

```sh
cargo test -p sumi-clip
cargo run -p sumi-clip --example probe -- path/to/document.clip
```

All fixtures are generated by the tests themselves with `clip-core`'s writer (`.clip`) or a minimal
ZIP writer (`.kra`); no external sample files are needed. `tests/export.rs` builds a small native
document, exports it both ways, reopens it through `ClipFile::open` / `KraDocument::open` and
through `import`, and compares structure and pixels.
