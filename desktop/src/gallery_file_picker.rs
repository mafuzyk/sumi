//! An in-window file chooser for the Linux gallery. Unlike the system dialog, this works when
//! neither an XDG FileChooser portal nor Zenity is installed (including portable builds).
//! Windows and macOS keep their platform-native pickers.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::{Context, Id, Modal, ScrollArea};

use crate::files;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Import,
    Artwork,
    Photo,
    Reference,
    Insert,
    InsertPhoto,
    Custom,
}

/// A generic in-window Linux chooser for paths outside the Start Deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    OpenOne,
    OpenMany,
    Save,
    Folder,
}

struct Custom {
    title: String,
    selection: Selection,
    extensions: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    Cancel,
    Files(Vec<PathBuf>),
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    name: String,
    directory: bool,
}

pub struct Picker {
    kind: Kind,
    directory: PathBuf,
    location: String,
    entries: Vec<Entry>,
    selected: BTreeSet<PathBuf>,
    filter: String,
    problem: Option<String>,
    custom: Option<Custom>,
    replace: Option<PathBuf>,
}

impl Picker {
    pub fn new(kind: Kind) -> Self {
        let directory = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut picker = Self {
            kind,
            location: directory.to_string_lossy().into_owned(),
            directory,
            entries: Vec::new(),
            selected: BTreeSet::new(),
            filter: String::new(),
            problem: None,
            custom: None,
            replace: None,
        };
        picker.refresh();
        picker
    }

    pub fn custom(title: String, selection: Selection, extensions: Vec<String>, suggested: Option<String>) -> Self {
        let mut picker = Self::new(Kind::Custom);
        picker.custom =
            Some(Custom { title, selection, extensions: extensions.into_iter().map(|e| e.to_lowercase()).collect() });
        if let Some(name) = suggested.filter(|s| !s.trim().is_empty()) {
            picker.location = picker.directory.join(name).to_string_lossy().into_owned();
        }
        picker.refresh();
        picker
    }

    fn selection(&self) -> Selection {
        self.custom.as_ref().map_or(Selection::OpenOne, |c| c.selection)
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    fn refresh(&mut self) {
        self.entries.clear();
        let read = match std::fs::read_dir(&self.directory) {
            Ok(read) => read,
            Err(err) => {
                self.problem = Some(format!("Can't list {}: {err}", self.directory.display()));
                return;
            }
        };
        for entry in read {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let directory = path.is_dir();
            if !directory && (self.selection() == Selection::Folder || !path.is_file() || !self.accepts(&path)) {
                continue;
            }
            self.entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), path, directory });
        }
        self.entries.sort_by(|a, b| {
            b.directory.cmp(&a.directory).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.problem = None;
    }

    fn accepts(&self, path: &Path) -> bool {
        match self.kind {
            Kind::Import => files::gallery_accepts(path, false),
            Kind::Artwork => {
                files::gallery_accepts(path, false) && !files::is_palette_file(path) && !files::is_brush_file(path)
            }
            Kind::Photo | Kind::Reference => files::gallery_accepts(path, true),
            Kind::Insert | Kind::InsertPhoto => {
                files::gallery_accepts(path, true)
                    || path.extension().is_some_and(|ext| {
                        ["mp4", "m4v", "mov", "wmv", "avi", "mkv"]
                            .contains(&ext.to_string_lossy().to_lowercase().as_str())
                    })
            }
            Kind::Custom => self.custom.as_ref().is_some_and(|c| {
                c.extensions.is_empty()
                    || path.extension().is_some_and(|ext| c.extensions.contains(&ext.to_string_lossy().to_lowercase()))
            }),
        }
    }

    fn enter(&mut self, directory: PathBuf) {
        if directory.is_dir() {
            self.directory = directory;
            self.location = self.directory.to_string_lossy().into_owned();
            self.selected.clear();
            self.replace = None;
            self.refresh();
        } else {
            self.problem = Some("That directory isn't available.".into());
        }
    }

    /// Resolve a typed path (relative to the current folder), or the selected files.
    fn choose(&self) -> Result<Vec<PathBuf>, String> {
        if self.selection() == Selection::Folder {
            let folder = if self.location.trim().is_empty() {
                self.directory.clone()
            } else {
                let entered = Path::new(self.location.trim());
                if entered.is_absolute() { entered.to_path_buf() } else { self.directory.join(entered) }
            };
            return if folder.is_dir() { Ok(vec![folder]) } else { Err("Select an existing folder.".into()) };
        }
        let mut paths: Vec<_> = if !self.selected.is_empty() {
            self.selected.iter().cloned().collect()
        } else {
            let entered = self.location.trim();
            if entered.is_empty() {
                return Err("Choose a file first.".into());
            }
            let p = Path::new(entered);
            vec![if p.is_absolute() { p.to_path_buf() } else { self.directory.join(p) }]
        };
        if self.selection() == Selection::Save
            && let Some(extension) = self.custom.as_ref().and_then(|custom| custom.extensions.first())
            && let Some(path) = paths.first_mut()
        {
            // Match the package writer's `with_extension` before checking overwrite.
            // Otherwise typing "name.jpg" and saving as Sumi could silently replace
            // an existing "name.sumi" that the chooser never asked to overwrite.
            path.set_extension(extension);
        }
        for path in &paths {
            if self.selection() == Selection::Save {
                if !path.parent().is_some_and(Path::is_dir) || path.is_dir() {
                    return Err(format!("Invalid destination: {}", path.display()));
                }
                continue;
            }
            if !path.is_file() {
                return Err(format!("Not a file: {}", path.display()));
            }
            if !self.accepts(path) {
                return Err(format!("Unsupported file type: {}", path.display()));
            }
            std::fs::File::open(path).map_err(|e| format!("Can't read {}: {e}", path.display()))?;
        }
        Ok(paths)
    }

    pub fn show(&mut self, ctx: &Context) -> Option<Choice> {
        let mut result = None;
        let id = if self.kind == Kind::Custom { "sumi-generic-file-picker" } else { "sumi-gallery-file-picker" };
        let modal = Modal::new(Id::unique(id)).show(ctx, |ui| {
            ui.set_min_width(ctx.content_rect().width().min(640.0).max(240.0));
            ui.heading(match self.kind {
                Kind::Import => "Open or import artwork",
                Kind::Artwork => "Open artwork",
                Kind::Photo => "Choose a photo",
                Kind::Reference => "Import reference image",
                Kind::Insert => "Insert a picture or video",
                Kind::InsertPhoto => "Insert a photo",
                Kind::Custom => self.custom.as_ref().map_or("Choose a file", |c| c.title.as_str()),
            });
            ui.horizontal(|ui| {
                if ui.button("Up").clicked()
                    && let Some(parent) = self.directory.parent()
                {
                    self.enter(parent.to_path_buf());
                }
                if ui.button("Home").clicked()
                    && let Some(home) = std::env::var_os("HOME")
                {
                    self.enter(PathBuf::from(home));
                }
                ui.label(self.directory.display().to_string());
            });
            ui.horizontal(|ui| {
                ui.label("Location");
                if ui.text_edit_singleline(&mut self.location).changed() {
                    self.selected.clear();
                    self.replace = None;
                }
                if ui.button("Go").clicked() {
                    let path = PathBuf::from(self.location.trim());
                    let path = if path.is_absolute() { path } else { self.directory.join(path) };
                    if path.is_dir() {
                        self.enter(path);
                    } else {
                        self.problem = Some("Enter a directory or select a file below.".into());
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Search");
                ui.text_edit_singleline(&mut self.filter).on_hover_text("Filter filenames in this folder");
            });
            ScrollArea::vertical().max_height(320.0).min_scrolled_height(180.0).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let filter = self.filter.to_lowercase();
                let matching: Vec<_> = self
                    .entries
                    .iter()
                    .filter(|entry| filter.is_empty() || entry.name.to_lowercase().contains(&filter))
                    .take(2049)
                    .cloned()
                    .collect();
                let mut visible = 0;
                for entry in matching {
                    if visible == 2048 {
                        ui.label("More files: narrow the Search to find them.");
                        break;
                    }
                    visible += 1;
                    if entry.directory {
                        if ui.button(format!("📁 {}", entry.name)).clicked() {
                            self.enter(entry.path);
                        }
                    } else {
                        let selected = self.selected.contains(&entry.path);
                        if ui.selectable_label(selected, &entry.name).clicked() {
                            if !(matches!(self.kind, Kind::Import | Kind::Artwork)
                                || self.selection() == Selection::OpenMany)
                                || !ui.input(|i| i.modifiers.ctrl)
                            {
                                self.selected.clear();
                            }
                            if !selected {
                                self.selected.insert(entry.path.clone());
                            } else {
                                self.selected.remove(&entry.path);
                            }
                            self.location = entry.path.to_string_lossy().into_owned();
                            self.problem = None;
                            self.replace = None;
                        }
                    }
                }
            });
            if let Some(problem) = &self.problem {
                ui.colored_label(egui::Color32::LIGHT_RED, problem);
            }
            ui.separator();
            if let Some(path) = &self.replace {
                ui.colored_label(egui::Color32::YELLOW, format!("Replace existing file: {}?", path.display()));
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    result = Some(Choice::Cancel);
                }
                let action = match self.selection() {
                    Selection::OpenOne | Selection::OpenMany => "Open",
                    Selection::Save => {
                        if self.replace.is_some() {
                            "Replace"
                        } else {
                            "Save"
                        }
                    }
                    Selection::Folder => "Choose folder",
                };
                if ui.button(action).clicked() {
                    match self.choose() {
                        Ok(paths)
                            if self.selection() == Selection::Save
                                && paths.first().is_some_and(|path| path.exists())
                                && self.replace.as_ref() != paths.first() =>
                        {
                            self.replace = paths.first().cloned();
                        }
                        Ok(paths) => result = Some(Choice::Files(paths)),
                        Err(error) => self.problem = Some(error),
                    }
                }
            });
        });
        if result.is_none() && modal.should_close() {
            result = Some(Choice::Cancel);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, Picker, Selection};

    #[test]
    fn generic_picker_opens_real_files_and_folders_without_a_portal() {
        let dir = std::env::temp_dir().join(format!("sumi-generic-picker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("photo.PNG");
        let txt = dir.join("notes.txt");
        std::fs::write(&png, b"image content not needed by picker").unwrap();
        std::fs::write(&txt, b"text").unwrap();
        let mut open = Picker::custom("Import photo".into(), Selection::OpenOne, vec!["png".into()], None);
        open.enter(dir.clone());
        assert!(open.entries.iter().any(|entry| entry.path == png));
        assert!(!open.entries.iter().any(|entry| entry.path == txt));
        open.location = png.to_string_lossy().into_owned();
        assert_eq!(open.choose().unwrap(), vec![png.clone()]);
        open.location = txt.to_string_lossy().into_owned();
        assert!(open.choose().is_err(), "typed unsupported extension must be rejected");

        let mut folder = Picker::custom("Export folder".into(), Selection::Folder, vec![], None);
        folder.enter(dir.clone());
        assert_eq!(folder.choose().unwrap(), vec![dir.clone()]);
        folder.location = dir.join("does-not-exist").to_string_lossy().into_owned();
        assert!(folder.choose().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_picker_normalizes_extension_before_overwrite_confirmation() {
        let dir = std::env::temp_dir().join(format!("sumi-save-picker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("painting.sumi");
        std::fs::write(&existing, b"do not overwrite this in a file picker test").unwrap();
        let mut save = Picker::custom("Save Sumi".into(), Selection::Save, vec!["sumi".into()], None);
        save.enter(dir.clone());
        save.location = dir.join("painting.png").to_string_lossy().into_owned();
        assert_eq!(save.choose().unwrap(), vec![existing.clone()]);
        assert!(existing.exists(), "selecting a destination does not write it");
        save.location = dir.join("missing-parent").join("painting.sumi").to_string_lossy().into_owned();
        assert!(save.choose().is_err(), "an invalid parent must be rejected before export");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_picker_selects_an_actual_image_for_import_and_photo() {
        let dir = std::env::temp_dir().join(format!("sumi-gallery-picker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("sample.PNG");
        image::RgbaImage::from_pixel(12, 8, image::Rgba([12, 34, 56, 255])).save(&image).unwrap();
        for kind in [Kind::Import, Kind::Artwork, Kind::Photo, Kind::Reference, Kind::Insert, Kind::InsertPhoto] {
            let mut picker = Picker::new(kind);
            picker.enter(dir.clone());
            assert_eq!(picker.entries.iter().filter(|entry| !entry.directory).count(), 1);
            picker.location = image.to_string_lossy().into_owned();
            let selected = picker.choose().unwrap();
            assert_eq!(selected, vec![image.clone()]);
            let canvas = if matches!(kind, Kind::Photo | Kind::Reference | Kind::Insert | Kind::InsertPhoto) {
                crate::files::picture_artwork(&selected[0]).unwrap()
            } else {
                crate::files::open_artwork_files(selected).unwrap().0
            };
            assert_eq!((canvas.info().width, canvas.info().height), (12, 8));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn insert_file_and_insert_photo_retain_existing_video_support() {
        let folder = std::env::temp_dir().join(format!("sumi-picker-video-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let video = folder.join("short.mp4");
        std::fs::write(&video, b"file picker type test").unwrap();
        for kind in [Kind::Insert, Kind::InsertPhoto] {
            let mut picker = Picker::new(kind);
            picker.enter(folder.clone());
            assert!(picker.entries.iter().any(|entry| entry.path == video));
            picker.location = video.to_string_lossy().into_owned();
            assert_eq!(picker.choose().unwrap(), vec![video.clone()]);
        }
        let mut gallery_photo = Picker::new(Kind::Photo);
        gallery_photo.enter(folder.clone());
        assert!(gallery_photo.entries.iter().all(|entry| entry.path != video));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn large_directory_can_find_later_files_without_arbitrary_truncation() {
        let folder = std::env::temp_dir().join(format!("sumi-gallery-picker-many-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        for n in 0..2052 {
            std::fs::write(folder.join(format!("image-{n:04}.png")), b"test").unwrap();
        }
        let mut picker = Picker::new(Kind::Photo);
        picker.enter(folder.clone());
        assert_eq!(picker.entries.len(), 2052, "no file is arbitrarily lost before search");
        picker.filter = "2051".into();
        assert_eq!(picker.entries.iter().filter(|e| e.name.contains(&picker.filter)).count(), 1);
        let file = folder.join("image-2051.png");
        picker.location = file.to_string_lossy().into_owned();
        assert_eq!(picker.choose().unwrap(), vec![file]);
        std::fs::remove_dir_all(folder).unwrap();
    }
}
