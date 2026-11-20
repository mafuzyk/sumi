//! Artworks on disk: new, open (Sumi packages, PSDs and PNGs), save as .sumi, export,
//! and asking before unsaved changes are lost.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rfd::{FileDialog, MessageDialog, MessageLevel};
use sumi_core::{
    CanvasPreset, CanvasSession, ClipOutcome, KeptGestures, ModelFile, PagesOutcome, PsdOutcome, ThumbnailImage,
    export_file_name, import_clip, import_pages, import_psd, preset_height, preset_width,
};

use crate::ui::Interface;

/// What an artwork is shared as (the Actions panel's Share tab): written to a file the user picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// The layered package, as .sumi or with the other app's extension.
    Sumi,
    Package,
    Psd,
    /// CLIP STUDIO PAINT's own document, its layers and masks kept.
    Clip,
    /// Krita's own document, its layers and masks kept.
    Kra,
    Pdf,
    Png,
    Jpeg,
    Tiff,
    /// Each layer a page, or a PNG file in a folder.
    LayersPdf,
    LayerPngs,
    /// The current and picked layers, each a PNG file in a folder.
    ChosenLayerPngs,
    /// A model: as an OBJ zipped with its material file and textures, or as a USDZ; as shown, as
    /// a PNG, a JPEG or a TIFF; its texture sets, a PNG each in a folder.
    ModelObj,
    ModelUsdz,
    ModelRender,
    ModelJpeg,
    ModelTiff,
    ModelTextures,
    ModelArtwork,
    /// A plugin's artwork, its shapes kept.
    Svg,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Sumi | Format::ModelArtwork => "Sumi (.sumi)",
            Format::Package => "Procreate",
            Format::Psd => "PSD",
            Format::Clip => "CLIP STUDIO (.clip)",
            Format::Kra => "Krita (.kra)",
            Format::Pdf | Format::LayersPdf => "PDF",
            Format::Png | Format::LayerPngs | Format::ChosenLayerPngs | Format::ModelRender | Format::ModelTextures => {
                "PNG"
            }
            Format::Tiff | Format::ModelTiff => "TIFF",
            Format::Jpeg | Format::ModelJpeg => "JPEG",
            Format::ModelObj => "OBJ",
            Format::ModelUsdz => "USDZ",
            Format::Svg => "SVG",
        }
    }

    /// A layered package: its file's name has underscores for spaces.
    fn package(self) -> bool {
        matches!(self, Format::Sumi | Format::Package | Format::ModelArtwork)
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Sumi | Format::ModelArtwork => "sumi",
            Format::Package => "procreate",
            Format::Psd => "psd",
            Format::Clip => "clip",
            Format::Kra => "kra",
            Format::Pdf | Format::LayersPdf => "pdf",
            Format::Png | Format::LayerPngs | Format::ChosenLayerPngs | Format::ModelRender | Format::ModelTextures => {
                "png"
            }
            Format::Tiff | Format::ModelTiff => "tiff",
            Format::Jpeg | Format::ModelJpeg => "jpg",
            Format::ModelObj => "zip",
            Format::Svg => "svg",
            Format::ModelUsdz => "usdz",
        }
    }
}

/// Where the app keeps what it keeps between runs (palettes and the like): the platform's place
/// for an app's data.
pub fn app_dir() -> PathBuf {
    app_dir_named(if cfg!(any(windows, target_os = "macos")) { "Sumi" } else { "sumi" })
}

/// What writing the artwork as a native document (`.clip` or `.kra`) would do, as Share's rows
/// say it before the file is picked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClipChanges {
    /// Every line the writer would say, in its order: none when the format holds the artwork.
    Kept(Vec<String>),
    /// The first thing the format cannot hold: the write would be refused instead.
    Refused(String),
}

impl ClipChanges {
    /// What the core's pre-flight answered: its warnings, or the refusal it stopped on.
    pub(crate) fn of(preflight: Result<Vec<String>, sumi_core::MobileError>) -> ClipChanges {
        match preflight {
            Ok(lines) => ClipChanges::Kept(lines),
            Err(sumi_core::MobileError::File { reason } | sumi_core::MobileError::Window { reason }) => {
                ClipChanges::Refused(reason)
            }
        }
    }

    /// The one line a row shows: `0 changes`, `N changes`, or what the format refuses.
    pub(crate) fn line(&self) -> String {
        match self {
            ClipChanges::Kept(lines) => match lines.len() {
                0 => "0 changes".to_owned(),
                1 => "1 change".to_owned(),
                count => format!("{count} changes"),
            },
            ClipChanges::Refused(refusal) => refusal.clone(),
        }
    }

    /// The whole of what the line stands for, for the row's tooltip.
    pub(crate) fn detail(&self) -> String {
        match self {
            ClipChanges::Kept(lines) if lines.is_empty() => {
                "Nothing the artwork holds is lost in this format.".to_owned()
            }
            ClipChanges::Kept(lines) => lines.join("\n"),
            ClipChanges::Refused(refusal) => refusal.clone(),
        }
    }
}

/// The folder the app used before it was named Sumi.
fn legacy_app_dir() -> PathBuf {
    app_dir_named(if cfg!(any(windows, target_os = "macos")) {
        sumi_core::legacy::APP_DIR_DESKTOP
    } else {
        sumi_core::legacy::APP_DIR_UNIX
    })
}

fn app_dir_named(name: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join(name)
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support").join(name)
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join(name)
    }
}

/// Moves the old app folder to the Sumi one, once, before anything opens either. A Sumi folder
/// that already exists is left alone: two histories are never merged blindly.
pub fn migrate_app_dir() {
    migrate_dir(&legacy_app_dir(), &app_dir());
}

fn migrate_dir(old: &Path, new: &Path) {
    if new.exists() || !old.exists() {
        return;
    }
    if let Err(e) = std::fs::rename(old, new) {
        // across filesystems (or with a missing parent) a rename cannot work: copy the history
        // over instead of leaving it behind, saying why either way
        log::error!("Could not move the old app folder ({e}); copying it instead");
        if let Err(e) = copy_dir(old, new) {
            log::error!("Could not copy the old app folder either: {e}");
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(old) {
            log::error!("The old app folder was copied but could not be removed: {e}");
        }
    }
}

/// Copies a folder and everything under it to `to`, which must not exist.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Shows a passing message with an OK button.
pub fn say(text: &str) {
    MessageDialog::new().set_level(MessageLevel::Info).set_title("Sumi").set_description(text).show();
}

/// Whether the file at `path` holds brushes, by its name (a zip by what it holds): ours (a brush,
/// a set or a library) or another app's.
pub fn is_brush_file(path: &Path) -> bool {
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    ["brush", "brushset", "brushlibrary"].contains(&extension.as_str())
        || sumi_core::OTHER_BRUSH_EXTENSIONS.contains(&extension.as_str())
        // a brush or a set sent on as a plain zip (told by its names, not read whole)
        || (extension == "zip"
            && sumi_core::zip_file_kind(path.to_string_lossy().into_owned()) == sumi_core::ZipKind::Brushes)
}

/// Where brush files given while no canvas is open wait for one.
pub fn pending_brushes_dir() -> PathBuf {
    app_dir().join("pending-brushes")
}

/// Brush files given while no canvas is open, kept (under their own names, after a number that
/// keeps their order) until one opens; says so. False if none could be kept.
pub fn keep_pending_brushes(paths: &[PathBuf]) -> bool {
    let dir = pending_brushes_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut kept = false;
    for (i, path) in paths.iter().enumerate() {
        let Some(name) = path.file_name() else { continue };
        let to = dir.join(format!("{stamp}-{i}-{}", name.to_string_lossy()));
        kept |= std::fs::copy(path, to).is_ok();
    }
    if kept {
        say("Brushes will be added to the library when a canvas opens.");
    }
    kept
}

/// The brush files kept while no canvas was open, oldest first, each with its own name; taken
/// out of the folder.
pub fn take_pending_brushes() -> Vec<(Vec<u8>, String)> {
    let Ok(read) = std::fs::read_dir(pending_brushes_dir()) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let bytes = std::fs::read(&path).ok();
            let _ = std::fs::remove_file(&path);
            let kept = path.file_name()?.to_string_lossy().into_owned();
            // the name it came with, after the stamp and the number
            let name = kept.splitn(3, '-').nth(2).unwrap_or(&kept).to_owned();
            Some((bytes?, name))
        })
        .collect()
}

/// Shows a message with an OK button; it reaches the log and the terminal as well, for a system
/// with no way of showing it.
pub fn tell(title: &str, text: &str) {
    log::error!("{title}: {text}");
    eprintln!("{title}: {text}");
    if cfg!(target_os = "linux") && !has_program("zenity") {
        // what shows the app's dialogs on Linux isn't there: the first that is of the others
        let shown = other_dialogs(title, text).iter().any(|(program, args)| {
            std::process::Command::new(program).args(args).status().is_ok_and(|done| done.success())
        });
        if shown {
            return;
        }
    }
    MessageDialog::new().set_level(MessageLevel::Error).set_title(title).set_description(text).show();
}

/// Whether `program` can be run here. Asked of the same program on every error dialog otherwise,
/// so the answer is kept for the life of the process (what is installed does not change meantime).
fn has_program(program: &str) -> bool {
    static FOUND: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        std::sync::OnceLock::new();
    let found = FOUND.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(&known) = found.lock().unwrap_or_else(|e| e.into_inner()).get(program) {
        return known;
    }
    let can_run = std::process::Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    _ = found.lock().unwrap_or_else(|e| e.into_inner()).insert(program.to_owned(), can_run);
    can_run
}

/// The programs that can show a message where the app's own dialogs can't, in the order to try
/// them, each with its arguments.
fn other_dialogs(title: &str, text: &str) -> [(&'static str, Vec<String>); 3] {
    let (title, text) = (title.to_owned(), text.to_owned());
    [
        ("kdialog", vec!["--title".to_owned(), title.clone(), "--error".to_owned(), text.clone()]),
        ("xmessage", vec!["-center".to_owned(), format!("{title}\n\n{text}")]),
        ("notify-send", vec!["--urgency=critical".to_owned(), title, text]),
    ]
}

/// Files being written on threads of their own (exports, shares, Save As): since when, whether
/// from the artwork open (which takes no input meanwhile, so nothing the export would end is
/// started), and the thread, waited for before the app ends so none is cut short. And what they
/// had to say, shown on the window's thread (some systems show dialogs only from there).
type Writing = (std::time::Instant, bool, std::thread::JoinHandle<()>);
static WRITING: std::sync::Mutex<Vec<Writing>> = std::sync::Mutex::new(Vec::new());
static TOLD: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
static NOTES: std::sync::Mutex<std::collections::VecDeque<(String, String)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
static NOTE_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Runs `write` on a thread of its own, among those the app waits for as it ends; `from_canvas`
/// when it writes the artwork open.
pub fn write_meanwhile(from_canvas: bool, write: impl FnOnce() + Send + 'static) {
    let started = std::thread::spawn(write);
    let mut writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    writing.retain(|(.., w)| !w.is_finished());
    writing.push((std::time::Instant::now(), from_canvas, started));
}

/// Whether a file is still being written.
pub fn still_writing() -> bool {
    WRITING.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(.., w)| !w.is_finished())
}

/// Since when the artwork open has been written, if it still is.
pub fn exporting_since() -> Option<std::time::Instant> {
    let writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    writing.iter().filter(|(_, from_canvas, w)| *from_canvas && !w.is_finished()).map(|(since, ..)| *since).min()
}

/// Waits for the files still being written.
pub fn finish_writing() {
    let writing = std::mem::take(&mut *WRITING.lock().unwrap_or_else(|e| e.into_inner()));
    for (.., w) in writing {
        let _ = w.join();
    }
}

/// A message from a thread of its own (writing, reading), to show on the window's
/// ([`tell_what_was_told`]).
pub fn tell_later(title: &str, text: &str) {
    TOLD.lock().unwrap_or_else(|e| e.into_inner()).push((title.to_owned(), text.to_owned()));
}

/// Non-fatal import/export change notes, queued for the app's own in-window alert instead of a
/// blocking OS error dialog.
pub fn note_later(title: &str, text: &str) {
    log::info!("{title}: {text}");
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).push_back((title.to_owned(), text.to_owned()));
    NOTE_READY.store(true, std::sync::atomic::Ordering::Release);
}

/// Consumes whether a new change note was queued since the last wakeup check.
pub fn take_notes_wakeup() -> bool {
    NOTE_READY.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Pops the oldest queued change note, if any.
pub fn take_next_note() -> Option<(String, String)> {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
}

/// Logs any unshown change notes when closing cleanly, without spawning blocking dialogs.
pub fn flush_notes_on_exit() {
    NOTE_READY.store(false, std::sync::atomic::Ordering::Release);
    let notes = std::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()));
    for (title, text) in notes {
        log::info!("{title}: {text}");
    }
}

/// Keeps `session`'s brushes in their folder. One that can't (the disk full, say) is told once,
/// from the window's thread, and not again until they are kept once more.
pub fn keep_brushes(session: &sumi_core::CanvasSession) {
    static FAILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if session.keep_brushes(brushes_dir()) {
        FAILING.store(false, std::sync::atomic::Ordering::Relaxed);
    } else if !FAILING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tell_later(
            "Can't keep the brushes",
            "The brush library couldn't be kept. The disk may be full: brushes brought in or changed \
             since are lost when Sumi closes, unless some room is made.",
        );
    }
}

/// Shows the messages the writing threads left.
pub fn tell_what_was_told() {
    let told = std::mem::take(&mut *TOLD.lock().unwrap_or_else(|e| e.into_inner()));
    for (title, text) in told {
        tell(&title, &text);
    }
}

/// Asks where to keep a file called `name` (ending `extension`, a `kind` of file) and writes
/// `bytes` there.
pub fn save_file(bytes: &[u8], name: &str, extension: &str, kind: &str) {
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " _-".contains(c) { c } else { '_' }).collect();
    let safe = if safe.trim().is_empty() { "Brush".to_owned() } else { safe };
    let Some(path) = FileDialog::new()
        .set_title(format!("Share {kind}"))
        .add_filter(kind, &[extension])
        .set_file_name(format!("{safe}.{extension}"))
        .save_file()
    else {
        return;
    };
    // a file there already is replaced only once this one is written whole
    if let Err(e) = write_whole(&path, bytes) {
        tell("Can't share", &e.to_string());
    }
}

/// A sound the user picks (titled `title`): a WAV file, read whole, with its name.
pub fn pick_sound(title: &str) -> Option<(Vec<u8>, String)> {
    let path = FileDialog::new().set_title(title).add_filter("Sounds", &["wav", "wave"]).pick_file()?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match std::fs::read(&path) {
        Ok(bytes) => Some((bytes, name)),
        Err(_) => {
            say("This file could not be read.");
            None
        }
    }
}

/// A file the user picks (titled `title`), read whole, with its name.
pub fn pick_file(title: &str) -> Option<(Vec<u8>, String)> {
    let path = FileDialog::new().set_title(title).pick_file()?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match std::fs::read(&path) {
        Ok(bytes) => Some((bytes, name)),
        Err(_) => {
            say("This file could not be read.");
            None
        }
    }
}

/// Says the canvas holds no more layers, and how many it holds.
pub fn no_room(session: &CanvasSession) {
    let most = session.layer_stats().maximum;
    tell("No room", &format!("This canvas holds at most {most} layers."));
}

/// Where the brush library, the brushes' choices and their sliders are kept.
pub fn brushes_dir() -> String {
    app_dir().join("brushes").to_string_lossy().into_owned()
}

/// The gesture settings kept: a line for each switch (`EYEDROPPER.HOLD true`) and each page's
/// delay (`EYEDROPPER.delay 0.4`).
pub fn kept_gestures() -> KeptGestures {
    let mut kept = KeptGestures::default();
    let text = std::fs::read_to_string(app_dir().join("gestures")).unwrap_or_default();
    for line in text.lines() {
        let Some((name, value)) = line.split_once(' ') else { continue };
        if name.ends_with(".delay") {
            if let Ok(v) = value.parse() {
                kept.delays.insert(name.to_owned(), v);
            }
        } else if let Ok(v) = value.parse() {
            kept.switches.insert(name.to_owned(), v);
        }
    }
    kept
}

pub fn keep_gestures(kept: &KeptGestures) {
    let mut lines: Vec<String> = kept.switches.iter().map(|(name, on)| format!("{name} {on}")).collect();
    lines.extend(kept.delays.iter().map(|(name, s)| format!("{name} {s}")));
    lines.sort();
    keep_text("gestures", &(lines.join("\n") + "\n"));
}

/// The brush colour and the one masks paint with as they were last left (hue, saturation and
/// brightness, 0..1), if they were kept.
pub fn kept_colours() -> Option<([f32; 3], [f32; 3])> {
    let text = std::fs::read_to_string(app_dir().join("colour")).ok()?;
    let v: Vec<f32> = text
        .split_whitespace()
        .filter_map(|t| t.parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .collect();
    match v[..] {
        [h, s, b, mh, ms, mb] => Some(([h, s, b], [mh, ms, mb])),
        [h, s, b] => Some(([h, s, b], [0.0; 3])),
        _ => None,
    }
}

pub fn keep_colours(brush: [f32; 3], mask: [f32; 3]) {
    let [h, s, b] = brush;
    let [mh, ms, mb] = mask;
    keep_text("colour", &format!("{h} {s} {b} {mh} {ms} {mb}"));
}

/// The QuickMenu's buttons kept, as the core writes them.
pub fn kept_quick_menu() -> Option<String> {
    std::fs::read_to_string(app_dir().join("quick_menu")).ok().map(|s| s.trim().to_owned())
}

/// The QuickMenu's menus and which is active, as the core writes them (with none, the one kept
/// before there were several, by [`kept_quick_menu`]).
pub fn kept_quick_menus() -> (Option<String>, usize) {
    let menus = std::fs::read_to_string(app_dir().join("quick_menus")).ok();
    let active = std::fs::read_to_string(app_dir().join("quick_menu_active")).ok();
    (menus, active.and_then(|s| s.trim().parse().ok()).unwrap_or(0))
}

pub fn keep_quick_menus(saved: &str, active: usize) {
    keep_text("quick_menus", saved);
    keep_text("quick_menu_active", &active.to_string());
}

/// A small file of the app's settings written (a failure only logged: the setting holds this run).
pub fn keep_text(name: &str, text: &str) {
    let dir = app_dir();
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| write_whole(&dir.join(name), text.as_bytes())) {
        log::warn!("keeping {name}: {e}");
    }
}

/// Writes `bytes` to the app's file `path` whole or not at all: into a file beside it, then
/// renamed over it, so a write cut short leaves the last one.
pub fn write_whole(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let next = path.with_extension("next");
    std::fs::write(&next, bytes)?;
    std::fs::rename(&next, path)
}

/// A new artwork as `preset` says: its size, DPI, background and colour profile.
pub fn new_from_preset(preset: &CanvasPreset) -> Arc<CanvasSession> {
    let rgb = [16, 8, 0].map(|shift| ((preset.background >> shift) & 0xFF) as f32 / 255.0).to_vec();
    let (w, h) = (preset_width(preset.clone()), preset_height(preset.clone()));
    let session = CanvasSession::new_custom(w, h, preset.dpi, rgb, !preset.background_hidden, String::new());
    session.set_new_profile(preset.profile.clone());
    session
}

/// The time-lapse settings `preset` gives a new artwork, kept in its folder `dir` before it opens.
pub fn keep_preset_time_lapse(preset: &CanvasPreset, dir: &str) {
    sumi_core::TimeLapse::new(dir.to_owned()).set_settings(sumi_core::TimeLapseSettings {
        resolution: preset.time_lapse_resolution,
        quality: preset.time_lapse_quality,
        hevc: preset.time_lapse_hevc,
    });
}

/// A colour profile the user picks (an .icc file), added to those a canvas can be in; its name, or
/// none (said so) if it isn't one.
pub fn import_profile() -> Option<String> {
    let path = FileDialog::new().set_title("Import").add_filter("Color profiles", &["icc", "icm"]).pick_file()?;
    let name = std::fs::read(path).ok().and_then(sumi_core::import_profile);
    if name.is_none() {
        tell("Can't import", "This file isn't a color profile.");
    }
    name
}

/// Whether the file at `path` is a plugin, by its name.
pub fn is_plugin_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("sumi-plugin") || sumi_core::legacy::is_legacy_plugin_extension(&e.to_string_lossy())
    })
}

const PLUGIN_FILTER_LABEL: &str = "Sumi plugins";
const ARTWORK_FILTER_LABEL: &str = "Sumi artwork";

/// A plugin file the user picks.
pub fn pick_plugin() -> Option<PathBuf> {
    FileDialog::new()
        .set_title("Import")
        .add_filter(
            PLUGIN_FILTER_LABEL,
            &["sumi-plugin", sumi_core::legacy::PLUGIN_EXTENSIONS[0], sumi_core::legacy::PLUGIN_EXTENSIONS[1]],
        )
        .pick_file()
}

/// A picture the user picks (PNG, JPEG, WebP, GIF, BMP or TIFF), to make a new artwork of
/// ([`picture_artwork`]).
pub fn pick_picture() -> Option<PathBuf> {
    FileDialog::new()
        .set_title("Photo")
        .add_filter("Pictures", &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"])
        .pick_file()
}

/// The picture at `path` as a new one-layer artwork its size, named after the file; why not if
/// it can't be read.
pub fn picture_artwork(path: &Path) -> Result<Arc<CanvasSession>, String> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let picture = image::open(path).map_err(|e| e.to_string())?;
    let rgba = picture.to_rgba8();
    let image = ThumbnailImage { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() };
    Ok(CanvasSession::from_image(image, name))
}

/// What a model to paint on can be: an OBJ, a USD (packaged or not), or a zip holding one with
/// its files.
const MODEL_EXTENSIONS: [&str; 6] = ["obj", "usdz", "usdc", "usda", "usd", "zip"];

/// Whether `path` is a model's file (or a zip holding one), by its name.
pub fn is_model_file(path: &Path) -> bool {
    MODEL_EXTENSIONS.contains(&extension_of(path).as_str())
}

fn extension_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// What a palette's file can be: Swatches, Adobe Swatch Exchange and Photoshop's colour swatches.
const PALETTE_EXTENSIONS: [&str; 3] = ["swatches", "ase", "aco"];

/// Whether `path` is a palette's file, by its name.
pub fn is_palette_file(path: &Path) -> bool {
    PALETTE_EXTENSIONS.contains(&extension_of(path).as_str())
}

/// The files the gallery's Import brings in, as the user picks them: an artwork or a model (as
/// [`pick_artwork`] takes), palettes and brushes.
pub fn pick_import() -> Option<Vec<PathBuf>> {
    let brushes: Vec<&str> =
        ["brush", "brushset", "brushlibrary"].into_iter().chain(sumi_core::OTHER_BRUSH_EXTENSIONS).collect();
    let more: Vec<&str> = PALETTE_EXTENSIONS.into_iter().chain(brushes).collect();
    pick_artwork_files(&more)
}

/// Artworks, models and the files with extensions `more`, as the user picks them.
pub fn pick_artwork_files(more: &[&str]) -> Option<Vec<PathBuf>> {
    let artworks = [
        "sumi",
        sumi_core::legacy::ARTWORK_EXTENSION,
        "procreate",
        "psd",
        "psb",
        "clip",
        "kra",
        "pdf",
        "png",
        "obj",
        "usdz",
        "usdc",
        "usda",
        "usd",
        "zip",
        "mtl",
        "jpg",
        "jpeg",
        "webp",
        "gif",
        "bmp",
        "tif",
        "tiff",
    ];
    let all: Vec<&str> = artworks.iter().chain(more).copied().collect();
    FileDialog::new().set_title("Import").add_filter("Artworks and models", &all).pick_files()
}

/// The artwork in the files `paths` (picked, or dropped on the gallery): a layered package, a
/// PSD, a PDF, a picture as a one-layer artwork, or a model to paint on with the other files
/// beside it.
pub fn open_artwork_files(paths: Vec<PathBuf>) -> Option<(Arc<CanvasSession>, Option<PathBuf>)> {
    // a model among them takes the rest as the files picked with it
    if let Some(model) = paths.iter().find(|p| is_model_file(p)) {
        return open_model(model, &paths).map(|s| (s, None)).map_err(|e| tell_later("Can't open the model", &e)).ok();
    }
    let path = paths.into_iter().next()?;
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // a model's artwork opens from where it is unpacked, and is kept as any new artwork is
    if sumi_core::zip_file_kind(path.to_string_lossy().into_owned()) == sumi_core::ZipKind::ModelArtwork {
        let opened = open_model_artwork(&path).map(|s| (s, None));
        return opened.map_err(|e| tell_later("Can't open the model", &e)).ok();
    }
    let opened = match extension.as_str() {
        "psd" | "psb" => open_psd(&path, &name),
        "clip" | "kra" => open_clip(&path, &name),
        "pdf" => open_pdf(&path, &name),
        "png" => read_png(&path).map(|image| CanvasSession::from_image(image, name)),
        "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" => image::open(&path)
            .map(|picture| {
                let rgba = picture.to_rgba8();
                let image = ThumbnailImage { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() };
                CanvasSession::from_image(image, name)
            })
            .map_err(|e| e.to_string()),
        _ => CanvasSession::open_package(path.to_string_lossy().into_owned()).map_err(|e| e.to_string()),
    };
    // a package brings its time-lapse too
    let package =
        (extension == "sumi" || sumi_core::legacy::is_legacy_artwork_extension(&extension) || extension == "procreate")
            .then_some(path);
    opened.map(|s| (s, package)).map_err(|e| tell_later("Can't open the file", &e)).ok()
}

/// The model's artwork in file `path`, unpacked beside the other things opened from files.
fn open_model_artwork(path: &Path) -> Result<Arc<CanvasSession>, String> {
    let work = std::env::temp_dir().join(format!("sumi-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let dir = work.to_string_lossy().into_owned();
    if !sumi_core::unpack_model_artwork(path.to_string_lossy().into_owned(), dir.clone()) {
        return Err("This file can't be opened.".to_owned());
    }
    let opened = CanvasSession::open(dir).map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&work);
    opened
}

/// The PDF in file `path` as a new artwork named `name`: each page a layer ("Page 1" at the
/// bottom), drawn on white at 300 dpi and fitted to the first page's size, with Page Assist on.
fn open_pdf(path: &Path, name: &str) -> Result<Arc<CanvasSession>, String> {
    let work = std::env::temp_dir().join(format!("sumi-pdf-{}", std::process::id()));
    let (pages, artwork) = (work.join("pages"), work.join("artwork"));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&pages).map_err(|e| e.to_string())?;
    let unreadable = || "This PDF can't be opened.".to_owned();
    let opened = crate::pdf::draw_pages(path, &pages)
        .map_err(|e| {
            log::error!("drawing a PDF's pages: {e}");
            if e.to_string() == crate::pdf::NO_POPPLER { crate::pdf::NO_POPPLER.to_owned() } else { unreadable() }
        })
        .and_then(|(count, w, h)| {
            let dir = artwork.to_string_lossy().into_owned();
            match import_pages(
                pages.to_string_lossy().into_owned(),
                count,
                w,
                h,
                crate::pdf::DPI,
                dir.clone(),
                name.to_owned(),
            ) {
                PagesOutcome::Imported => CanvasSession::open(dir).map_err(|e| e.to_string()),
                PagesOutcome::TooManyPages { most } => {
                    Err(format!("This PDF has more pages than a canvas its size can hold as layers ({most})."))
                }
                PagesOutcome::Unreadable => Err(unreadable()),
            }
        });
    // the session holds what it read
    let _ = std::fs::remove_dir_all(&work);
    opened
}

/// The model in file `path` to paint on, with the other files `picked` along with it; its own
/// textures in its texture sets.
fn open_model(path: &Path, picked: &[PathBuf]) -> Result<Arc<CanvasSession>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let files = picked
        .iter()
        .filter(|p| p.as_path() != path)
        .filter_map(|p| std::fs::read(p).ok().map(|bytes| ModelFile { name: file_name(p), bytes }))
        .collect();
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|n| !n.trim().is_empty());
    let name = name.unwrap_or_else(|| "Untitled Artwork".to_owned());
    let session = CanvasSession::open_model(bytes, file_name(path), files, name, 0).map_err(|e| e.to_string())?;
    session.load_model_textures();
    Ok(session)
}

/// A PSD made an artwork in a folder of its own, then opened from there.
fn open_psd(path: &Path, name: &str) -> Result<Arc<CanvasSession>, String> {
    let dir = std::env::temp_dir().join(format!("sumi-psd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let outcome = import_psd(path.to_string_lossy().into_owned(), dir.to_string_lossy().into_owned(), name.to_owned());
    let opened = match outcome {
        PsdOutcome::Imported => CanvasSession::open(dir.to_string_lossy().into_owned()).map_err(|e| e.to_string()),
        PsdOutcome::TooManyLayers { most } => Err(format!("It has more layers than a canvas its size holds ({most}).")),
        PsdOutcome::Unsupported => {
            Err("This kind of PSD isn't read (bitmap, indexed, Lab or 32 bits a channel).".into())
        }
        PsdOutcome::Unreadable => Err("It isn't a PSD, or it's damaged.".into()),
    };
    // the opened artwork holds its layers' files by now
    let _ = std::fs::remove_dir_all(&dir);
    opened
}

/// A `.clip` or `.kra` document made a native artwork in a folder of its own, then opened from
/// there; the original file is only read.
fn open_clip(path: &Path, name: &str) -> Result<Arc<CanvasSession>, String> {
    let dir = std::env::temp_dir().join(format!("sumi-clip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let outcome = import_clip(path.to_string_lossy().into_owned(), dir.to_string_lossy().into_owned(), name.to_owned());
    let opened = match outcome {
        ClipOutcome::Imported { warnings } => {
            if !warnings.is_empty() {
                note_later("Imported with changes", &warnings.join("\n"));
            }
            CanvasSession::open(dir.to_string_lossy().into_owned()).map_err(|e| e.to_string())
        }
        ClipOutcome::TooLarge { most } => {
            Err(format!("It is larger than a canvas may be here ({most} pixels an edge)."))
        }
        ClipOutcome::TooManyLayers { most } => {
            Err(format!("It has more layers than a canvas its size holds ({most})."))
        }
        ClipOutcome::Unsupported(why) => Err(format!("This document isn't read yet: {why}.")),
        ClipOutcome::Unreadable(why) => Err(format!("It isn't a .clip or .kra document, or it's damaged ({why}).")),
        ClipOutcome::FailedToSave(why) => Err(format!("It was read but couldn't be made an artwork here ({why}).")),
    };
    // the opened artwork holds its layers' files by now
    let _ = std::fs::remove_dir_all(&dir);
    opened
}

/// A picture's pixels (in `bytes`: a PNG, JPEG, WebP, GIF, BMP or TIFF) as 0xAARRGGBB, for its
/// colours; none if it isn't one.
pub fn picture_pixels(bytes: &[u8]) -> Option<Vec<u32>> {
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let argb = |&[r, g, b, a]: &[u8; 4]| u32::from(a) << 24 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
    Some(rgba.as_raw().as_chunks::<4>().0.iter().map(argb).collect())
}

/// A PNG's size and pixels (in `bytes`), straight RGBA; none if it isn't one.
pub fn png_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let data = buf.get(..frame.buffer_size())?;
    let rgba = match frame.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
        _ => return None,
    };
    Some((frame.width, frame.height, rgba))
}

/// A PNG's pixels, straight RGBA.
pub fn read_png(path: &Path) -> Result<ThumbnailImage, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("too large")?];
    let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let bytes = buf.get(..frame.buffer_size()).ok_or("short image")?;
    let rgba = match frame.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::GrayscaleAlpha => bytes.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
        other => return Err(format!("PNG colours not read: {other:?}")),
    };
    Ok(ThumbnailImage { width: frame.width, height: frame.height, rgba })
}

fn about_title() -> &'static str {
    "Sumi"
}

fn about_description() -> String {
    format!("Version {}\n\nBuilt-in brushes by Sumi.", env!("SUMI_VERSION"))
}

/// Shows the app's name, version and attribution.
pub fn about() {
    MessageDialog::new()
        .set_level(MessageLevel::Info)
        .set_title(about_title())
        .set_description(about_description())
        .show();
}

/// Saves the artwork as a .sumi package, where it was last saved unless `ask` (or it never
/// was): written on a thread of its own (a large one takes seconds), the interface told when it
/// is ([`Interface::saved_as`]).
pub fn save(session: &Arc<CanvasSession>, ui: &mut Interface, ask: bool) {
    let path = match ui.path.clone().filter(|_| !ask) {
        Some(p) => p,
        None => {
            let Some(p) = FileDialog::new()
                .set_title("Save")
                .add_filter(ARTWORK_FILTER_LABEL, &["sumi"])
                .set_file_name(format!("{}.sumi", session.name()))
                .save_file()
            else {
                return;
            };
            with_extension(p, "sumi")
        }
    };
    let edits = session.edits();
    let time_lapse = ui.artwork_dir.as_deref().map(crate::timelapse::for_package);
    let (session, (done, saved)) = (Arc::clone(session), std::sync::mpsc::channel());
    write_meanwhile(true, move || {
        let written = session.export_package(path.to_string_lossy().into_owned(), seed(), time_lapse);
        let _ = done.send(written.map(|()| (path, edits)).map_err(|e| e.to_string()));
    });
    ui.saving = Some(saved);
}

/// Writes the artwork as `format` where the user picks, its card showing how far it has come;
/// cancelled, nothing is left there.
pub fn export(
    session: &Arc<CanvasSession>,
    format: Format,
    dir: Option<&str>,
    pdf_quality: u8,
    card: &mut crate::export_card::ExportCard,
) {
    if card.busy() {
        return;
    }
    // each layer (or a model's texture set) a PNG file, in a folder the user picks
    if matches!(format, Format::LayerPngs | Format::ChosenLayerPngs | Format::ModelTextures) {
        let title =
            if format == Format::ModelTextures { "Share textures as PNG files" } else { "Share layers as PNG files" };
        let Some(dir) = FileDialog::new().set_title(title).pick_folder() else { return };
        let dir = dir.to_string_lossy().into_owned();
        writing(session, card, "Can't share", move |session| {
            // every picture goes into a folder of the export's own first: cancelling takes only
            // that, and a file already in the picked folder is never written over
            let staging = FolderStaging::claim(Path::new(&dir)).map_err(|e| e.to_string())?;
            let at = staging.path.to_string_lossy().into_owned();
            match format {
                Format::LayerPngs => session.export_layer_pngs(at),
                Format::ModelTextures => session.export_model_textures(at),
                _ => session.export_chosen_pngs(at),
            }
            .map_err(|e| e.to_string())?;
            Ok((Delivery::Folder(staging), Vec::new()))
        });
        return;
    }
    let Some(path) = FileDialog::new()
        .set_title(format!("Export {}", format.name()))
        .add_filter(format.name(), &[format.extension()])
        .set_file_name(format!("{}.{}", export_file_name(session.name(), format.package()), format.extension()))
        .save_file()
    else {
        return;
    };
    let path = with_extension(path, format.extension());
    let artwork = dir.map(str::to_owned);
    let time_lapse = dir.map(crate::timelapse::for_package);
    writing(session, card, "Can't export", move |session| {
        // a file of the export's own beside the target first, so a cancelled export leaves what
        // was there as it was and never takes a `.part` another one left
        let staging = Staging::claim(&path).map_err(|e| e.to_string())?;
        let at = staging.path.to_string_lossy().into_owned();
        let warnings = match format {
            Format::ModelArtwork => session.export_model_artwork(artwork.unwrap_or_default(), at).map(|()| Vec::new()),
            Format::Sumi | Format::Package => session.export_package(at, seed(), time_lapse).map(|()| Vec::new()),
            Format::Psd => session.export_psd(at).map(|()| Vec::new()),
            Format::Clip => session.export_clip(at, false),
            Format::Kra => session.export_clip(at, true),
            Format::Pdf => session.export_pdf(at, false, pdf_quality).map(|()| Vec::new()),
            Format::LayersPdf => session.export_pdf(at, true, pdf_quality).map(|()| Vec::new()),
            Format::Png => session.export_png(at).map(|()| Vec::new()),
            Format::Tiff => session.export_tiff(at).map(|()| Vec::new()),
            Format::Jpeg => session.export_jpeg(at).map(|()| Vec::new()),
            Format::ModelJpeg => session.export_model_jpeg(at).map(|()| Vec::new()),
            Format::ModelObj => session.export_model_obj(at).map(|()| Vec::new()),
            Format::ModelUsdz => session.export_model_usdz(at).map(|()| Vec::new()),
            Format::ModelRender => session.export_model_render(at, false).map(|()| Vec::new()),
            Format::ModelTiff => session.export_model_tiff(at).map(|()| Vec::new()),
            Format::Svg => session.export_svg(at).map(|()| Vec::new()),
            Format::LayerPngs | Format::ChosenLayerPngs | Format::ModelTextures => Ok(Vec::new()),
        }
        .map_err(|e| e.to_string())?;
        Ok((Delivery::File(staging), warnings))
    });
}

/// Writes the file plugin export `key` makes of the artwork's layers where the user picks, as
/// [`export`] writes the app's own.
pub fn export_plugin(session: &Arc<CanvasSession>, key: &str, card: &mut crate::export_card::ExportCard) {
    if card.busy() {
        return;
    }
    let Some(export) = session.plugin_exports().into_iter().find(|e| e.key == key) else { return };
    let extension = export.extension.clone();
    let Some(path) = FileDialog::new()
        .set_title(format!("Export {}", export.label))
        .add_filter(export.label.as_str(), &[extension.as_str()])
        .set_file_name(format!("{}.{extension}", export_file_name(session.name(), false)))
        .save_file()
    else {
        return;
    };
    let path = with_extension(path, &extension);
    let key = key.to_owned();
    writing(session, card, "Can't export", move |session| {
        // a file of the export's own beside the target first, as `export` writes it
        let staging = Staging::claim(&path).map_err(|e| e.to_string())?;
        session.export_plugin(key, staging.path.to_string_lossy().into_owned()).map_err(|e| e.to_string())?;
        Ok((Delivery::File(staging), Vec::new()))
    });
}

/// Writes an export on a thread of its own (a large one takes seconds, the window drawing on
/// meanwhile, as the tablet's does), `card` showing how far it has come; telling `failed` with the
/// reason if it can't. `write` returns what it made, where it is staged, and what the file kept
/// differently (told once it is in place): put in place once whole, or taken away if cancelled.
fn writing(
    session: &Arc<CanvasSession>,
    card: &mut crate::export_card::ExportCard,
    failed: &'static str,
    write: impl FnOnce(&CanvasSession) -> Result<(Delivery, Vec<String>), String> + Send + 'static,
) {
    session.reset_export_progress();
    let following = Arc::clone(session);
    let exporting = card.start(move || following.export_progress());
    let session = Arc::clone(session);
    write_meanwhile(true, move || {
        let mut guard = EndGuard::new(|made| exporting.ended(made));
        let made = write(&session)
            .and_then(|(made, warnings)| made.finish(exporting.cancelled()).map(|placed| (placed, warnings)));
        match made {
            Ok((placed, warnings)) => {
                guard.finish(placed);
                if placed && !warnings.is_empty() {
                    note_later("Exported with changes", &warnings.join("\n"));
                }
            }
            Err(e) => {
                guard.finish(false);
                if !exporting.cancelled() {
                    tell_later(failed, &e);
                }
            }
        }
    });
}

/// Ends an export on the way out if the work did not end it: a panic in the writing thread must not
/// leave the card up with `busy` refusing every later export. The panic still unwinds; it is only
/// reported here so it reaches the log as well as the thread's own message.
struct EndGuard<F: Fn(bool)> {
    on_end: F,
    done: bool,
}

impl<F: Fn(bool)> EndGuard<F> {
    fn new(on_end: F) -> Self {
        Self { on_end, done: false }
    }

    /// The work ended: `made` whether a file was made; disarms the guard.
    fn finish(&mut self, made: bool) {
        self.done = true;
        (self.on_end)(made);
    }
}

impl<F: Fn(bool)> Drop for EndGuard<F> {
    fn drop(&mut self) {
        if !self.done {
            log::error!("An export thread panicked before it ended; ending the run as not made");
            (self.on_end)(false);
        }
    }
}

/// A file an export writes into first, beside the one it will go to: a name of its own, claimed
/// with `create_new` so no file that is already there can be taken for it. Dropped unpublished
/// (cancelled, or failed), it is removed; publishing renames it over the target.
struct Staging {
    path: PathBuf,
    target: PathBuf,
}

impl Staging {
    /// Claims a file of its own beside `target` (in the working folder if it names none).
    fn claim(target: &Path) -> std::io::Result<Staging> {
        let mut n = 0;
        loop {
            let path = staging_path(target, n);
            // a target that happens to carry the staging's name does not become its own staging
            if path.as_path() == target {
                n += 1;
                continue;
            }
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Staging { path, target: target.to_path_buf() }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// Puts the file made in place, replacing what was there whole or not at all.
    fn publish(&self) -> std::io::Result<()> {
        std::fs::rename(&self.path, &self.target)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // publishing moved it away; cancelling or failing leaves only this to take. A target that
        // somehow had the staging's own name is never taken: publishing cannot move it apart.
        if self.path != self.target {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A staging file's name for try `n`: of its own, never the target's name with a suffix, so a
/// target at the filesystem's length limit still leaves room for it.
fn staging_path(target: &Path, n: u32) -> PathBuf {
    let folder = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    folder.join(format!(".sumi-export-{}-{n}.part", std::process::id()))
}

/// A folder an export writes into first, inside the one the user picked, its files moved into
/// that one only once every one is made. Cancelling removes this staging folder only, so nothing
/// that was in the picked folder is touched.
struct FolderStaging {
    path: PathBuf,
    to: PathBuf,
}

impl FolderStaging {
    /// Claims a folder of its own inside `to`, skipping one already there.
    fn claim(to: &Path) -> std::io::Result<FolderStaging> {
        let mut n = 0;
        loop {
            let path = folder_staging_path(to, n);
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(FolderStaging { path, to: to.to_path_buf() }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// Moves what it holds into the folder picked. A file already there is replaced; should a
    /// move fail partway, what was moved already stays (a folder's files are not put back as one
    /// step), and the share says it failed.
    fn publish(&self) -> Result<bool, String> {
        for entry in std::fs::read_dir(&self.path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            std::fs::rename(entry.path(), self.to.join(entry.file_name())).map_err(|e| e.to_string())?;
        }
        Ok(true)
    }
}

impl Drop for FolderStaging {
    fn drop(&mut self) {
        // publishing moved its files away; cancelling or failing leaves all of it here to take
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn folder_staging_path(to: &Path, n: u32) -> PathBuf {
    to.join(format!(".sumi-export-{}-{n}", std::process::id()))
}

/// What an export made, and how it goes where the user picked it: staged until whole.
enum Delivery {
    File(Staging),
    Folder(FolderStaging),
}

impl Delivery {
    /// Puts what was made in place, or leaves what was there as it was if cancelled. True when
    /// something was put in place.
    fn finish(self, cancelled: bool) -> Result<bool, String> {
        if cancelled {
            return Ok(false);
        }
        match self {
            Delivery::File(staging) => staging.publish().map(|()| true).map_err(|e| e.to_string()),
            Delivery::Folder(folder) => folder.publish(),
        }
    }
}

/// The file `made` copied to `to`, whole or not at all: into a staging file of the export's own
/// beside it first, then renamed over it. `made` may be `to` itself (the save dialog names the
/// file the export made): the copy is read before the target is replaced.
pub(crate) fn deliver_file(made: &Path, to: &Path) -> std::io::Result<()> {
    let staging = Staging::claim(to)?;
    std::fs::copy(made, &staging.path)?;
    staging.publish()
}

/// Artworks from the gallery written as `format`: one where the user picks, several into a folder
/// the user picks, each named by its name (a number after it when two share one). Each is opened
/// on a GPU device of its own, never shown. `card` shows them made, an artwork at a time;
/// cancelled, none is left where it was going.
pub fn share_artworks(
    list: &[sumi_core::GalleryArtwork],
    format: Format,
    pdf_quality: u8,
    card: &mut crate::export_card::ExportCard,
) {
    if card.busy() {
        return;
    }
    let extension = format.extension();
    let file_name = |name: &str| export_file_name(name.to_owned(), false);
    let targets: Vec<(sumi_core::GalleryArtwork, PathBuf)> = if let [one] = list {
        let Some(path) = FileDialog::new()
            .set_title(format!("Share {}", format.name()))
            .add_filter(format.name(), &[extension])
            .set_file_name(format!("{}.{extension}", file_name(&one.name)))
            .save_file()
        else {
            return;
        };
        vec![(one.clone(), with_extension(path, extension))]
    } else {
        let Some(dir) = FileDialog::new().set_title(format!("Share as {}", format.name())).pick_folder() else {
            return;
        };
        let mut used = std::collections::HashSet::new();
        list.iter()
            .map(|a| {
                let base = file_name(&a.name);
                let (mut name, mut n) = (base.clone(), 2);
                while !used.insert(name.clone()) {
                    name = format!("{base} {n}");
                    n += 1;
                }
                (a.clone(), dir.join(format!("{name}.{extension}")))
            })
            .collect()
    };
    // written on a thread of their own (each opened on a device of its own takes seconds), the
    // window drawing on meanwhile; each beside where it goes until all are made
    let done = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let following = Arc::clone(&done);
    let exporting = card.start(move || f32::from_bits(following.load(std::sync::atomic::Ordering::Relaxed)));
    let count = targets.len().max(1);
    write_meanwhile(false, move || {
        let mut guard = EndGuard::new(|made| exporting.ended(made));
        let mut staged: Vec<Staging> = Vec::new();
        // what the files made so far kept differently, told once they are in place
        let mut told: Vec<String> = Vec::new();
        let tell_kept_differently = |told: &[String]| {
            if !told.is_empty() {
                note_later("Exported with changes", &told.join("\n"));
            }
        };
        for (i, (a, path)) in targets.into_iter().enumerate() {
            if exporting.cancelled() {
                break;
            }
            let staging = match Staging::claim(&path) {
                Ok(staging) => staging,
                Err(e) => {
                    // those made before it are kept, as before
                    for made in &staged {
                        let _ = made.publish();
                    }
                    tell_kept_differently(&told);
                    guard.finish(false);
                    tell_later("Can't share", &e.to_string());
                    return;
                }
            };
            match write_artwork(&a, format, &staging.path.to_string_lossy(), pdf_quality) {
                Ok(warnings) => told.extend(warnings.into_iter().map(|w| format!("{}: {w}", a.name))),
                Err(e) => {
                    // those made before it are kept, as before; this one's staging goes with it
                    for made in &staged {
                        let _ = made.publish();
                    }
                    tell_kept_differently(&told);
                    guard.finish(false);
                    tell_later("Can't share", &format!("{}: {e}", a.name));
                    return;
                }
            }
            staged.push(staging);
            let progress = (i + 1) as f32 / count as f32;
            done.store(progress.to_bits(), std::sync::atomic::Ordering::Relaxed);
        }
        if exporting.cancelled() {
            guard.finish(false);
            // staged files are dropped and taken away; what was there is as it was
            return;
        }
        for made in &staged {
            if let Err(e) = made.publish() {
                guard.finish(false);
                tell_later("Can't share", &e.to_string());
                // a publish that fails partway leaves the files already moved, as a folder's
                // share does: what is left staged goes
                return;
            }
        }
        tell_kept_differently(&told);
        guard.finish(true);
    });
}

/// The artwork `a` from the gallery written as `format` at `path` (a PDF's pictures at
/// `pdf_quality`), opened on a GPU device of its own, never shown; returns what the file kept
/// differently, a line each (only `.clip` and `.kra` keep anything differently).
pub fn write_artwork(
    a: &sumi_core::GalleryArtwork,
    format: Format,
    path: &str,
    pdf_quality: u8,
) -> Result<Vec<String>, String> {
    let path = path.to_owned();
    let s = CanvasSession::open(a.dir.clone()).map_err(|e| e.to_string())?;
    if !s.prepare_offscreen() {
        return Err("There's no GPU device to draw it with.".to_owned());
    }
    match format {
        Format::Sumi | Format::Package => {
            s.export_package(path, seed(), Some(crate::timelapse::for_package(&a.dir))).map(|()| Vec::new())
        }
        Format::Psd => s.export_psd(path).map(|()| Vec::new()),
        Format::Clip => s.export_clip(path, false),
        Format::Kra => s.export_clip(path, true),
        Format::Pdf | Format::LayersPdf => s.export_pdf(path, false, pdf_quality).map(|()| Vec::new()),
        Format::Tiff => s.export_tiff(path).map(|()| Vec::new()),
        Format::Jpeg => s.export_jpeg(path).map(|()| Vec::new()),
        _ => s.export_png(path).map(|()| Vec::new()),
    }
    .map_err(|e| e.to_string())
}

/// What an artwork dragged out of the gallery becomes, by Drag and Drop export's preferred file
/// format ([`crate::prefs::DRAG_FORMATS`]).
pub const DRAG_FORMATS: [Format; 5] = [Format::Sumi, Format::Psd, Format::Jpeg, Format::Png, Format::Tiff];

/// An artwork lifted out of the gallery (the folder `dir`, named `name`) written as `format` into
/// the system's temporary folder for dragging; its path.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn drag_file(a: &sumi_core::GalleryArtwork, format: Format) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("Sumi drags");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = export_file_name(a.name.clone(), false);
    let path = dir.join(format!("{name}.{}", format.extension()));
    let warnings = write_artwork(a, format, &path.to_string_lossy(), 100)?;
    // the file the system takes is made: what it kept differently is still said
    if !warnings.is_empty() {
        note_later("Exported with changes", &warnings.join("\n"));
    }
    Ok(path)
}

/// A dragged artwork's picture: its thumbnail fitted in `side` pixels, on white, as a PNG.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn drag_picture(dir: &str, side: u32) -> Option<Vec<u8>> {
    let thumbnail = image::open(Path::new(dir).join("thumbnail.png")).ok()?.thumbnail(side, side).to_rgba8();
    let mut on_white =
        image::RgbaImage::from_pixel(thumbnail.width(), thumbnail.height(), image::Rgba([255, 255, 255, 255]));
    image::imageops::overlay(&mut on_white, &thumbnail, 0, 0);
    let mut png = Vec::new();
    on_white.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

/// `path`, ending in `.extension`.
fn with_extension(path: PathBuf, extension: &str) -> PathBuf {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(extension)) {
        path
    } else {
        path.with_extension(extension)
    }
}

/// A new number for a package's layer ids.
fn seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    #[test]
    fn a_panicking_export_ends_the_run_as_not_made() {
        let ended_not_made = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&ended_not_made);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = EndGuard::new(|made| observed.store(!made, Ordering::Relaxed));
            panic!("the write thread failed");
        }));
        assert!(outcome.is_err());
        assert!(ended_not_made.load(Ordering::Relaxed), "the guard must end the run on unwind");
    }

    #[test]
    fn an_export_that_ends_normally_does_not_end_twice() {
        let ends = Arc::new(AtomicU32::new(0));
        let observed = Arc::clone(&ends);
        {
            let mut guard = EndGuard::new(|_| {
                observed.fetch_add(1, Ordering::Relaxed);
            });
            guard.finish(true);
        }
        assert_eq!(ends.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_new_artwork_records_its_time_lapse_as_its_preset_says() {
        let dir = folder("preset-time-lapse");
        let mut preset = sumi_core::standard_presets().remove(0);
        (preset.time_lapse_resolution, preset.time_lapse_quality, preset.time_lapse_hevc) = (2, 3, true);
        keep_preset_time_lapse(&preset, &dir.to_string_lossy());
        let settings = sumi_core::TimeLapse::new(dir.to_string_lossy().into_owned()).settings();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(settings, sumi_core::TimeLapseSettings { resolution: 2, quality: 3, hevc: true });
    }

    #[test]
    fn a_message_has_other_ways_of_showing_with_its_words_in_each() {
        let ways = other_dialogs("No drawing", "no GPU");
        assert_eq!(ways.map(|(program, _)| program), ["kdialog", "xmessage", "notify-send"]);
        for (_, args) in other_dialogs("No drawing", "no GPU") {
            assert!(args.iter().any(|a| a.contains("no GPU")), "{args:?}");
        }
    }

    #[test]
    fn a_preflight_line_counts_what_a_format_keeps_differently() {
        let clean = ClipChanges::of(Ok(Vec::new()));
        assert_eq!(clean.line(), "0 changes");
        assert!(clean.detail().contains("Nothing the artwork holds"));

        let one = ClipChanges::of(Ok(vec!["the text layer \"Title\" was written as pixels".to_owned()]));
        assert_eq!(one.line(), "1 change");
        assert_eq!(one.detail(), "the text layer \"Title\" was written as pixels");

        let two = ClipChanges::of(Ok(vec!["one".to_owned(), "two".to_owned()]));
        assert_eq!(two.line(), "2 changes");
        assert_eq!(two.detail(), "one\ntwo");
    }

    #[test]
    fn a_refused_preflight_line_is_the_refusal_itself() {
        let refused = ClipChanges::of(Err(sumi_core::MobileError::File {
            reason: "Krita cannot represent the clipping the layer \"Ink\" holds".to_owned(),
        }));
        assert_eq!(refused.line(), "Krita cannot represent the clipping the layer \"Ink\" holds");
        assert_eq!(refused.detail(), refused.line());
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sumi-place-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_made_export_replaces_the_file_it_was_written_beside() {
        let dir = folder("made");
        let target = dir.join("art.psd");
        std::fs::write(&target, b"old").unwrap();
        let staging = Staging::claim(&target).unwrap();
        std::fs::write(&staging.path, b"new").unwrap();
        let staged = staging.path.clone();
        staging.publish().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!staged.exists(), "the staging file was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_export_leaves_what_was_there_as_it_was() {
        let dir = folder("cancelled");
        let target = dir.join("art.psd");
        std::fs::write(&target, b"old").unwrap();
        let staging = Staging::claim(&target).unwrap();
        std::fs::write(&staging.path, b"new").unwrap();
        let staged = staging.path.clone();
        drop(staging);
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert!(!staged.exists(), "a cancelled export left its staging file behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file beside the target that this export did not write: a download's `<target>.part`,
    /// another app's, or a staging file an export that stopped left. A claim skips the staging
    /// name, and publishing never takes either.
    #[test]
    fn publishing_never_takes_a_part_file_it_did_not_write() {
        let dir = folder("foreign-part");
        let target = dir.join("art.psd");
        std::fs::write(&target, b"old").unwrap();
        let foreign = dir.join("art.psd.part");
        std::fs::write(&foreign, b"someone else's").unwrap();
        let left = staging_path(&target, 0);
        std::fs::write(&left, b"another export's").unwrap();
        let staging = Staging::claim(&target).unwrap();
        assert_ne!(staging.path, left, "a staging file another export left was claimed");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"someone else's", "a foreign part was taken");
        assert_eq!(std::fs::read(&left).unwrap(), b"another export's", "a foreign staging file was taken");
        std::fs::write(&staging.path, b"new").unwrap();
        staging.publish().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"someone else's", "a foreign part was taken");
        assert_eq!(std::fs::read(&left).unwrap(), b"another export's", "a foreign staging file was taken");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Delivering a made file, as the animated export does, claims a staging name of its own: a
    /// staging file another export left is neither used nor taken, and the made file is kept.
    #[test]
    fn delivering_a_made_file_leaves_a_foreign_staging_file_alone() {
        let dir = folder("deliver-foreign");
        let made = dir.join("made.gif");
        let target = dir.join("artwork.gif");
        let foreign = staging_path(&target, 0);
        std::fs::write(&made, b"made animation").unwrap();
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&foreign, b"someone else's").unwrap();
        deliver_file(&made, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"made animation", "the made file was not put in place");
        assert_eq!(std::fs::read(&made).unwrap(), b"made animation", "the made file was changed");
        assert_eq!(std::fs::read(&foreign).unwrap(), b"someone else's", "a foreign staging file was taken");
        let foreign_name = foreign.file_name().unwrap().to_string_lossy().into_owned();
        let staging_left: Vec<String> = names_in(&dir).into_iter().filter(|n| n.starts_with(".sumi-export-")).collect();
        assert_eq!(staging_left, vec![foreign_name], "a staging file of its own was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder's share writes each layer's picture into a folder of its own first. Cancelling
    /// it must remove only what the export staged, never a file that was in the folder before.
    #[test]
    fn cancelling_a_share_keeps_a_file_it_did_not_stage() {
        let dir = folder("cancel-before");
        let existing = dir.join("Layer 1.png");
        std::fs::write(&existing, b"the artist's own").unwrap();
        let staging = FolderStaging::claim(&dir).unwrap();
        std::fs::write(staging.path.join("Layer 1.png"), b"new").unwrap();
        let staged = staging.path.clone();
        drop(staging);
        assert_eq!(std::fs::read(&existing).unwrap(), b"the artist's own", "a file there before the share was taken");
        assert!(!staged.exists(), "a cancelled share left its staging folder behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_made_folder_share_replaces_the_files_it_was_written_beside() {
        let dir = folder("made-folder");
        let existing = dir.join("Layer 1.png");
        let untouched = dir.join("notes.txt");
        std::fs::write(&existing, b"old").unwrap();
        std::fs::write(&untouched, b"keep").unwrap();
        let staging = FolderStaging::claim(&dir).unwrap();
        std::fs::write(staging.path.join("Layer 1.png"), b"new").unwrap();
        std::fs::write(staging.path.join("Layer 2.png"), b"second").unwrap();
        let staged = staging.path.clone();
        staging.publish().unwrap();
        drop(staging);
        assert_eq!(std::fs::read(&existing).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("Layer 2.png")).unwrap(), b"second");
        assert_eq!(std::fs::read(&untouched).unwrap(), b"keep");
        assert!(!staged.exists(), "the staging folder was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A target basename up to the filesystem's limit is legal. What the export writes beside it
    /// first must leave room for that name: it cannot be the target's own name with a suffix.
    #[test]
    fn a_legal_long_export_name_leaves_room_for_its_staging_file() {
        let dir = folder("long-staging");
        let target = dir.join(format!("{}.psd", "a".repeat(250)));
        assert!(target.file_name().is_some_and(|n| n.len() <= 255), "the fixture's target name is legal");
        let staging = Staging::claim(&target).expect("a legal target's staging file could be written");
        assert!(staging.path.file_name().is_some_and(|n| n.len() <= 255), "the staging name is too long");
        std::fs::write(&staging.path, b"new").unwrap();
        staging.publish().expect("a legal target could be replaced");
        assert!(target.exists(), "the export was not put in place");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn about_and_dialog_labels_identify_sumi_without_the_old_brand() {
        assert_eq!(about_title(), "Sumi");
        let desc = about_description();
        assert!(!desc.to_ascii_lowercase().contains("craft"), "{desc}");
        assert!(desc.contains("Built-in brushes by Sumi"), "{desc}");
        assert!(ARTWORK_FILTER_LABEL.contains("Sumi"));
        assert_eq!(PLUGIN_FILTER_LABEL, "Sumi plugins");
        assert_eq!(Format::Sumi.extension(), "sumi");
    }

    #[test]
    fn the_old_app_folder_moves_to_sumi_once_and_never_over_a_sumi_folder() {
        let base = std::env::temp_dir().join(format!(
            "sumi-migrate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let old = base.join(sumi_core::legacy::APP_DIR_UNIX);
        let new = base.join("sumi");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("artworks"), b"one").unwrap();
        migrate_dir(&old, &new);
        assert!(!old.exists());
        assert_eq!(std::fs::read(new.join("artworks")).unwrap(), b"one");
        // a later old folder is left alone rather than merged into the moved history
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("late"), b"two").unwrap();
        migrate_dir(&old, &new);
        assert!(old.join("late").exists());
        assert!(!new.join("late").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_fallback_copy_keeps_every_file_and_nested_folder() {
        let base = std::env::temp_dir().join(format!(
            "sumi-copy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let from = base.join("old");
        let to = base.join("new");
        std::fs::create_dir_all(from.join("brush sets/nested")).unwrap();
        std::fs::write(from.join("artworks"), b"one").unwrap();
        std::fs::write(from.join("brush sets/nested/brush"), b"two").unwrap();
        copy_dir(&from, &to).unwrap();
        assert_eq!(std::fs::read(to.join("artworks")).unwrap(), b"one");
        assert_eq!(std::fs::read(to.join("brush sets/nested/brush")).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn native_format_display_labels_use_sumi_extension() {
        assert_eq!(Format::Sumi.name(), "Sumi (.sumi)");
        assert_eq!(Format::ModelArtwork.name(), "Sumi (.sumi)");
        assert_eq!(Format::Sumi.extension(), "sumi");
        assert_eq!(Format::ModelArtwork.extension(), "sumi");
        assert_eq!(DRAG_FORMATS[0], Format::Sumi);
        // the native document formats keep the name and extension each app knows them by
        assert_eq!(Format::Clip.name(), "CLIP STUDIO (.clip)");
        assert_eq!(Format::Clip.extension(), "clip");
        assert_eq!(Format::Kra.name(), "Krita (.kra)");
        assert_eq!(Format::Kra.extension(), "kra");
        assert!(!Format::Clip.package() && !Format::Kra.package());
    }

    #[test]
    fn change_notes_queue_separately_from_errors_in_fifo_order() {
        flush_notes_on_exit();
        let _ = take_notes_wakeup();
        let told_before = TOLD.lock().unwrap_or_else(|e| e.into_inner()).len();

        note_later("Imported with changes", "Layer blend approximated");
        note_later("Exported with changes", "Background not stored");

        assert!(take_notes_wakeup(), "queueing notes should signal a UI wakeup");
        assert!(!take_notes_wakeup(), "wakeup flag should clear after consumption");
        assert_eq!(take_next_note(), Some(("Imported with changes".into(), "Layer blend approximated".into())));
        assert_eq!(take_next_note(), Some(("Exported with changes".into(), "Background not stored".into())));
        assert_eq!(take_next_note(), None);
        assert_eq!(TOLD.lock().unwrap_or_else(|e| e.into_inner()).len(), told_before);
    }
}
