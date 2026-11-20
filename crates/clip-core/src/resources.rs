//! Locate the user's CLIP STUDIO PAINT resources.
//!
//! Nothing here writes: the CSP installation and the user resource directory are
//! read-only inputs. Resolution is explicit - environment override first, then
//! the Wine layout derived from `SUMI_CSP_ROOT`, then nothing. Callers keep
//! their built-in presets when a group is unset, so the application runs on a
//! machine that never had CSP installed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Tool database inside the installation.
const INSTALLED_TOOL_DATABASE: &str = "drive_c/Program Files/CELSYS/CLIP STUDIO 1.5/CLIP STUDIO PAINT/Settings/PAINT/Tool/english/EditImageTool.todb";
/// All-users material root; the authored `material_0.layer` files live here.
const INSTALLED_MATERIALS: &str = "drive_c/ProgramData/CELSYS/CLIPStudio/Common/Material/Install";
/// The installed English brush preset database.
const INSTALLED_BRUSH_PRESETS: &str = "drive_c/Program Files/CELSYS/CLIP STUDIO 1.5/CLIP STUDIO PAINT/Settings/PAINT/BrushPreset/english/BrushPreset.bfps";

/// Every read-only resource the editor and the CLI can load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resources {
    /// Wine prefix or equivalent root given by `SUMI_CSP_ROOT`.
    pub csp_root: Option<PathBuf>,
    pub tool_database: Option<PathBuf>,
    pub user_tool_database: Option<PathBuf>,
    pub brush_presets: Option<PathBuf>,
    pub user_brush_presets: Option<PathBuf>,
    /// User material root (`.../CLIPStudioCommon/Material/Install`).
    pub materials: Option<PathBuf>,
    pub install_materials: Option<PathBuf>,
    pub color_sets: Option<PathBuf>,
}

impl Resources {
    /// True when nothing was configured, so the built-in presets are the only
    /// source.
    pub fn is_empty(&self) -> bool {
        self.tool_database.is_none()
            && self.user_tool_database.is_none()
            && self.brush_presets.is_none()
            && self.user_brush_presets.is_none()
            && self.materials.is_none()
            && self.install_materials.is_none()
            && self.color_sets.is_none()
    }

    /// One line for the status bar or the CLI: which roots were found.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "No CLIP STUDIO PAINT resources configured; using built-in presets"
                .to_string();
        }
        let mut parts = Vec::new();
        if self.tool_database.is_some() {
            parts.push("tool database");
        }
        if self.brush_presets.is_some() {
            parts.push("brush presets");
        }
        if self.materials.is_some() || self.install_materials.is_some() {
            parts.push("materials");
        }
        if self.color_sets.is_some() {
            parts.push("colour sets");
        }
        format!("CLIP STUDIO PAINT resources: {}", parts.join(", "))
    }
}

/// Resolve every path from a variable lookup. Kept free of `std::env` so tests
/// can supply a fixed environment without touching the process.
pub fn resolve_all(get: impl Fn(&str) -> Option<PathBuf>) -> Resources {
    let root = get("SUMI_CSP_ROOT");
    let derived = |relative: &str| root.as_ref().map(|root| root.join(relative));
    Resources {
        tool_database: get("SUMI_TOOL_DATABASE").or_else(|| derived(INSTALLED_TOOL_DATABASE)),
        install_materials: get("SUMI_INSTALL_MATERIALS").or_else(|| derived(INSTALLED_MATERIALS)),
        user_tool_database: get("SUMI_USER_TOOL_DATABASE"),
        brush_presets: get("SUMI_BRUSH_PRESETS").or_else(|| derived(INSTALLED_BRUSH_PRESETS)),
        user_brush_presets: get("SUMI_USER_BRUSH_PRESETS"),
        materials: get("SUMI_MATERIAL_ROOT"),
        color_sets: get("SUMI_COLOR_SETS"),
        csp_root: root,
    }
}

/// The choices the editor persists for itself. Only `csp_root` is written by
/// the UI today; the remaining keys exist so a user can pin one location
/// without environment variables.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub csp_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_database: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_tool_database: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush_presets: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_brush_presets: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materials: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_materials: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_sets: Option<PathBuf>,
}

/// `$XDG_CONFIG_HOME/sumi-csp/resources.json`, falling back to `~/.config`.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("sumi-csp").join("resources.json"))
}

/// Missing or malformed files yield the defaults: the editor keeps working with
/// built-in presets instead of failing to start.
pub fn load_config_from(path: &Path) -> ResourceConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_config_to(path: &Path, config: &ResourceConfig) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config).map_err(std::io::Error::other)?;
    // Publish through a temporary so an interrupted write cannot truncate the
    // previous settings.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)
}

/// Map one `SUMI_*` name onto the persisted config.
pub fn config_lookup(config: &ResourceConfig, name: &str) -> Option<PathBuf> {
    let value = match name {
        "SUMI_CSP_ROOT" => config.csp_root.as_ref(),
        "SUMI_TOOL_DATABASE" => config.tool_database.as_ref(),
        "SUMI_USER_TOOL_DATABASE" => config.user_tool_database.as_ref(),
        "SUMI_BRUSH_PRESETS" => config.brush_presets.as_ref(),
        "SUMI_USER_BRUSH_PRESETS" => config.user_brush_presets.as_ref(),
        "SUMI_MATERIAL_ROOT" => config.materials.as_ref(),
        "SUMI_INSTALL_MATERIALS" => config.install_materials.as_ref(),
        "SUMI_COLOR_SETS" => config.color_sets.as_ref(),
        _ => None,
    };
    value.cloned()
}

fn env_lookup(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// Resolve from the process environment, then the persisted config.
pub fn load() -> Resources {
    let config = config_path()
        .map(|path| load_config_from(&path))
        .unwrap_or_default();
    resolve_all(|name| env_lookup(name).or_else(|| config_lookup(&config, name)))
}

/// First path that exists, so a caller can keep a fallback chain.
pub fn first_existing(candidates: &[Option<PathBuf>]) -> Option<PathBuf> {
    candidates
        .iter()
        .flatten()
        .find(|path| path.is_file())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<PathBuf> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| PathBuf::from(value))
        }
    }

    #[test]
    fn persisted_config_resolves_like_the_environment() {
        let config = ResourceConfig {
            csp_root: Some(PathBuf::from("/opt/csp")),
            color_sets: Some(PathBuf::from("/data/default.pcs")),
            ..ResourceConfig::default()
        };
        let resources = resolve_all(|name| config_lookup(&config, name));
        assert!(resources
            .tool_database
            .as_deref()
            .is_some_and(|path| path.starts_with("/opt/csp")));
        assert_eq!(
            resources.color_sets.as_deref(),
            Some(Path::new("/data/default.pcs"))
        );
        // The environment still wins over the file.
        let env_root = PathBuf::from("/srv/csp");
        let resources = resolve_all(|name| {
            if name == "SUMI_CSP_ROOT" {
                Some(env_root.clone())
            } else {
                config_lookup(&config, name)
            }
        });
        assert!(resources
            .tool_database
            .as_deref()
            .is_some_and(|path| path.starts_with("/srv/csp")));
    }

    #[test]
    fn config_round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resources.json");
        assert_eq!(load_config_from(&path), ResourceConfig::default());
        let config = ResourceConfig {
            csp_root: Some(PathBuf::from("/opt/csp")),
            ..ResourceConfig::default()
        };
        save_config_to(&path, &config).unwrap();
        assert_eq!(load_config_from(&path), config);
        // Malformed content falls back to the defaults instead of failing.
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(load_config_from(&path), ResourceConfig::default());
    }

    #[test]
    fn empty_environment_resolves_nothing() {
        let resources = resolve_all(env(&[]));
        assert!(resources.is_empty());
        assert_eq!(
            resources.summary(),
            "No CLIP STUDIO PAINT resources configured; using built-in presets"
        );
    }

    #[test]
    fn csp_root_derives_the_installed_tool_database_and_materials() {
        let resources = resolve_all(env(&[("SUMI_CSP_ROOT", "/opt/csp")]));
        assert!(!resources.is_empty());
        assert_eq!(
            resources.tool_database.as_deref(),
            Some(Path::new("/opt/csp").join(INSTALLED_TOOL_DATABASE).as_path())
        );
        assert_eq!(
            resources.install_materials.as_deref(),
            Some(Path::new("/opt/csp").join(INSTALLED_MATERIALS).as_path())
        );
        // The user resource directory cannot be derived: it names a Windows
        // user, so it stays an explicit override.
        assert!(resources.user_tool_database.is_none());
        assert!(resources.materials.is_none());
        assert!(resources
            .brush_presets
            .as_deref()
            .is_some_and(|path| path.ends_with("english/BrushPreset.bfps")));
    }

    #[test]
    fn explicit_overrides_win_over_the_derived_layout() {
        let resources = resolve_all(env(&[
            ("SUMI_CSP_ROOT", "/opt/csp"),
            ("SUMI_TOOL_DATABASE", "/data/tool.todb"),
            ("SUMI_MATERIAL_ROOT", "/data/materials"),
        ]));
        assert_eq!(
            resources.tool_database.as_deref(),
            Some(Path::new("/data/tool.todb"))
        );
        assert_eq!(
            resources.materials.as_deref(),
            Some(Path::new("/data/materials"))
        );
        // The non-overridden derived path still comes from the root.
        assert!(resources
            .install_materials
            .as_deref()
            .is_some_and(|path| path.starts_with("/opt/csp")));
    }

    #[test]
    fn summary_names_the_groups_that_were_found() {
        let resources = resolve_all(env(&[
            ("SUMI_TOOL_DATABASE", "/data/tool.todb"),
            ("SUMI_COLOR_SETS", "/data/default.pcs"),
        ]));
        assert_eq!(
            resources.summary(),
            "CLIP STUDIO PAINT resources: tool database, colour sets"
        );
    }
}
