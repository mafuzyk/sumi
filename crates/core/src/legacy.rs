//! Read-only backward compatibility constants and helpers for pre-Sumi artworks, packages,
//! brush files, settings keys, and plugins. Keeping these isolated here allows opening existing
//! files, user data stores, and already-unpacked plugins seamlessly while keeping the rest of the
//! codebase Sumi-only.

/// Artwork JSON format tag used by earlier pre-Sumi saves.
pub const ARTWORK_FORMAT: &str = "procraft-artwork";
/// Native layered package extension used before `.sumi`.
pub const ARTWORK_EXTENSION: &str = "procraft";
/// Plugin manifest filename inside legacy plugin archives and unpacked folders.
pub const PLUGIN_MANIFEST: &str = "craft-extension.json";
/// Legacy plugin package extensions accepted when importing plugins.
pub const PLUGIN_EXTENSIONS: [&str; 2] = ["procraft-plugin", "procraftplugin"];
/// Legacy key under `"compat"` in plugin manifests.
pub const PLUGIN_COMPAT_KEY: &str = "procraft";
/// Legacy WASM import module name provided by the host to plugin actions.
pub const PLUGIN_HOST_MODULE: &str = "procraft";
/// Prefix on exported WASM symbols in legacy plugins.
pub const PLUGIN_SYMBOL_PREFIX: &str = "procraft_";
/// Reserved plugin ID prefix blocked from third-party plugins alongside `sumi.`.
pub const PLUGIN_OWN_ID_PREFIX: &str = "procraft.";
/// Legacy bundled plugin ID namespace migrated to `app.sumi.`.
pub const PLUGIN_LEGACY_ID_PREFIX: &str = "io.github.artcraftandroid.";
/// Current bundled plugin ID namespace.
pub const PLUGIN_ID_PREFIX: &str = "app.sumi.";
/// Legacy single-brush JSON version field name.
pub const BRUSH_FIELD: &str = "procraft_brush";
/// Legacy brush-set JSON version field name.
pub const BRUSH_SET_FIELD: &str = "procraft_brush_set";
/// Unix data directory leaf used prior to `sumi`, migrated one-way on startup.
pub const APP_DIR_UNIX: &str = "procraft";
/// Windows/macOS data directory leaf used prior to `Sumi`, migrated one-way on startup.
pub const APP_DIR_DESKTOP: &str = "ProCraft";

/// Whether `format` in `artwork.json` is readable by this version of Sumi.
pub fn is_supported_artwork_format(format: &str) -> bool {
    format == "sumi-artwork" || format == ARTWORK_FORMAT
}

/// Whether `ext` matches the legacy layered package extension.
pub fn is_legacy_artwork_extension(ext: &str) -> bool {
    ext.eq_ignore_ascii_case(ARTWORK_EXTENSION)
}

/// Whether `ext` matches a legacy plugin archive extension.
pub fn is_legacy_plugin_extension(ext: &str) -> bool {
    PLUGIN_EXTENSIONS.iter().any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

/// Translates a `sumi_*` WASM export symbol to its legacy `procraft_*` fallback name.
pub fn legacy_wasm_export(sumi_symbol: &str) -> Option<String> {
    sumi_symbol.strip_prefix("sumi_").map(|rest| format!("{PLUGIN_SYMBOL_PREFIX}{rest}"))
}

/// Translates an `app.sumi.*` bundled plugin ID to its historical ID if present on disk.
pub fn legacy_bundled_id(id: &str) -> Option<String> {
    id.strip_prefix(PLUGIN_ID_PREFIX).map(|rest| format!("{PLUGIN_LEGACY_ID_PREFIX}{rest}"))
}

/// Normalizes a legacy plugin key, setting key, or kind (`<old_ns>.<rest>`) to `app.sumi.<rest>`.
pub fn normalize_plugin_key(key: &str) -> Option<String> {
    key.strip_prefix(PLUGIN_LEGACY_ID_PREFIX).map(|rest| format!("{PLUGIN_ID_PREFIX}{rest}"))
}

/// Whether a saved `wanted` kind/key refers to the installed `listed` kind/key (directly or via legacy ID migration).
pub fn matches_plugin_key(listed: &str, wanted: &str) -> bool {
    listed == wanted || normalize_plugin_key(wanted).as_deref() == Some(listed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::{Value, json};

    use super::*;
    use crate::plugin_file::{self, sha256};
    use crate::plugins::{PluginControl, Plugins};
    use crate::{CanvasSession, brush_library::is_brush_text};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("sumi-legacy-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn slider_value(plugins: &Plugins, id: &str, suffix: &str) -> f32 {
        let card = plugins.plugins().into_iter().find(|p| p.id == id).unwrap();
        let key = format!("{id}.{suffix}");
        card.controls
            .into_iter()
            .find_map(|c| match c {
                PluginControl::Slider { control } if control.key == key => Some(control.value),
                _ => None,
            })
            .unwrap()
    }

    fn rewrite_json_key(raw: &str, old_key: &str, new_key: &str) -> String {
        let mut obj: Value = serde_json::from_str(raw).unwrap();
        let map = obj.as_object_mut().unwrap();
        let val = map.remove(old_key).unwrap();
        map.insert(new_key.to_owned(), val);
        serde_json::to_string(&obj).unwrap()
    }

    #[test]
    fn opens_legacy_artwork_folder_and_package() {
        let dir = temp_dir("artwork");
        let artwork_dir = dir.join("folder");
        let session = CanvasSession::new(24, 16);
        session.save(artwork_dir.to_string_lossy().into_owned()).unwrap();

        let json_path = artwork_dir.join("artwork.json");
        let mut raw: Value = serde_json::from_str(&fs::read_to_string(&json_path).unwrap()).unwrap();
        raw["format"] = json!(ARTWORK_FORMAT);
        fs::write(&json_path, serde_json::to_vec(&raw).unwrap()).unwrap();

        let reopened = CanvasSession::open(artwork_dir.to_string_lossy().into_owned()).unwrap();
        assert_eq!(reopened.layers().len(), session.layers().len());

        let doc = sumi_engine::document::Document::new(32, 20);
        let layer_id = doc.current();
        let bytes_len = 32 * 20 * 4;
        let pixels = HashMap::from([(layer_id, vec![0u8; bytes_len])]);
        let composite = vec![0u8; bytes_len];
        let pkg = sumi_engine::native::write(&sumi_engine::native::Export {
            document: &doc,
            pixels: &pixels,
            composite: &composite,
            thumbnail_png: &[],
            name: "LegacyPkg",
            author: "",
            strokes: 0,
            tracked_seconds: 0.0,
            seed: 42,
            time_lapse: None,
            reference: None,
            reference_picture: None,
        })
        .unwrap();
        let pkg_path = dir.join(format!("legacy.{ARTWORK_EXTENSION}"));
        assert!(is_legacy_artwork_extension(ARTWORK_EXTENSION));
        fs::write(&pkg_path, pkg).unwrap();

        let opened_pkg = CanvasSession::open_package(pkg_path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(opened_pkg.layers().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_legacy_brush_and_brush_set_json() {
        let session = CanvasSession::new(16, 16);
        let brush_json = session.brush_file(0, 0).unwrap();
        let legacy_brush = rewrite_json_key(&brush_json, "sumi_brush", BRUSH_FIELD);
        assert!(is_brush_text(&legacy_brush));
        assert_eq!(session.import_brush_file(0, legacy_brush), Some(0));

        let set_json = session.brush_set_file(0).unwrap();
        let legacy_set = rewrite_json_key(&set_json, "sumi_brush_set", BRUSH_SET_FIELD);
        assert!(is_brush_text(&legacy_set));
        assert_eq!(session.import_brush_file(0, legacy_set), Some(0));
    }

    #[test]
    fn imports_and_runs_legacy_plugin_package_and_folder() {
        assert!(is_legacy_plugin_extension(PLUGIN_EXTENSIONS[0]));
        assert!(is_legacy_plugin_extension(PLUGIN_EXTENSIONS[1]));
        let wat_src = format!(
            r#"(module
                (import "{PLUGIN_HOST_MODULE}" "add_layer" (func $add (result i64)))
                (func (export "{PLUGIN_SYMBOL_PREFIX}action") (param i32) (result i32)
                    (drop (call $add))
                    (i32.const 0)))"#
        );
        let wasm = wat::parse_str(wat_src).unwrap();
        let id = "com.example.legacy";
        let manifest = json!({
            "schema": 1,
            "id": id,
            "name": "Legacy Plugin",
            "version": "1.0.0",
            "holds": ["hooks"],
            "publisher": { "name": "Tester" },
            "description": "Tests legacy ABI and manifest compatibility.",
            "compat": { PLUGIN_COMPAT_KEY: { "min_app": "0.0.1", "interface": 2 } },
            "files": [{ "path": crate::plugin_run::MODULE, "sha256": sha256(&wasm), "size": wasm.len() }],
            "permissions": ["artwork.write"],
            "hooks": [],
            "actions": [{ "key": "act", "label": "Run" }],
        });
        let mut zip = sumi_engine::zip::ZipWriter::new();
        zip.add(PLUGIN_MANIFEST, manifest.to_string().as_bytes());
        zip.add(crate::plugin_run::MODULE, &wasm);
        let pkg = zip.finish();

        let store = temp_dir("plugin-store");
        let plugins = Plugins::with_store(store.to_string_lossy().into_owned());
        assert_eq!(plugins.install(pkg), None);
        assert!(plugins.set_on(id.to_owned(), true));

        let session = CanvasSession::new(16, 16);
        let before = session.layers().len();
        assert!(plugins.run_action(format!("{id}.act"), Arc::clone(&session)));
        assert_eq!(session.layers().len(), before + 1);

        // Verify unpacked store folder using legacy manifest filename reads back cleanly too.
        let unpacked = store.join("com.example.unpacked").join("1.0.0");
        fs::create_dir_all(&unpacked).unwrap();
        let mut unpacked_manifest = manifest;
        unpacked_manifest["id"] = json!("com.example.unpacked");
        fs::write(unpacked.join(PLUGIN_MANIFEST), unpacked_manifest.to_string()).unwrap();
        fs::write(unpacked.join(crate::plugin_run::MODULE), &wasm).unwrap();
        let all = plugin_file::read_all(&store);
        assert!(all.iter().any(|m| m.id == "com.example.unpacked"));
        let _ = fs::remove_dir_all(&store);
    }

    #[test]
    fn migrates_legacy_bundled_store_without_clash_or_duplicates() {
        let store = temp_dir("bundled-migrate");
        let legacy_gap_id = format!("{PLUGIN_LEGACY_ID_PREFIX}gap-closing");
        let legacy_outline_id = format!("{PLUGIN_LEGACY_ID_PREFIX}outline");

        let gap_dir = store.join(&legacy_gap_id).join("0.9.0");
        fs::create_dir_all(&gap_dir).unwrap();
        let gap_wat = format!(
            r#"(module
                (func (export "{PLUGIN_SYMBOL_PREFIX}position") (param f32 f32) (result i64) (i64.const 0))
                (func (export "{PLUGIN_SYMBOL_PREFIX}control") (param i32 f32)))"#
        );
        let gap_wasm = wat::parse_str(gap_wat).unwrap();
        let gap_manifest = json!({
            "schema": 1,
            "id": legacy_gap_id,
            "name": "ColorDrop gap closing",
            "version": "0.9.0",
            "holds": ["hooks"],
            "publisher": { "name": "ArtCraft" },
            "description": "Legacy gap closing.",
            "compat": { PLUGIN_COMPAT_KEY: { "interface": 1 } },
            "files": [{ "path": crate::plugin_run::MODULE, "sha256": sha256(&gap_wasm), "size": gap_wasm.len() }],
            "permissions": [],
            "hooks": ["position"],
            "controls": [
                { "key": "gap", "type": "slider", "label": "Gap", "least": 0, "most": 32, "step": 1, "starts": 0, "keeps": "fill_gap" }
            ],
        });
        fs::write(gap_dir.join(PLUGIN_MANIFEST), gap_manifest.to_string()).unwrap();
        fs::write(gap_dir.join(crate::plugin_run::MODULE), &gap_wasm).unwrap();
        fs::write(store.join("added"), format!("{legacy_gap_id}\n{legacy_outline_id} off\n")).unwrap();

        let plugins = Plugins::with_bundled(store.to_string_lossy().into_owned());
        let list = plugins.plugins();
        assert_eq!(list.iter().filter(|p| p.id == "app.sumi.gap-closing").count(), 1);
        assert!(!list.iter().any(|p| p.id.starts_with(PLUGIN_LEGACY_ID_PREFIX)));
        assert!(!list.iter().any(|p| p.id == "app.sumi.outline"), "removed outline must stay removed");

        // Legacy interface settings keys still map onto the migrated plugin controls.
        plugins.read_kept(format!("{PLUGIN_LEGACY_ID_PREFIX}interface-colour.hue"), "45".to_owned());
        assert!((slider_value(&plugins, "app.sumi.interface-colour", "hue") - 45.0).abs() < 0.01);
        let _ = fs::remove_dir_all(&store);
    }
}
