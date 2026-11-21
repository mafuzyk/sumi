//! The artwork's time-lapse, as the Android app keeps it (canvas/TimeLapse.kt, ui/TimeLapseUi.kt):
//! a frame recorded after each change (the core keeps them), the replay over everything, and the
//! video made of it (full length or 30 seconds) with the system's encoder.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, channel, sync_channel};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Id, Order, Rect, Sense, TextureHandle, pos2, vec2};
use sumi_core::{
    CanvasSession, TIME_LAPSE_FPS, ThumbnailImage, TimeLapse, time_lapse_bits_per_pixel, time_lapse_picks,
    time_lapse_side,
};

use crate::alert::{Alert, Answer, Asking};
use crate::icons;
use crate::look::{Look, Themed, Weight, argb, pc, text};
use crate::panel;
use crate::tool_header;

const REPLAY_FILL: Themed = panel::DOCK_FILL;
const GRID_LINE: Themed = panel::DOCK_EDGE;
const EMPTY_INK: Themed = pc::BAR_TEXT;
const PROGRESS: Themed = pc::ACTIVE;

#[derive(Default)]
pub struct TimeLapseUi {
    /// The time-lapse of the artwork open, and its folder.
    lapse: Option<(String, Arc<TimeLapse>)>,
    /// The edits last recorded, and a frame being recorded (a change meanwhile is recorded after).
    recorded: Option<u64>,
    recording: Option<Receiver<()>>,
    replay: Option<TimeLapsePlayer>,
    length: Alert,
    /// Recording turned off with something recorded: whether to delete it.
    purge: Alert,
    /// The video being made, to save once made, as its card shows it; made, it waits for the card
    /// to go before it is saved.
    exporting: Option<(Receiver<Result<PathBuf, String>>, crate::export_card::Exporting)>,
    #[cfg(test)]
    export_reader_metrics: Arc<ReaderMetrics>,
    card: crate::export_card::ExportCard,
    made: Option<PathBuf>,
}

impl TimeLapseUi {
    /// The artwork in folder `dir` (none: not kept in the gallery) is open, its edits now `edits`.
    pub fn open(&mut self, dir: Option<&str>, edits: u64) {
        // the artwork left: the frames it recorded made into one more video (on the GPU where it
        // can be), off the window's thread, their pictures then gone, as the artwork file
        // keeps a time-lapse
        if let Some((left, _)) = self.lapse.take() {
            package_later(left);
        }
        self.lapse = dir.map(|d| (d.to_owned(), TimeLapse::new(d.to_owned())));
        // the pictures its videos hold too go, as the artwork file keeps a time-lapse: only as video
        if let Some((dir, lapse)) = &self.lapse {
            let (packaging, lapse) = (Packaging::start(dir), Arc::clone(lapse));
            std::thread::spawn(move || {
                let _packaging = packaging;
                let gone = drop_pictures(&lapse);
                if gone > 0 {
                    log::info!("the time-lapse's videos hold {gone} frames: their pictures went");
                }
            });
        }
        self.recorded = Some(edits);
        self.replay = None;
    }

    /// The frames recorded so far, and what they take on disk (bytes).
    pub fn info(&self) -> (u32, u64) {
        self.lapse.as_ref().map_or((0, 0), |(_, t)| (t.count(), t.bytes()))
    }

    /// The video's size, quality and codec, kept with the artwork.
    pub fn settings(&self) -> sumi_core::TimeLapseSettings {
        self.lapse.as_ref().map(|(_, t)| t.settings()).unwrap_or_default()
    }

    /// Whether it records changes (the Video tab's switch).
    pub fn recording(&self) -> bool {
        self.lapse.as_ref().is_none_or(|(_, t)| t.recording())
    }

    /// The switch goes at once; turned off with something recorded, whether to delete that is
    /// asked.
    pub fn set_recording(&mut self, on: bool) {
        let Some((_, t)) = &self.lapse else { return };
        t.set_recording(on);
        if !on && t.count() > 0 {
            self.purge.ask(Asking {
                title: "Purge Video?".into(),
                message: "The video recorded so far will be deleted.
It can't be brought back."
                    .into(),
                action: "Purge".into(),
                icon: Some(&icons::ALERT_QUESTION),
                destructive: true,
                extra: None,
                first: Some("Don't Purge".into()),
                alone: false,
            });
        }
    }

    /// Something has been recorded (Replay and Export need it).
    pub fn recorded(&self) -> bool {
        self.lapse.as_ref().is_some_and(|(_, t)| t.count() > 0)
    }

    /// The artwork changed since the last frame (and nothing is pressed on it): a picture of it
    /// is recorded off the window's thread, one at a time while changes come.
    pub fn record(&mut self, ctx: &egui::Context, session: &Arc<CanvasSession>, idle: bool) {
        let Some((_, lapse)) = &self.lapse else { return };
        if let Some(rx) = &self.recording {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                    return;
                }
                _ => self.recording = None,
            }
        }
        let edits = session.edits();
        if !idle || self.recorded == Some(edits) || !lapse.recording() {
            return;
        }
        self.recorded = Some(edits);
        let (tx, rx) = channel();
        let (session, lapse) = (Arc::clone(session), Arc::clone(lapse));
        std::thread::spawn(move || {
            // frames as large as the video they make, private layers left out
            let side = time_lapse_side(lapse.settings());
            if let Some(shot) = session.time_lapse_frame(side) {
                lapse.record(shot.rgba, shot.width, shot.height);
            }
            // one that couldn't be kept is told once, from the window's thread
            if let Some(text) = lapse.take_problem() {
                crate::files::tell_later("Can't keep the time-lapse", &text);
            }
            let _ = tx.send(());
        });
        self.recording = Some(rx);
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    /// The replay starts, over everything.
    pub fn replay(&mut self) {
        let lapse = self.lapse.as_ref().map(|(_, t)| Arc::clone(t));
        self.replay = Some(TimeLapsePlayer::new(lapse));
    }

    pub fn replaying(&self) -> bool {
        self.replay.is_some()
    }

    /// Export time-lapse video: how long, asked first.
    pub fn ask_length(&mut self) {
        self.length.ask(Asking {
            title: "Video length".into(),
            message: String::new(),
            action: "30 Seconds".into(),
            icon: Some(&icons::CLOCK),
            destructive: false,
            extra: None,
            first: Some("Full length".into()),
            alone: false,
        });
    }

    /// The replay while it shows, the length alert, and a video being made (named after the
    /// artwork's `name`); returns a message to show, if any.
    pub fn show(&mut self, ctx: &egui::Context, look: &mut Look, name: &str) -> Option<&'static str> {
        if let Some(replay) = self.replay.as_mut()
            && replay.show(ctx, look)
        {
            self.replay = None;
        }
        // Purge deletes what was recorded; Don't Purge, or a tap beside it, keeps it
        if self.purge.show(ctx, look) == Some(Answer::Action)
            && let Some((_, t)) = &self.lapse
        {
            t.purge();
            self.recorded = None;
        }
        let mut said = None;
        match self.length.show(ctx, look) {
            Some(Answer::First) => said = self.export(None, name),
            Some(Answer::Action) => said = self.export(Some(30), name),
            _ => {}
        }
        if let Some((rx, exporting)) = &self.exporting {
            match rx.try_recv() {
                Ok(made) => {
                    let cancelled = exporting.cancelled();
                    self.exporting = None;
                    match made {
                        Ok(path) if !cancelled => self.made = Some(path),
                        Ok(_) => {}
                        Err(e) if !cancelled => {
                            log::warn!("exporting the time-lapse: {e}");
                            said = Some(if e == crate::video::NO_FFMPEG {
                                crate::video::NO_FFMPEG
                            } else {
                                "The time-lapse video couldn't be made."
                            });
                        }
                        Err(_) => {}
                    }
                }
                Err(_) => ctx.request_repaint_after(Duration::from_millis(100)),
            }
        }
        // saved once its card has gone, as the tablet shares it
        if self.card.show(ctx, look) == Some(crate::export_card::Ended::Made)
            && let Some(path) = self.made.take()
        {
            save_as(&path);
        }
        said
    }

    /// The video made off the window's thread: all the frames, or `seconds` of them spread
    /// evenly; each fitted on white into the first one's size, within the settings' side.
    fn export(&mut self, seconds: Option<u32>, name: &str) -> Option<&'static str> {
        let Some((_, lapse)) = &self.lapse else { return Some("There is no time-lapse to export yet.") };
        if lapse.count() == 0 {
            return Some("There is no time-lapse to export yet.");
        }
        if self.card.busy() {
            return None;
        }
        let (tx, rx) = channel();
        let lapse = Arc::clone(lapse);
        let done = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let following = Arc::clone(&done);
        let exporting = self.card.start(move || f32::from_bits(following.load(std::sync::atomic::Ordering::Relaxed)));
        let told = exporting.clone();
        #[cfg(test)]
        let export_reader_metrics = Arc::clone(&self.export_reader_metrics);
        let name: String = name.chars().filter(|c| c.is_alphanumeric() || " _-".contains(*c)).collect();
        let name = if name.trim().is_empty() { "Untitled Artwork".to_owned() } else { name.trim().to_owned() };
        // a path of its own: another export, or another video of this artwork, can't overwrite it
        let out = crate::video::export_temp_path(&name, "mp4");
        std::thread::spawn(move || {
            let made = (|| {
                let picks = time_lapse_picks(lapse.count(), seconds);
                let frames = read_in_order(
                    Arc::clone(&lapse),
                    picks.clone(),
                    ReadPurpose::Export,
                    Some(told.cancellation_flag()),
                    #[cfg(test)]
                    Some(Arc::clone(&export_reader_metrics)),
                );
                let (_, first) = frames.recv_until_cancelled(&told).map_err(|e| match e {
                    ReadFrameError::Cancelled => "the time-lapse export was cancelled",
                    ReadFrameError::Disconnected => "the first frame can't be read",
                })?;
                let (width, height) = (first.width, first.height);
                let last = std::cell::RefCell::new(Some(first));
                let settings = lapse.settings();
                std::fs::create_dir_all(out.parent().unwrap_or(&out)).map_err(|e| e.to_string())?;
                crate::video::write_mp4(
                    &out,
                    width,
                    height,
                    time_lapse_side(settings),
                    TIME_LAPSE_FPS as f32,
                    settings.hevc,
                    time_lapse_bits_per_pixel(settings),
                    picks.len(),
                    |n, w, h| {
                        // cancelled, the frames stop (and the video is not made)
                        if told.cancelled() {
                            return None;
                        }
                        let progress = n as f32 / picks.len().max(1) as f32;
                        done.store(progress.to_bits(), std::sync::atomic::Ordering::Relaxed);
                        // the first was read already; one that can't be read, the one before again
                        if n > 0 {
                            match frames.recv_until_cancelled(&told) {
                                Ok((_, frame)) => *last.borrow_mut() = Some(frame),
                                Err(ReadFrameError::Cancelled) => return None,
                                // A missing/truncated frame still repeats the last good frame.
                                Err(ReadFrameError::Disconnected) => {}
                            }
                        }
                        Some(on_white(last.borrow().as_ref()?, w, h))
                    },
                )?;
                Ok(())
            })();
            // on cancel (or failure) the file is removed: never published, never left half written
            let made = match made {
                Ok(()) => crate::video::take_made(&out, false, told.cancelled())
                    .map(|(path, _)| path)
                    .ok_or_else(|| "the time-lapse video couldn't be made".to_owned()),
                Err(e) => {
                    let _ = crate::video::take_made(&out, true, false);
                    Err(e)
                }
            };
            told.ended(made.is_ok() && !told.cancelled());
            let _ = tx.send(made);
        });
        self.exporting = Some((rx, exporting));
        None
    }
}

/// Whether the artwork in folder `artwork` has a time-lapse recorded, read without changing it;
/// while its pictures are being made into videos it is not read (they may be dropped as it goes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recorded {
    Yes,
    No,
    Packaging,
}

/// The artworks whose time-lapse is being made into videos (and whose pictures it may drop), so
/// the gallery doesn't read one half done.
static PACKAGING: LazyLock<Mutex<std::collections::HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// One artwork's packaging, marked while it lives.
pub(crate) struct Packaging(String);

impl Packaging {
    fn start(artwork: &str) -> Packaging {
        if let Ok(mut packaging) = PACKAGING.lock() {
            *packaging.entry(artwork.to_owned()).or_insert(0) += 1;
        }
        Packaging(artwork.to_owned())
    }
}

impl Drop for Packaging {
    fn drop(&mut self) {
        if let Ok(mut packaging) = PACKAGING.lock()
            && let Some(count) = packaging.get_mut(&self.0)
        {
            if *count > 1 {
                *count -= 1;
            } else {
                packaging.remove(&self.0);
            }
        }
    }
}

fn packaging(artwork: &str) -> bool {
    PACKAGING.lock().is_ok_and(|packaging| packaging.contains_key(artwork))
}

/// Holds packaging open for a test, as the editor does while it makes the videos.
#[cfg(test)]
pub(crate) fn hold_packaging(artwork: &str) -> Packaging {
    Packaging::start(artwork)
}

/// Makes the time-lapse of `dir` into videos on a thread of its own, marked as packaging before it
/// starts: the gallery waits for it rather than read half-written frames.
pub(crate) fn package_later(dir: String) {
    let packaging = Packaging::start(&dir);
    std::thread::spawn(move || {
        let _packaging = packaging;
        let _ = for_package(&dir);
    });
}

/// Whether the artwork in folder `artwork` has a time-lapse recorded, read without changing it.
pub(crate) fn recorded(artwork: &str) -> Recorded {
    if packaging(artwork) {
        return Recorded::Packaging;
    }
    let has = TimeLapse::new(artwork.to_owned()).count() > 0;
    // it began being packaged as it was read: read again once it is done
    if packaging(artwork) {
        return Recorded::Packaging;
    }
    if has { Recorded::Yes } else { Recorded::No }
}

/// The time-lapse of the artwork in folder `dir` as a package keeps it, its frames made into
/// videos first (only those since the last share, when the videos made then still hold).
pub fn for_package(dir: &str) -> sumi_core::PackageTimeLapse {
    let _packaging = Packaging::start(dir);
    let lapse = TimeLapse::new(dir.to_owned());
    let mut plan = lapse.segment_plan();
    while let Some(p) = plan {
        plan = match make_segment(&lapse, &p) {
            Ok((w, h)) => lapse.segment_made(p, w, h, true),
            Err(e) => {
                log::warn!("a time-lapse video for a package: {e}");
                lapse.segment_made(p, 0, 0, false)
            }
        };
    }
    // the pictures now in a video go, as the artwork file keeps a time-lapse: only as video
    drop_pictures(&lapse);
    lapse.for_package()
}

/// The pictures its videos hold too gone, once this system has read those videos: videos go
/// through its own decoders, and where it has none for them the pictures are all there is to
/// replay. How many went.
fn drop_pictures(lapse: &TimeLapse) -> u32 {
    let readable = lapse.clips_with_pictures().iter().all(|clip| {
        let mut read = false;
        let done = crate::video::read_frames(std::path::Path::new(&clip.path), |_, _, _, _| {
            read = true;
            false
        });
        if let Err(e) = done {
            log::warn!("a time-lapse video: {e}");
        }
        read
    });
    if readable {
        lapse.drop_clip_frames()
    } else {
        log::warn!("the time-lapse's videos can't be read here: their pictures stay");
        0
    }
}

/// Sets its flag once dropped: what it stops, stopped however its holder ends.
struct Stops(Arc<AtomicBool>);

impl Drop for Stops {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// What reading the frames ahead has come to: all read, or why one couldn't be (the first such
/// cause, as the video reader gives it).
#[derive(Default)]
struct ReadState {
    finished: bool,
    problem: Option<String>,
    #[cfg(test)]
    hit_full_queue: bool,
}

/// Frames read ahead off the window's thread, and the stop that ends their reading when they go.
struct Reading {
    frames: Receiver<(u32, ThumbnailImage)>,
    state: Arc<Mutex<ReadState>>,
    _stops: Stops,
}

/// Cancelled reader without its receiver (which could block a full send queue).
/// Its worker must finish, including reaping the video decoder, before a new one starts.
struct RetiringReader(Arc<Mutex<ReadState>>);

impl RetiringReader {
    fn finished(&self) -> bool {
        self.0.lock().is_ok_and(|state| state.finished)
    }
}

#[cfg(test)]
#[derive(Default)]
struct ReaderMetrics {
    active: AtomicUsize,
    peak: AtomicUsize,
    started: AtomicUsize,
    simulated_startup_ms: std::sync::atomic::AtomicU64,
}

#[cfg(test)]
struct ActiveReader(Arc<ReaderMetrics>);

#[cfg(test)]
impl ActiveReader {
    fn new(metrics: Arc<ReaderMetrics>) -> Self {
        let active = metrics.active.fetch_add(1, Ordering::SeqCst) + 1;
        metrics.peak.fetch_max(active, Ordering::SeqCst);
        metrics.started.fetch_add(1, Ordering::SeqCst);
        Self(metrics)
    }
}

#[cfg(test)]
impl Drop for ActiveReader {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReadFrameError {
    Cancelled,
    Disconnected,
}

impl Reading {
    /// Export-card cancellation is checked while waiting for *any* frame, including the
    /// first one. Its atomic is also the reader's stop flag, so the worker wakes even
    /// while waiting behind another Required decoder permit.
    fn recv_until_cancelled(
        &self,
        exporting: &crate::export_card::Exporting,
    ) -> Result<(u32, ThumbnailImage), ReadFrameError> {
        loop {
            if exporting.cancelled() {
                self._stops.0.store(true, Ordering::Release);
                return Err(ReadFrameError::Cancelled);
            }
            match self.frames.recv_timeout(Duration::from_millis(20)) {
                Ok(frame) => {
                    if exporting.cancelled() {
                        self._stops.0.store(true, Ordering::Release);
                        return Err(ReadFrameError::Cancelled);
                    }
                    return Ok(frame);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return if exporting.cancelled() {
                        Err(ReadFrameError::Cancelled)
                    } else {
                        Err(ReadFrameError::Disconnected)
                    };
                }
            }
        }
    }

    /// What it has come to: whether it is done, and the first cause it couldn't read a frame.
    fn state(&self) -> ReadState {
        match self.state.lock() {
            Ok(state) => ReadState {
                finished: state.finished,
                problem: state.problem.clone(),
                #[cfg(test)]
                hit_full_queue: state.hit_full_queue,
            },
            Err(_) => ReadState {
                finished: true,
                problem: Some("the frames' reader stopped".into()),
                #[cfg(test)]
                hit_full_queue: false,
            },
        }
    }

    fn retire(self) -> RetiringReader {
        let state = Arc::clone(&self.state);
        // Dropping Stops sets the stop flag; dropping the receiver wakes a blocked send.
        drop(self);
        RetiringReader(state)
    }
}

/// Marks its reading finished however the reading thread ends.
struct Finished(Arc<Mutex<ReadState>>);

impl Drop for Finished {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.finished = true;
        }
    }
}

/// Notes the first cause a frame couldn't be read (the one after it tells nothing new).
fn note_problem(state: &Arc<Mutex<ReadState>>, problem: String) {
    if let Ok(mut state) = state.lock()
        && state.problem.is_none()
    {
        state.problem = Some(problem);
    }
}

/// Never hold the global decoder mutex during delivery. A competing export/player can
/// preempt a paused replay by setting `stop`, which interrupts the retry loop within
/// 5 ms. Keep backpressure rather than dropping frames: exports need every image.
fn deliver_frame(
    tx: &SyncSender<(u32, ThumbnailImage)>,
    stop: &AtomicBool,
    stalled: &AtomicBool,
    #[cfg(test)] state: &Arc<Mutex<ReadState>>,
    mut frame: (u32, ThumbnailImage),
) -> bool {
    // A full queue is normal for a fast decoder while the UI displays its frames.
    // Yield to another replay only after the queue has stayed full long enough to
    // indicate that no frames are being consumed (e.g. an actually paused replay).
    let mut full_since = None;
    while !stop.load(Ordering::Acquire) {
        match tx.try_send(frame) {
            Ok(()) => {
                stalled.store(false, Ordering::Release);
                return true;
            }
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(pending)) => {
                let since = full_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(200) {
                    stalled.store(true, Ordering::Release);
                }
                #[cfg(test)]
                if let Ok(mut state) = state.lock() {
                    state.hit_full_queue = true;
                }
                frame = pending;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    false
}

#[derive(Clone, Copy)]
enum ReadPurpose {
    Replay,
    Export,
}

/// Frames `wanted` (rising) of `lapse`, each with its number, read in order off the window's
/// thread a few ahead: from its videos (decoded as they come, the artwork file's way of keeping them)
/// and its pictures. A frame that can't be read is left out; what was left out is in [`Reading`]'s
/// state. It stops once it is dropped or nothing takes them.
fn read_in_order(
    lapse: Arc<TimeLapse>,
    wanted: Vec<u32>,
    purpose: ReadPurpose,
    cancellation: Option<Arc<AtomicBool>>,
    #[cfg(test)] metrics: Option<Arc<ReaderMetrics>>,
) -> Reading {
    let (tx, rx) = sync_channel(4);
    let state = Arc::new(Mutex::new(ReadState::default()));
    let shared = Arc::clone(&state);
    let stop = cancellation.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let stalled = Arc::new(AtomicBool::new(false));
    let stops = Stops(Arc::clone(&stop));
    std::thread::spawn(move || {
        let _finished = Finished(Arc::clone(&shared));
        #[cfg(test)]
        let _active = metrics.as_ref().map(|metrics| ActiveReader::new(Arc::clone(metrics)));
        let clips = lapse.clips();
        let mut next = 0usize;
        while let Some(&i) = wanted.get(next) {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Some(clip) = clips.iter().find(|c| i >= c.first && i - c.first < c.frames) else {
                match lapse.frame(i) {
                    Some(frame) => {
                        if !deliver_frame(
                            &tx,
                            &stop,
                            &stalled,
                            #[cfg(test)]
                            &shared,
                            (i, frame),
                        ) {
                            return;
                        }
                    }
                    None => note_problem(&shared, "a frame can't be read".into()),
                }
                next += 1;
                continue;
            };
            let end = clip.first + clip.frames;
            let (mut at, mut gone) = (clip.first, false);
            let read = crate::video::read_frames_cancelable(
                std::path::Path::new(&clip.path),
                Arc::clone(&stop),
                Arc::clone(&stalled),
                match purpose {
                    ReadPurpose::Replay => crate::video::DecoderPriority::Replay,
                    ReadPurpose::Export => crate::video::DecoderPriority::Required,
                },
                |rgba, width, height, _| {
                    let here = at;
                    at += 1;
                    if wanted.get(next) == Some(&here) {
                        next += 1;
                        if !deliver_frame(
                            &tx,
                            &stop,
                            &stalled,
                            #[cfg(test)]
                            &shared,
                            (here, ThumbnailImage { width, height, rgba }),
                        ) {
                            gone = true;
                            return false;
                        }
                    }
                    !stop.load(Ordering::Relaxed) && wanted.get(next).is_some_and(|&n| n < end)
                },
                #[cfg(test)]
                metrics.as_ref().map_or(0, |m| m.simulated_startup_ms.load(Ordering::SeqCst)),
            );
            if gone {
                return;
            }
            if let Err(e) = read {
                log::warn!("a time-lapse video: {e}");
                note_problem(&shared, e);
            }
            // frames the video didn't hold (fewer than noted): left out
            while wanted.get(next).is_some_and(|&n| n < end) {
                next += 1;
            }
        }
    });
    Reading { frames: rx, state, _stops: stops }
}

/// `plan`'s frames as a video, fitted on white; one added to those before comes out their size.
fn make_segment(lapse: &TimeLapse, plan: &sumi_core::SegmentPlan) -> Result<(u32, u32), String> {
    let settings = sumi_core::TimeLapseSettings { hevc: plan.hevc, ..lapse.settings() };
    let (width, height, side) = if plan.width > 0 && plan.height > 0 {
        (plan.width, plan.height, plan.width.max(plan.height))
    } else {
        let first = lapse.frame(*plan.frames.first().ok_or("no frames")?).ok_or("the first frame can't be read")?;
        (first.width, first.height, time_lapse_side(settings))
    };
    let size = crate::video::write_mp4(
        std::path::Path::new(&plan.path),
        width,
        height,
        side,
        TIME_LAPSE_FPS as f32,
        settings.hevc,
        time_lapse_bits_per_pixel(settings),
        plan.frames.len(),
        |n, w, h| Some(on_white(&lapse.frame(*plan.frames.get(n)?)?, w, h)),
    )?;
    if plan.append && size != (plan.width, plan.height) {
        return Err("the encoder won't make it the size of those before".into());
    }
    Ok(size)
}

/// The time-lapse of the package at `package` taken into the artwork folder `dir`: its settings,
/// and its videos' frames (the videos kept to go back into a package when they all came in).
pub fn take_package(package: &std::path::Path, dir: &str) {
    let temp = std::env::temp_dir().join(format!("sumi-segments-{}", std::process::id()));
    let taken =
        sumi_core::package_time_lapse(package.to_string_lossy().into_owned(), temp.to_string_lossy().into_owned());
    let t = match taken {
        Ok(Some(t)) => t,
        Ok(None) => return,
        Err(e) => {
            log::warn!("a package's time-lapse: {e}");
            return;
        }
    };
    let lapse = TimeLapse::new(dir.to_owned());
    // as the package keeps it: its videos as they are, nothing decoded
    if lapse.take_clips(t.clone()) {
        let _ = std::fs::remove_dir_all(&temp);
        return;
    }
    let videos = t.segments.clone();
    lapse.take_begin(t);
    for path in videos {
        let read = crate::video::read_frames(std::path::Path::new(&path), |rgba, w, h, _| {
            lapse.keep_frame(rgba, w, h);
            true
        });
        let frames = read.unwrap_or_else(|e| {
            log::warn!("a package's time-lapse video: {e}");
            0
        });
        lapse.take_video(path, frames);
    }
    lapse.take_end();
    let _ = std::fs::remove_dir_all(&temp);
}

/// `frame` fitted into `w` × `h` on white, as RGBA rows.
fn on_white(frame: &sumi_core::ThumbnailImage, w: u32, h: u32) -> Vec<u8> {
    let mut out = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
    if let Some(picture) = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone()) {
        let k = (w as f32 / frame.width.max(1) as f32).min(h as f32 / frame.height.max(1) as f32);
        let (dw, dh) = (((frame.width as f32 * k) as u32).max(1), ((frame.height as f32 * k) as u32).max(1));
        let fitted = image::imageops::resize(&picture, dw, dh, image::imageops::FilterType::Triangle);
        image::imageops::overlay(&mut out, &fitted, i64::from((w - dw.min(w)) / 2), i64::from((h - dh.min(h)) / 2));
    }
    out.into_raw()
}

/// The video made copied where the person picks.
fn save_as(made: &std::path::Path) {
    let name = made.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if cfg!(target_os = "linux") {
        let made = crate::video::OwnedExport::new(made.to_path_buf());
        crate::file_chooser::save_with_result("Export time-lapse video", "mp4", &name, move |selected| {
            if let Some(to) = selected {
                crate::files::write_meanwhile(false, move || {
                    let mut made = made;
                    match save_to(&made.path, &to) {
                        Ok(()) => made.keep_if_saved_here(&to),
                        Err(e) => crate::files::tell_later("Can't export", &e.to_string()),
                    }
                });
            }
        });
        return;
    }
    let Some(to) = rfd::FileDialog::new()
        .set_title("Export time-lapse video")
        .set_file_name(name)
        .add_filter("MP4 video", &["mp4"])
        .save_file()
    else {
        return;
    };
    if let Err(e) = save_to(made, &to) {
        crate::files::tell("Can't export", &e.to_string());
    }
}

/// The video made copied to `to`, whole or not at all: into a file of its own beside it, then
/// renamed over it (copied onto itself, a file is emptied; cut short, the one there would be).
fn save_to(made: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    let mut n = 0;
    // a name of its own: never over a file that is already beside `to`
    let staging = loop {
        let part = save_staging(to, n);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(_) => break part,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
    };
    let put = std::fs::copy(made, &staging).and_then(|_| std::fs::rename(&staging, to));
    if put.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    put
}

/// The file beside `to` a save copies into first, `n` keeping it its own: named on its own so a
/// destination at the filesystem's name limit still leaves it room.
fn save_staging(to: &std::path::Path, n: u32) -> PathBuf {
    to.with_file_name(format!(".sumi-save-{}-{n}.part", std::process::id()))
}

/// The artwork replayed with transport controls. Decoding stays on the reader thread.
pub(crate) struct TimeLapsePlayer {
    count: u32,
    index: u32,
    stepped: Instant,
    playing: bool,
    speed: u32,
    timeline_scrubbing: bool,
    /// Dragged: where it began and the frame then.
    scrub: Option<(f32, u32)>,
    shown: Option<(u32, TextureHandle)>,
    /// The frames from one on, read ahead (from its videos, decoded as they come), and when it
    /// last began again elsewhere.
    ahead: Option<Ahead>,
    /// A stopped reader remains here until its worker and any decoder have exited.
    /// We never start another reader while this is unfinished.
    retiring: Option<RetiringReader>,
    #[cfg(test)]
    reader_metrics: Arc<ReaderMetrics>,
    began: Instant,
    /// The time-lapse it replays, if there is one (none: nothing recorded yet).
    lapse: Option<Arc<TimeLapse>>,
    /// Why no frame could be shown, once its frames were all read.
    unavailable: Option<&'static str>,
}

/// Frames read ahead from `from`: the next to come, and one come before it was wanted.
struct Ahead {
    from: u32,
    next: u32,
    /// Last frame requested, exclusive; a playback window is bounded even at 16x.
    until: u32,
    stride: u32,
    waiting: Option<(u32, ThumbnailImage)>,
    reading: Reading,
}

impl Ahead {
    fn can_serve(&self, from: u32, to: u32, stride: u32) -> bool {
        // The reader has consumed everything before `next`, except a frame kept in waiting.
        // It can still deliver a later in-window frame without another decoder.
        let earliest = self.waiting.as_ref().map_or(self.next, |(i, _)| *i);
        to >= from
            && to >= self.from
            && to >= earliest
            && to < self.until
            && self.stride == stride
            && !self.reading._stops.0.load(Ordering::Acquire)
            // A 16x reader contains 0,16,32,..., not arbitrary frames between them.
            // A timeline seek to an unrequested frame needs a new window anchored there.
            && (to - self.from) % stride.max(1) == 0
    }
}

impl TimeLapsePlayer {
    /// A player of `lapse`'s frames; none, nothing recorded yet.
    pub(crate) fn new(lapse: Option<Arc<TimeLapse>>) -> TimeLapsePlayer {
        let count = lapse.as_ref().map_or(0, |t| t.count());
        TimeLapsePlayer {
            count,
            index: 0,
            stepped: Instant::now(),
            playing: count > 1,
            speed: 1,
            timeline_scrubbing: false,
            scrub: None,
            shown: None,
            ahead: None,
            retiring: None,
            #[cfg(test)]
            reader_metrics: Arc::new(ReaderMetrics::default()),
            began: Instant::now(),
            lapse: lapse.filter(|_| count > 0),
            unavailable: None,
        }
    }

    /// Plays the time-lapse of the artwork in folder `artwork`, read without changing it.
    pub(crate) fn for_gallery(artwork: &str) -> TimeLapsePlayer {
        TimeLapsePlayer::new(Some(TimeLapse::new(artwork.to_owned())))
    }

    /// What it says instead of frames: nothing was recorded, or why none could be read.
    pub(crate) fn message(&self) -> Option<&'static str> {
        if self.count == 0 { Some("Nothing recorded yet") } else { self.unavailable }
    }

    /// Whether a frame has been shown (what the gallery and editor wait for).
    #[cfg(test)]
    pub(crate) fn showing(&self) -> bool {
        self.shown.is_some()
    }

    /// The stop signal of the frames being read ahead, held to check a drop stops them.
    #[cfg(test)]
    fn reading_stop(&self) -> Option<Arc<AtomicBool>> {
        self.ahead.as_ref().map(|a| Arc::clone(&a.reading._stops.0))
    }

    /// Seek in recorded frames, independent of the speed of playback.
    fn seek(&mut self, frame: u32) {
        let next = frame.min(self.count.saturating_sub(1));
        if next != self.index {
            if self.ahead.as_ref().is_some_and(|a| !a.can_serve(self.index, next, self.speed)) {
                self.retire_active();
            }
            self.unavailable = None;
        }
        self.index = next;
        self.stepped = Instant::now();
    }

    fn retire_active(&mut self) {
        if let Some(ahead) = self.ahead.take() {
            debug_assert!(self.retiring.is_none(), "a replacement cannot start before cancellation finishes");
            self.retiring = Some(ahead.reading.retire());
        }
    }

    fn jump_seconds(&mut self, seconds: i32) {
        let delta = i64::from(seconds) * i64::from(TIME_LAPSE_FPS);
        self.seek((i64::from(self.index) + delta).clamp(0, i64::from(self.count.saturating_sub(1))) as u32);
    }

    fn toggle_playback(&mut self) {
        if self.count <= 1 {
            return;
        }
        if !self.playing && self.index + 1 == self.count {
            self.seek(0);
        }
        self.playing = !self.playing;
        self.stepped = Instant::now();
    }

    /// Wall-clock playback at the recording's 30 fps, even when a UI repaint is late.
    /// Speed skips frames instead of decoding on the UI thread. If a frame is still
    /// being read, keep the elapsed time as debt and catch up once it is available.
    fn advance(&mut self, ready: bool, elapsed: Duration) {
        if !self.playing || !ready || self.scrub.is_some() || self.timeline_scrubbing {
            return;
        }
        let step = Duration::from_secs_f64(1.0 / f64::from(TIME_LAPSE_FPS));
        let ticks = elapsed.as_nanos() / step.as_nanos();
        if ticks == 0 {
            return;
        }
        let frames = ticks.saturating_mul(u128::from(self.speed));
        let last = self.count.saturating_sub(1);
        self.index = (u128::from(self.index).saturating_add(frames)).min(u128::from(last)) as u32;
        // Preserve the fractional frame period rather than resetting to `now`:
        // resetting made 1x systematically slower whenever a repaint was late.
        let remainder = Duration::from_nanos((elapsed.as_nanos() % step.as_nanos()) as u64);
        let now = Instant::now();
        self.stepped = now.checked_sub(remainder).unwrap_or(now);
        if self.index >= last {
            self.playing = false;
        }
    }

    fn timeline_frame(&self, x: f32, bar: Rect) -> u32 {
        let fraction = ((x - bar.left()) / bar.width().max(1.0)).clamp(0.0, 1.0);
        (fraction * self.count.saturating_sub(1) as f32).round() as u32
    }

    /// The frame to show now, once read: frames read ahead from it, again from where it is when
    /// it jumps back or far ahead (while scrubbing, at most every 0.1 s). Once all its frames have
    /// been read without one to show, it says why it can't instead of staying blank.
    fn fetch(&mut self, ctx: &egui::Context, lapse: &Arc<TimeLapse>) {
        if self.unavailable.is_some() {
            return;
        }
        if self.retiring.as_ref().is_some_and(RetiringReader::finished) {
            self.retiring = None;
        }
        let want = self.index;
        if self.shown.as_ref().is_some_and(|(i, _)| *i == want) {
            return;
        }
        // A waiting export/other player may preempt a paused replay. That is an
        // intentional cancellation, not a corrupt frame or end-of-video error.
        if self.ahead.as_ref().is_some_and(|a| a.reading._stops.0.load(Ordering::Acquire)) {
            self.retire_active();
        }
        // Playback changes speed without a seek; invalidate its previous stride here.
        if self.ahead.as_ref().is_some_and(|a| !a.can_serve(want, want, self.speed)) {
            self.retire_active();
        }
        let scrubbing = self.scrub.is_some() || self.timeline_scrubbing;
        // Cancelled video decoders must exit (including ffprobe) before the next can start.
        // This remains async for the UI even if the old decode was blocked mid-RGBA frame.
        if self.retiring.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }
        // While dragging, throttle only new readers, not seeks the active reader can serve.
        if self.ahead.is_none() && (!scrubbing || self.began.elapsed() >= Duration::from_millis(150)) {
            let wanted = playback_window(want, self.count, self.speed);
            let until = wanted.last().map_or(want.saturating_add(1), |last| last.saturating_add(1));
            let reading = read_in_order(
                Arc::clone(lapse),
                wanted,
                ReadPurpose::Replay,
                None,
                #[cfg(test)]
                Some(Arc::clone(&self.reader_metrics)),
            );
            self.ahead = Some(Ahead { from: want, next: want, until, stride: self.speed, waiting: None, reading });
            self.began = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_millis(16));
        let Some(ahead) = self.ahead.as_mut() else { return };
        while let Some((i, frame)) = ahead.waiting.take().or_else(|| ahead.reading.frames.try_recv().ok()) {
            ahead.next = i + 1;
            if i < want {
                continue;
            }
            if i > want && scrubbing {
                ahead.waiting = Some((i, frame));
                break;
            }
            // one left out (unreadable): the next shown in its place
            self.index = i;
            let image =
                egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
            self.shown = Some((i, ctx.load_texture("time-lapse", image, egui::TextureOptions::LINEAR)));
            break;
        }
        // No more frames will arrive at the requested position. Stop instead of repeatedly
        // restarting an exhausted/truncated clip (including after a previous frame was shown).
        if let Some(ahead) = &self.ahead
            && ahead.reading.state().finished
            && ahead.waiting.is_none()
            && self.shown.as_ref().is_none_or(|(i, _)| *i != self.index)
        {
            self.unavailable = Some(problem_message(ahead.reading.state().problem.as_deref()));
            self.playing = false;
            self.retire_active();
            ctx.request_repaint();
        }
    }

    /// True when Done was tapped (or Escape).
    pub(crate) fn show(&mut self, ctx: &egui::Context, look: &mut Look) -> bool {
        let screen = ctx.content_rect();
        let caught_up = self.shown.as_ref().is_some_and(|(i, _)| *i == self.index);
        let step = Duration::from_secs_f64(1.0 / f64::from(TIME_LAPSE_FPS));
        self.advance(caught_up, self.stepped.elapsed());
        if self.playing && self.scrub.is_none() && !self.timeline_scrubbing {
            ctx.request_repaint_after(step.saturating_sub(self.stepped.elapsed()));
        }
        let keyboard = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
            )
        });
        if keyboard.0 {
            self.toggle_playback();
        }
        if keyboard.1 {
            self.jump_seconds(-5);
        }
        if keyboard.2 {
            self.jump_seconds(5);
        }
        if keyboard.3 {
            self.seek(0);
        }
        if keyboard.4 {
            self.seek(self.count.saturating_sub(1));
        }
        let lapse = self.lapse.clone();
        if let Some(lapse) = &lapse {
            self.fetch(ctx, lapse);
        }
        // (under the bar with its Done, in the Foreground: in one layer order, a press on the picture
        // would raise it over the bar)
        egui::Area::new(Id::unique("time-lapse replay")).fixed_pos(screen.min).order(Order::Middle).show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::hover());
            ui.painter().rect_filled(screen, 0, argb(REPLAY_FILL));
            surround_grid(ui, screen);
            let stage = Rect::from_min_max(
                pos2(screen.left(), screen.top() + tool_header::HEIGHT),
                pos2(screen.right(), (screen.bottom() - 116.0).max(screen.top() + tool_header::HEIGHT)),
            );
            let response = ui.interact(stage, ui.scope_id().with("replay stage scrub"), Sense::click_and_drag());
            if let Some((_, t)) = &self.shown {
                // Fit the whole recording above the transport instead of cropping it behind UI.
                let [w, h] = t.size().map(|v| v.max(1) as f32);
                let k = (stage.width() / w).min(stage.height() / h);
                let picture = Rect::from_center_size(stage.center(), vec2(w * k, h * k));
                ui.painter().image(t.id(), picture, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            if let Some(message) = self.message() {
                if self.unavailable.is_some() {
                    unavailable_note(ui, screen, message);
                } else {
                    text(
                        ui.painter(),
                        stage.center(),
                        Align2::CENTER_CENTER,
                        message,
                        17.0,
                        Weight::Regular,
                        argb(EMPTY_INK),
                    );
                }
            }
            if response.drag_started()
                && let Some(p) = response.interact_pointer_pos()
            {
                self.scrub = Some((p.x, self.index));
            }
            if let (Some((start, from)), Some(p)) = (self.scrub, response.interact_pointer_pos())
                && response.dragged()
            {
                let moved = (p.x - start) / stage.width().max(1.0);
                let to = from as f32 + (moved * self.count as f32).trunc();
                self.seek(to.clamp(0.0, self.count.saturating_sub(1) as f32) as u32);
            }
            if response.drag_stopped() {
                self.scrub = None;
                self.stepped = Instant::now();
            }
            self.transport(ui, screen);
        });
        // the time into the replay, in frames past the second
        let seconds = self.index / TIME_LAPSE_FPS;
        let time = format!(
            "{:02}:{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            self.index % TIME_LAPSE_FPS
        );
        let tapped = tool_header::show_in(ctx, look, "Time-lapse Replay", &[(&time, false)], tool_header::OVER_PICTURE);
        crate::ui::escape(ctx) || tapped == Some(tool_header::Tapped::Done)
    }

    /// A real timeline and transport, unlike the old decoration below the title bar.
    fn transport(&mut self, ui: &egui::Ui, screen: Rect) {
        let bottom = Rect::from_min_max(pos2(screen.left(), screen.bottom() - 116.0), screen.max);
        ui.painter().rect_filled(bottom, 0.0, argb(panel::DOCK_FILL));
        ui.painter()
            .line_segment([bottom.left_top(), bottom.right_top()], egui::Stroke::new(1.0, argb(panel::DOCK_EDGE)));
        let timeline = Rect::from_min_max(
            pos2(bottom.left() + 24.0, bottom.top() + 24.0),
            pos2(bottom.right() - 24.0, bottom.top() + 39.0),
        );
        let rail = Rect::from_center_size(timeline.center(), vec2(timeline.width(), 5.0));
        ui.painter().rect_filled(rail, 3.0, argb(panel::DOCK_EDGE));
        let fraction = if self.count > 1 { self.index as f32 / (self.count - 1) as f32 } else { 0.0 };
        let knob = pos2(rail.left() + rail.width() * fraction, rail.center().y);
        ui.painter().rect_filled(Rect::from_min_max(rail.min, pos2(knob.x, rail.bottom())), 3.0, argb(PROGRESS));
        ui.painter().circle_filled(knob, 6.5, argb(pc::ACTIVE));
        let progress = ui.interact(timeline, ui.scope_id().with("replay timeline"), Sense::click_and_drag());
        if (progress.clicked() || progress.drag_started() || progress.dragged())
            && let Some(p) = progress.interact_pointer_pos()
        {
            self.timeline_scrubbing = true;
            self.seek(self.timeline_frame(p.x, rail));
            ui.ctx().request_repaint();
        }
        if progress.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
            self.timeline_scrubbing = false;
        }

        let center_y = bottom.top() + 80.0;
        let action = |ui: &egui::Ui, id: &str, label: &str, x: f32, width: f32| {
            let rect = Rect::from_center_size(pos2(x, center_y), vec2(width, 34.0));
            let response = ui.interact(rect, ui.scope_id().with(id), Sense::click());
            if response.hovered() {
                ui.painter().rect_filled(rect, 8.0, argb(pc::ACTIVE).gamma_multiply(0.24));
            }
            text(ui.painter(), rect.center(), Align2::CENTER_CENTER, label, 14.0, Weight::SemiBold, argb(pc::BAR_TEXT));
            response.clicked()
        };
        if action(ui, "replay backward", "-5s", bottom.left() + 44.0, 55.0) {
            self.jump_seconds(-5);
        }
        if action(ui, "replay toggle", if self.playing { "Pause" } else { "Play" }, bottom.left() + 120.0, 80.0) {
            self.toggle_playback();
        }
        if action(ui, "replay forward", "+5s", bottom.left() + 196.0, 55.0) {
            self.jump_seconds(5);
        }

        let duration = self.count.saturating_sub(1) / TIME_LAPSE_FPS;
        let current = self.index / TIME_LAPSE_FPS;
        let clock = format!("{} / {}", playback_clock(current), playback_clock(duration));
        if bottom.width() >= 680.0 {
            text(
                ui.painter(),
                pos2(bottom.left() + 248.0, center_y),
                Align2::LEFT_CENTER,
                &clock,
                12.5,
                Weight::Medium,
                argb(EMPTY_INK),
            );
        }
        // Keep the transport functional in small windows: a single cycling speed button
        // replaces the five choices without overlapping the seek controls.
        let narrow = bottom.width() < 540.0;
        let speed_width = 44.0;
        let speeds: &[u32] = if narrow { &[1] } else { &[1, 2, 4, 8, 16] };
        for (n, &speed) in speeds.iter().enumerate() {
            let x = bottom.right() - 24.0 - (speeds.len() - 1 - n) as f32 * (speed_width + 4.0) - speed_width / 2.0;
            let rect = Rect::from_center_size(pos2(x, center_y), vec2(speed_width, 34.0));
            let response = ui
                .interact(rect, ui.scope_id().with(("replay speed", speed)), Sense::click())
                .on_hover_text(if narrow { "Click to change playback speed" } else { "Playback speed" });
            let display_speed = if narrow { self.speed } else { speed };
            let selected = self.speed == speed;
            if selected || narrow || response.hovered() {
                ui.painter().rect_filled(rect, 8.0, if selected { argb(pc::ACTIVE) } else { argb(panel::DOCK_EDGE) });
            }
            text(
                ui.painter(),
                rect.center(),
                Align2::CENTER_CENTER,
                &format!("{display_speed}x"),
                13.0,
                Weight::SemiBold,
                if selected || narrow { argb(pc::ACCENT_INK) } else { argb(pc::BAR_TEXT) },
            );
            if response.clicked() {
                self.speed = if narrow {
                    match self.speed {
                        1 => 2,
                        2 => 4,
                        4 => 8,
                        8 => 16,
                        _ => 1,
                    }
                } else {
                    speed
                };
                self.stepped = Instant::now();
            }
        }
    }
}

fn playback_clock(seconds: u32) -> String {
    if seconds >= 3600 {
        format!("{:02}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

/// Request only the frames that will actually be displayed at the chosen speed.
/// The background reader still decodes video internally but does not send discarded RGBA frames
/// to the UI; each window uses at most four seconds' worth of output images.
fn playback_window(start: u32, count: u32, speed: u32) -> Vec<u32> {
    let speed = speed.clamp(1, 16);
    (0..TIME_LAPSE_FPS * 4).map(|i| start.saturating_add(i.saturating_mul(speed))).take_while(|&i| i < count).collect()
}

/// What a player with no frame to show says: the encoder's own words when it isn't there, else
/// that these frames can't be read here.
fn problem_message(problem: Option<&str>) -> &'static str {
    if problem == Some(crate::video::NO_FFMPEG) {
        crate::video::NO_FFMPEG
    } else {
        "The time-lapse's frames can't be read here."
    }
}

/// A message over the blank grid, wrapped to the window and on a panel of its own so the grid
/// doesn't read through it.
fn unavailable_note(ui: &egui::Ui, screen: Rect, message: &str) {
    let (width, ink) = ((screen.width() - 160.0).max(160.0), argb(pc::TEXT));
    let galley = ui.painter().layout(message.to_owned(), Weight::Regular.font(16.0), ink, width);
    let panel = Rect::from_center_size(screen.center(), galley.size() + vec2(36.0, 28.0));
    ui.painter().rect_filled(panel, 10.0, argb(REPLAY_FILL));
    ui.painter().galley(panel.center() - galley.size() / 2.0, galley, ink);
}

/// The surround's grid as the canvas draws it (1 1/3 pt lines every 16, none under the top bar),
/// for screens shown in the canvas's place.
fn surround_grid(ui: &egui::Ui, screen: Rect) {
    let (step, width, top) = (16.0, 4.0 / 3.0, screen.top() + 44.0);
    let origin = screen.min + vec2(0.5, 0.5);
    let mut x = origin.x;
    while x < screen.right() {
        let line = Rect::from_min_max(pos2(x - width / 2.0, top), pos2(x + width / 2.0, screen.bottom()));
        ui.painter().rect_filled(line, 0, argb(GRID_LINE));
        x += step;
    }
    let mut y = origin.y - ((origin.y - top) / step).floor() * step;
    while y < screen.bottom() {
        if y - width / 2.0 >= top {
            let line = Rect::from_min_max(pos2(screen.left(), y - width / 2.0), pos2(screen.right(), y + width / 2.0));
            ui.painter().rect_filled(line, 0, argb(GRID_LINE));
        }
        y += step;
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{Hash, Hasher};
    use std::path::{Path, PathBuf};

    use egui::{Event, Modifiers, RawInput, Rect, pos2, vec2};

    use super::*;
    use crate::video::decoder_test_guard;

    #[test]
    fn player_transport_preserves_pause_seek_and_speed() {
        let mut player = TimeLapsePlayer::new(None);
        player.count = TIME_LAPSE_FPS * 120;
        player.playing = true;
        player.speed = 16;
        player.advance(true, Duration::from_secs(1));
        assert_eq!(player.index, TIME_LAPSE_FPS * 16, "16x advances sixteen seconds of frames per real second");
        player.toggle_playback();
        assert!(!player.playing);
        player.advance(true, Duration::from_secs(10));
        assert_eq!(player.index, TIME_LAPSE_FPS * 16, "pause must not advance even at 16x");
        player.jump_seconds(-5);
        assert_eq!(player.index, TIME_LAPSE_FPS * 11);
        player.jump_seconds(5);
        assert_eq!(player.index, TIME_LAPSE_FPS * 16);
        player.seek(u32::MAX);
        assert_eq!(player.index, player.count - 1);
        player.toggle_playback();
        assert_eq!(player.index, 0, "play from the last frame restarts");
        assert!(player.playing);
    }

    #[test]
    fn playback_one_x_uses_elapsed_wall_time_and_keeps_fractional_frame_period() {
        let mut player = TimeLapsePlayer::new(None);
        player.count = 900;
        player.playing = true;
        let tick = Duration::from_secs_f64(1.0 / f64::from(TIME_LAPSE_FPS));

        player.advance(true, tick / 2);
        assert_eq!(player.index, 0);
        player.advance(true, Duration::from_secs(1));
        assert_eq!(player.index, TIME_LAPSE_FPS, "1x advances 30 recorded frames in one elapsed second");

        player.advance(true, tick + tick / 2);
        assert_eq!(player.index, TIME_LAPSE_FPS + 1);
        assert!(player.stepped.elapsed() >= tick / 2, "late repaint retains the fractional tick");

        player.advance(false, Duration::from_secs(2));
        assert_eq!(player.index, TIME_LAPSE_FPS + 1, "never outrun the unread frame");
        player.advance(true, Duration::from_secs(2));
        assert_eq!(player.index, TIME_LAPSE_FPS * 3 + 1, "recover elapsed time after decoding resumes");
    }

    #[test]
    fn playback_speed_and_last_frame_are_correct_after_a_delayed_repaint() {
        let mut player = TimeLapsePlayer::new(None);
        player.count = 101;
        player.playing = true;
        player.speed = 4;
        player.advance(true, Duration::from_millis(250));
        assert_eq!(player.index, 28, "a 250 ms repaint spans seven complete ticks at 4x");
        player.advance(true, Duration::from_secs(1));
        assert_eq!(player.index, 100);
        assert!(!player.playing, "stop on the last frame without overflowing its index");
    }

    #[test]
    fn timeline_seek_clamps_and_clock_formats_long_recordings() {
        let mut player = TimeLapsePlayer::new(None);
        player.count = 101;
        let bar = Rect::from_min_size(pos2(20.0, 20.0), vec2(400.0, 15.0));
        assert_eq!(player.timeline_frame(-100.0, bar), 0);
        assert_eq!(player.timeline_frame(220.0, bar), 50);
        assert_eq!(player.timeline_frame(1000.0, bar), 100);
        player.count = 0;
        assert_eq!(player.timeline_frame(220.0, bar), 0);
        assert_eq!(playback_clock(65), "01:05");
        assert_eq!(playback_clock(3601), "01:00:01");
    }

    #[test]
    fn accelerated_reader_only_requests_displayed_frames_and_stays_bounded() {
        assert_eq!(playback_window(5, 100, 1), (5..100).collect::<Vec<_>>());
        assert_eq!(playback_window(5, 100, 4), (5..100).step_by(4).collect::<Vec<_>>());
        assert_eq!(playback_window(5, 100, 16), vec![5, 21, 37, 53, 69, 85]);
        assert_eq!(playback_window(100, 100, 16), Vec::<u32>::new());
        assert_eq!(playback_window(0, 1_000_000, 16).len(), (TIME_LAPSE_FPS * 4) as usize);
    }

    #[test]
    fn clickable_timeline_and_transport_act_without_closing_the_player() {
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut player = TimeLapsePlayer::new(None);
        player.count = 300;
        player.playing = false;
        let mut time = 0.0;
        // The very first egui frame registers widgets; input hit-testing uses their
        // positions from the preceding frame, just as the gallery click tests do.
        frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
        frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
        let mut click = |at: egui::Pos2, player: &mut TimeLapsePlayer| {
            let mut closed = frame(player, &ctx, &mut look, vec![Event::PointerMoved(at)], &mut time);
            closed |= frame(
                player,
                &ctx,
                &mut look,
                vec![Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }],
                &mut time,
            );
            closed |= frame(
                player,
                &ctx,
                &mut look,
                vec![Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
                &mut time,
            );
            assert!(!closed, "the transport click closed the replay");
        };
        // At 800 × 600 the timeline runs from x=24 to 776, y=515.5.
        click(pos2(400.0, 515.5), &mut player);
        assert_eq!(player.index, 150, "timeline click should seek to its midpoint");
        click(pos2(120.0, 564.0), &mut player);
        assert!(player.playing, "Play should resume playback");
        click(pos2(584.0, 564.0), &mut player);
        assert_eq!(player.speed, 1, "the first speed remains selected");
        click(pos2(776.0 - 22.0, 564.0), &mut player);
        assert_eq!(player.speed, 16, "the last speed selects 16×");
    }

    /// A directory of its own for a test, under the system's temporary directory (the build sets
    /// it inside the HDD), made unique by the process and the clock.
    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("sumi-timelapse-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn has_ffmpeg_and_ffprobe() -> bool {
        ["ffmpeg", "ffprobe"].into_iter().all(|program| {
            std::process::Command::new(program)
                .arg("-version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    }

    /// A real on-disk MPEG4 clip. The decoder tests never silently fall back to JPEGs.
    #[cfg(unix)]
    fn recorded_video_fixture(root: &Path, frames: u8) -> Arc<TimeLapse> {
        let art = root.join("artwork");
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        for i in 0..frames {
            assert!(lapse.record([i.wrapping_mul(5), 55, 190, 255].repeat(16), 4, 4));
        }
        let plan = lapse.segment_plan().expect("segment plan for recorded frames");
        let status = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=c=red:s=128x128:r=30"])
            .arg("-frames:v")
            .arg(frames.to_string())
            .args(["-c:v", "mpeg4", "-y"])
            .arg(&plan.path)
            .status()
            .expect("ffmpeg is available");
        assert!(status.success(), "generate a real MPEG4 fixture");
        assert!(lapse.segment_made(plan, 128, 128, true).is_none());
        assert_eq!(lapse.clips().len(), 1, "fixture must use an MP4 rather than JPEG fallback");
        lapse
    }

    /// The names in a folder, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// A small JPEG at `path`, all one colour.
    fn write_jpeg(path: &Path) {
        write_jpeg_colour(path, [127, 127, 127]);
    }

    /// A small JPEG at `path` of one colour (its frames differ by it).
    fn write_jpeg_colour(path: &Path, colour: [u8; 3]) {
        let rgb: Vec<u8> = colour.iter().copied().cycle().take(8 * 8 * 3).collect();
        image::save_buffer_with_format(path, &rgb, 8, 8, image::ExtendedColorType::Rgb8, image::ImageFormat::Jpeg)
            .unwrap();
    }

    /// What the artwork folder holds: each file's path, size and a hash of its bytes.
    fn manifest(dir: &Path) -> Vec<(String, usize, u64)> {
        fn walk(base: &Path, at: &Path, out: &mut Vec<(String, usize, u64)>) {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(at).unwrap().map(|e| e.unwrap().path()).collect();
            paths.sort();
            for path in paths {
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut hash);
                    out.push((
                        path.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                        bytes.len(),
                        hash.finish(),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out
    }

    /// A time-lapse whose only video is not a video (the artwork changed without pictures after).
    fn unreadable_video(art: &Path, segment: bool) {
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        if segment {
            std::fs::write(
                lapse.join("segments.json"),
                r#"{"resolution":0,"quality":1,"hevc":false,"width":8,"height":8,"bitrate":1000000,"ends":[1]}"#,
            )
            .unwrap();
            std::fs::write(lapse.join("segment-1.mp4"), b"not a video").unwrap();
        } else {
            std::fs::write(lapse.join("frame-000000.jpg"), b"not a jpeg").unwrap();
        }
    }

    /// Runs one headless frame of the player; true when it was closed (Done or Escape).
    fn frame(
        player: &mut TimeLapsePlayer,
        ctx: &egui::Context,
        look: &mut Look,
        events: Vec<Event>,
        time: &mut f64,
    ) -> bool {
        *time += 1.0 / 60.0;
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            time: Some(*time),
            events,
            ..RawInput::default()
        };
        let mut closed = false;
        let mut out = ctx.run_ui(raw, |ui| closed = player.show(ui.ctx(), look));
        // the pictures' and fonts' uploads: these tests read what the player shows, not the textures
        out.textures_delta.clear();
        closed
    }

    #[test]
    fn seeking_back_to_a_frame_already_read_shows_it_again() {
        let root = scratch("seek-back");
        let art = root.join("artwork");
        let lapse_dir = art.join("timelapse");
        std::fs::create_dir_all(&lapse_dir).unwrap();
        write_jpeg_colour(&lapse_dir.join("frame-000000.jpg"), [255, 0, 0]);
        write_jpeg_colour(&lapse_dir.join("frame-000001.jpg"), [0, 0, 255]);
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        let (first, second) = (lapse.frame(0).expect("frame 0"), lapse.frame(1).expect("frame 1"));
        assert_ne!(first.rgba, second.rgba, "the fixture's frames differ");
        let mut player = TimeLapsePlayer::new(Some(Arc::clone(&lapse)));
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let mut shows = |player: &mut TimeLapsePlayer, want: u32| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                frame(player, &ctx, &mut look, Vec::new(), &mut time);
                if player.shown.as_ref().is_some_and(|(i, _)| *i == want) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            false
        };
        assert!(shows(&mut player, 0), "frame 0 shows first");
        player.stepped = Instant::now() - Duration::from_secs(1);
        assert!(shows(&mut player, 1), "playback moves to frame 1");
        // the drag is held back at the start: the index is 0 while frame 1 is still shown
        player.scrub = Some((0.0, 1));
        player.seek(0);
        assert!(shows(&mut player, 0), "seeking back to a frame already shown shows it again");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn consecutive_forward_scrubs_reuse_reader_and_cancel_before_replacement() {
        let root = scratch("forward-reader-lifetime");
        let art = root.join("artwork");
        let lapse_dir = art.join("timelapse");
        std::fs::create_dir_all(&lapse_dir).unwrap();
        for i in 0..150 {
            write_jpeg_colour(&lapse_dir.join(format!("frame-{i:06}.jpg")), [i as u8, 32, 240]);
        }
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        let mut player = TimeLapsePlayer::new(Some(Arc::clone(&lapse)));
        let metrics = Arc::clone(&player.reader_metrics);
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        player.scrub = Some((0.0, 0));

        let shows = |player: &mut TimeLapsePlayer, target: u32, look: &mut Look, time: &mut f64| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                frame(player, &ctx, look, Vec::new(), time);
                if player.shown.as_ref().is_some_and(|(i, _)| *i == target) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("frame {target} was not shown after seek");
        };

        shows(&mut player, 0, &mut look, &mut time);
        assert_eq!(metrics.started.load(Ordering::SeqCst), 1);
        let initial = player.reading_stop().expect("active reader");
        // Simulate a drag across the timeline with consecutive forward seeks ~150ms apart.
        // Even though frames are decoded ahead on a worker, these seeks all fit its window.
        for i in 1..=12 {
            std::thread::sleep(Duration::from_millis(150));
            player.seek(i);
            assert!(player.ahead.is_some(), "forward seek {i} discarded a usable reader");
            assert!(!initial.load(Ordering::SeqCst), "in-window forward seek {i} cancelled its reader");
            shows(&mut player, i, &mut look, &mut time);
            assert_eq!(metrics.started.load(Ordering::SeqCst), 1, "forward seek {i} created a new decoder");
        }

        // An out-of-window seek cancels the old worker, without creating a new one
        // until the previous worker is completely done and its decoder has been reaped.
        player.seek(140);
        assert!(initial.load(Ordering::SeqCst), "out-of-window seek did not cancel the reader");
        assert!(player.ahead.is_none());
        assert!(player.retiring.is_some(), "old reader must be tracked until its full shutdown");
        let deadline = Instant::now() + Duration::from_secs(3);
        while metrics.started.load(Ordering::SeqCst) == 1 && Instant::now() < deadline {
            frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
            assert!(metrics.peak.load(Ordering::SeqCst) <= 1);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(metrics.started.load(Ordering::SeqCst), 2, "replacement reader did not start");
        assert_eq!(metrics.peak.load(Ordering::SeqCst), 1, "two readers were alive at the same time");
        shows(&mut player, 140, &mut look, &mut time);
        drop(player);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sparse_speed_seek_restarts_for_unrequested_frame_then_reuses_aligned_frames() {
        let root = scratch("sparse-forward-scrub");
        let art = root.join("artwork");
        let dir = art.join("timelapse");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..48 {
            write_jpeg_colour(&dir.join(format!("frame-{i:06}.jpg")), [i as u8, 64, 200]);
        }
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        let mut player = TimeLapsePlayer::new(Some(lapse));
        player.speed = 16;
        player.playing = false;
        player.scrub = Some((0.0, 0));
        let metrics = Arc::clone(&player.reader_metrics);
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let shows = |player: &mut TimeLapsePlayer, target: u32, look: &mut Look, time: &mut f64| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                frame(player, &ctx, look, Vec::new(), time);
                if player.shown.as_ref().is_some_and(|(i, _)| *i == target) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            panic!("frame {target} was never displayed");
        };
        shows(&mut player, 0, &mut look, &mut time);
        let initial = player.reading_stop().unwrap();
        player.seek(3);
        assert!(initial.load(Ordering::SeqCst), "3 is not in the 0,16,32,... decoder window");
        shows(&mut player, 3, &mut look, &mut time);
        assert_eq!(metrics.started.load(Ordering::SeqCst), 2);
        let second = player.reading_stop().unwrap();
        player.seek(19);
        assert!(!second.load(Ordering::SeqCst), "19 belongs to the 3,19,35,... decoder window");
        shows(&mut player, 19, &mut look, &mut time);
        assert_eq!(metrics.started.load(Ordering::SeqCst), 2);
        assert_eq!(metrics.peak.load(Ordering::SeqCst), 1);
        drop(player);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn consecutive_video_scrubs_reap_ffmpeg_before_starting_the_next_reader() {
        let _decoder_test = decoder_test_guard();
        if !has_ffmpeg_and_ffprobe() {
            eprintln!("SKIP real FFmpeg seek regression: ffmpeg or ffprobe unavailable on PATH");
            return;
        }
        let root = scratch("real-video-decoder-lifetime");
        let lapse = recorded_video_fixture(&root, 6);

        let mut player = TimeLapsePlayer::new(Some(lapse));
        let metrics = Arc::clone(&player.reader_metrics);
        player.playing = false;
        player.scrub = Some((0.0, 0));
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let shows = |player: &mut TimeLapsePlayer, target: u32, look: &mut Look, time: &mut f64| {
            let deadline = Instant::now() + Duration::from_secs(6);
            while Instant::now() < deadline {
                frame(player, &ctx, look, Vec::new(), time);
                if player.shown.as_ref().is_some_and(|(i, _)| *i == target) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("video frame {target} not shown after seek");
        };
        shows(&mut player, 0, &mut look, &mut time);
        for i in 1..6 {
            player.seek(i);
            shows(&mut player, i, &mut look, &mut time);
        }
        assert_eq!(metrics.started.load(Ordering::SeqCst), 1, "forward seeks reused decoder");
        let initial = player.reading_stop().unwrap();
        player.seek(0);
        assert!(initial.load(Ordering::SeqCst));
        shows(&mut player, 0, &mut look, &mut time);
        assert_eq!(metrics.started.load(Ordering::SeqCst), 2, "backward seek starts a new decoder");
        assert_eq!(metrics.peak.load(Ordering::SeqCst), 1, "two video readers overlapped");
        drop(player);
        let deadline = Instant::now() + Duration::from_secs(5);
        while metrics.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(metrics.active.load(Ordering::SeqCst), 0, "a cancelled decoder must finish before the next test");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn two_active_replays_with_slow_decoder_startup_both_advance_without_preemption() {
        let _decoder_test = decoder_test_guard();
        if !has_ffmpeg_and_ffprobe() {
            eprintln!("SKIP concurrent replay startup: ffmpeg or ffprobe unavailable on PATH");
            return;
        }
        let root = scratch("two-slow-video-replays");
        let lapse = recorded_video_fixture(&root, 24);
        let mut first = TimeLapsePlayer::new(Some(Arc::clone(&lapse)));
        let mut second = TimeLapsePlayer::new(Some(lapse));
        // Slow startup happens AFTER the first replay acquires the exclusive permit.
        // Both players are playing, not paused. Their redraws must not cancel either
        // replay merely because the other is still waiting for ffprobe/ffmpeg.
        first.reader_metrics.simulated_startup_ms.store(350, Ordering::SeqCst);
        second.reader_metrics.simulated_startup_ms.store(350, Ordering::SeqCst);
        let (first_ctx, second_ctx) = (egui::Context::default(), egui::Context::default());
        let (mut first_look, mut second_look) = (Look::default(), Look::default());
        crate::look::install_fonts(&first_ctx);
        crate::look::install_fonts(&second_ctx);
        let (mut first_time, mut second_time) = (0.0, 0.0);
        frame(&mut first, &first_ctx, &mut first_look, Vec::new(), &mut first_time);
        let first_stop = first.reading_stop().expect("the first video reader starts");
        let deadline = Instant::now() + Duration::from_secs(8);
        while !crate::video::is_active_decoder_for_test(&first_stop) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(crate::video::is_active_decoder_for_test(&first_stop), "the first reader owns decoder during startup");
        frame(&mut second, &second_ctx, &mut second_look, Vec::new(), &mut second_time);
        let second_stop = second.reading_stop().expect("the second video reader starts");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::video::is_waiting_decoder_for_test(&second_stop) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(crate::video::is_waiting_decoder_for_test(&second_stop), "second decoder must queue behind the first");
        // Multiple redraw cycles while the first ffmpeg has not even started.
        for _ in 0..6 {
            frame(&mut first, &first_ctx, &mut first_look, Vec::new(), &mut first_time);
            frame(&mut second, &second_ctx, &mut second_look, Vec::new(), &mut second_time);
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(!first_stop.load(Ordering::Acquire), "another active Replay preempted the first during startup");
        assert!(!second_stop.load(Ordering::Acquire), "a queued Replay was cancelled during startup");

        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            frame(&mut first, &first_ctx, &mut first_look, Vec::new(), &mut first_time);
            frame(&mut second, &second_ctx, &mut second_look, Vec::new(), &mut second_time);
            if first.shown.as_ref().is_some_and(|(i, _)| *i >= 2) && second.shown.as_ref().is_some_and(|(i, _)| *i >= 2)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(6));
        }
        assert!(first.shown.as_ref().is_some_and(|(i, _)| *i >= 2), "first active Replay did not advance");
        assert!(second.shown.as_ref().is_some_and(|(i, _)| *i >= 2), "second active Replay did not advance");
        assert_eq!(first.reader_metrics.started.load(Ordering::SeqCst), 1, "first Replay was needlessly restarted");
        assert_eq!(second.reader_metrics.started.load(Ordering::SeqCst), 1, "second Replay was needlessly restarted");
        let first_metrics = Arc::clone(&first.reader_metrics);
        let second_metrics = Arc::clone(&second.reader_metrics);
        drop((first, second));
        let deadline = Instant::now() + Duration::from_secs(5);
        while (first_metrics.active.load(Ordering::SeqCst) > 0 || second_metrics.active.load(Ordering::SeqCst) > 0)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(first_metrics.active.load(Ordering::SeqCst), 0, "first decoder worker was not reaped");
        assert_eq!(second_metrics.active.load(Ordering::SeqCst), 0, "second decoder worker was not reaped");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_full_queue_marks_stall_until_a_consumer_makes_progress() {
        // The stall detector has its own test: unlike an FFmpeg fixture it cannot
        // be preempted by unrelated Required readers in parallel tests.
        let (tx, rx) = sync_channel(1);
        let frame = || ThumbnailImage { width: 1, height: 1, rgba: vec![10, 20, 30, 255] };
        tx.try_send((0, frame())).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stalled = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(ReadState::default()));
        let (worker_stop, worker_stalled, worker_state) = (Arc::clone(&stop), Arc::clone(&stalled), Arc::clone(&state));
        let started = Instant::now();
        let worker =
            std::thread::spawn(move || deliver_frame(&tx, &worker_stop, &worker_stalled, &worker_state, (1, frame())));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !stalled.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(stalled.load(Ordering::Acquire), "a persistently full queue must eventually yield");
        assert!(started.elapsed() >= Duration::from_millis(200), "a transient full queue is not a stall");
        assert!(state.lock().unwrap().hit_full_queue);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap().0, 0);
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap().0, 1);
        assert!(worker.join().unwrap(), "the pending frame must be delivered in full");
        assert!(!stalled.load(Ordering::Acquire), "resuming consumption clears the stall signal");
    }

    #[cfg(unix)]
    #[test]
    fn paused_replay_with_full_frame_channel_allows_export_and_cancellation() {
        let _decoder_test = decoder_test_guard();
        if !has_ffmpeg_and_ffprobe() {
            eprintln!("SKIP full-queue decoder regression: ffmpeg or ffprobe unavailable on PATH");
            return;
        }
        let root = scratch("paused-video-export");
        let lapse = recorded_video_fixture(&root, 48);
        // The preview receives only its first frame. Its producer fills the remaining
        // four slots, then must wait for space without holding the global decoder mutex.
        let paused = read_in_order(Arc::clone(&lapse), (0..48).collect(), ReadPurpose::Replay, None, None);
        assert_eq!(paused.frames.recv_timeout(Duration::from_secs(5)).expect("preview frame").0, 0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !paused.state().hit_full_queue && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(paused.state().hit_full_queue, "the paused preview did not fill its bounded queue");
        assert!(!paused.state().finished, "the paused reader must still be holding its decoded frames");

        // An export may request the decoder while that replay has a full queue. It must
        // preempt the preview, reap its ffmpeg child, and begin producing without the
        // user ever unpausing or draining the preview channel.
        let paused_stop = Arc::clone(&paused._stops.0);
        let export = read_in_order(Arc::clone(&lapse), (0..48).collect(), ReadPurpose::Export, None, None);
        assert_eq!(
            export.frames.recv_timeout(Duration::from_secs(5)).expect("export starts despite paused preview").0,
            0
        );
        assert!(paused_stop.load(Ordering::Acquire), "a competing export must cancel a stalled replay");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !paused.state().finished && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(paused.state().finished, "preempted replay did not finish/reap its decoder");

        // Cancel the export itself with its own queue full, then ensure a fresh replay
        // can start: a blocked producer must not hold an unreleasable decoder permit.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !export.state().hit_full_queue && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(export.state().hit_full_queue, "export did not reach the full-channel case");
        let export_state = Arc::clone(&export.state);
        drop(export);
        let next = read_in_order(Arc::clone(&lapse), vec![0], ReadPurpose::Replay, None, None);
        assert_eq!(
            next.frames.recv_timeout(Duration::from_secs(5)).expect("a new reader starts after export cancellation").0,
            0
        );
        assert!(
            export_state.lock().unwrap().finished,
            "cancelled export must fully stop before another decoder starts"
        );
        let next_state = Arc::clone(&next.state);
        drop(next);
        drop(paused);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !next_state.lock().unwrap().finished && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(next_state.lock().unwrap().finished, "the final replay must reap before releasing the test guard");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_export_card_while_required_decoder_is_held_stops_all_workers() {
        let _decoder_test = decoder_test_guard();
        let root = scratch("cancel-required-permit-from-card");
        let art = root.join("artwork");
        // Deliberately unreadable video: the first frame would fail if a decoder ever
        // started. This test must cancel while still waiting for Required ownership,
        // without relying on ffmpeg availability or any video callback.
        unreadable_video(&art, true);
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        assert_eq!(lapse.count(), 1);
        let held = crate::video::hold_required_decoder_for_test();

        for attempt in 0..4 {
            let mut ui = TimeLapseUi::default();
            ui.lapse = Some((art.to_string_lossy().into_owned(), Arc::clone(&lapse)));
            let metrics = Arc::clone(&ui.export_reader_metrics);
            assert!(ui.export(None, &format!("cancel-required-{attempt}")).is_none());
            assert!(ui.card.busy(), "a real export card is open");
            let (done, card_exporting) = ui.exporting.as_ref().expect("real UI export handle");
            let this_reader = card_exporting.cancellation_flag();

            let deadline = Instant::now() + Duration::from_secs(3);
            while !crate::video::is_waiting_decoder_for_test(&this_reader) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(
                crate::video::is_waiting_decoder_for_test(&this_reader),
                "this export worker did not wait for Required permit"
            );
            assert!(matches!(done.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
            assert_eq!(metrics.active.load(Ordering::SeqCst), 1, "worker is alive and waiting");

            // This is the same method used by the card's Cancel click, not drop(Reading).
            card_exporting.cancel();
            let finished = done.recv_timeout(Duration::from_secs(2)).expect("cancelled export must finish promptly");
            assert!(finished.is_err(), "a cancelled export must not publish a video");
            let deadline = Instant::now() + Duration::from_secs(2);
            while metrics.active.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(metrics.started.load(Ordering::SeqCst), 1, "one export creates one frame reader");
            assert_eq!(metrics.active.load(Ordering::SeqCst), 0, "no reader worker may survive card cancellation");
            assert_eq!(metrics.peak.load(Ordering::SeqCst), 1, "no additional workers accumulated");
            assert!(
                !crate::video::is_waiting_decoder_for_test(&this_reader),
                "this cancelled Required wait must be removed, independently of other parallel tests"
            );
        }

        drop(held);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finishing_a_frame_reader_does_not_cancel_the_export_card() {
        let root = scratch("successful-export-stop-is-not-cancel");
        let art = root.join("artwork");
        let dir = art.join("timelapse");
        std::fs::create_dir_all(&dir).unwrap();
        write_jpeg_colour(&dir.join("frame-000000.jpg"), [64, 128, 192]);
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        let mut card = crate::export_card::ExportCard::default();
        let exporting = card.start(|| 0.0);
        let reading = read_in_order(lapse, vec![0], ReadPurpose::Export, Some(exporting.cancellation_flag()), None);
        assert_eq!(reading.frames.recv_timeout(Duration::from_secs(2)).unwrap().0, 0);
        drop(reading);
        assert!(!exporting.cancelled(), "finishing/dropping the reader must not discard a completed video");
        exporting.ended(true);
        assert!(!exporting.cancelled(), "successful export remains successful after its reader stops");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_empty_replay_says_nothing_was_recorded_and_escape_closes_it() {
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let mut player = TimeLapsePlayer::new(None);
        assert_eq!(player.message(), Some("Nothing recorded yet"));
        frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
        let escape = Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert!(frame(&mut player, &ctx, &mut look, vec![escape], &mut time), "Escape closes the replay");
    }

    #[test]
    fn a_replay_whose_video_cannot_be_read_says_so_instead_of_staying_blank() {
        let _decoder_test = decoder_test_guard();
        let root = scratch("unreadable-video");
        let art = root.join("artwork");
        unreadable_video(&art, true);
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let mut player = TimeLapsePlayer::for_gallery(&art.to_string_lossy());
        assert_eq!(player.count, 1, "the video notes a frame");
        // A parallel Required import/export may own the global decoder or preempt
        // this Replay before its broken clip is opened. Drive redraws until the UI
        // can retry and report the actual unreadable video, rather than assuming
        // the very next redraw always completes the decoder.
        let deadline = Instant::now() + Duration::from_secs(10);
        while player.message().is_none() && Instant::now() < deadline {
            _ = frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
            std::thread::sleep(Duration::from_millis(5));
        }
        let said =
            player.message().expect("unreadable video must eventually report an error, even with other decoders");
        assert!(!player.showing(), "an unreadable video must never produce a frame");
        assert!(said == crate::video::NO_FFMPEG || said.contains("can't be read"), "{said}");
        let escape = Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert!(frame(&mut player, &ctx, &mut look, vec![escape], &mut time));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_unreadable_time_lapse_is_left_as_it_was() {
        let root = scratch("unreadable-pictures");
        let art = root.join("artwork");
        unreadable_video(&art, false);
        let before = manifest(&art);
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let mut player = TimeLapsePlayer::for_gallery(&art.to_string_lossy());
        for _ in 0..300 {
            frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
            if player.message().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(player.message().is_some(), "a replay with no frame says why");
        drop(player);
        assert_eq!(before, manifest(&art), "replaying left the artwork as it was");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_onto_the_made_video_leaves_it_whole() {
        // The save dialog names the file the export made (in the temporary folder it was made
        // in), so it can be pointed back at that file: delivering must not empty it.
        let root = scratch("save-onto-itself");
        let made = root.join("artwork.mp4");
        std::fs::write(&made, b"made video").unwrap();
        save_to(&made, &made).unwrap();
        assert_eq!(std::fs::read(&made).unwrap(), b"made video", "the made video was emptied");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_to_owned_temporary_video_preserves_the_selected_destination_after_drop() {
        let made = crate::video::export_temp_path("same-path-timelapse", "mp4");
        std::fs::create_dir_all(made.parent().unwrap()).unwrap();
        std::fs::write(&made, b"user selected this exact temporary video").unwrap();
        {
            let mut owned = crate::video::OwnedExport::new(made.clone());
            save_to(&owned.path, &made).unwrap();
            owned.keep_if_saved_here(&made);
        }
        assert_eq!(std::fs::read(&made).unwrap(), b"user selected this exact temporary video");
        crate::video::discard(&made);
    }

    #[test]
    fn saving_leaves_an_unrelated_part_file_alone() {
        let root = scratch("save-beside-part");
        let (made, to) = (root.join("new.mp4"), root.join("artwork.mp4"));
        std::fs::write(&made, b"new video").unwrap();
        std::fs::write(&to, b"old video").unwrap();
        let part = root.join("artwork.mp4.part");
        std::fs::write(&part, b"someone else's").unwrap();
        save_to(&made, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"new video", "the video was not put in place");
        assert_eq!(std::fs::read(&part).unwrap(), b"someone else's", "a file beside it was overwritten");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_a_long_legal_name_still_saves() {
        // A destination name up to the filesystem's limit is legal; the staging file beside it
        // must not be built from that name (which would make the staging name longer still).
        let root = scratch("save-long-name");
        let made = root.join("made.mp4");
        std::fs::write(&made, b"made video").unwrap();
        let to = root.join(format!("{}.mp4", "a".repeat(250)));
        assert!(to.file_name().unwrap().len() <= 255, "the fixture's name is legal");
        save_to(&made, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"made video", "the video was not saved");
        assert_eq!(std::fs::read(&made).unwrap(), b"made video", "the made video changed");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_a_missing_video_keeps_the_old_file_and_leaves_nothing_behind() {
        let root = scratch("save-missing-made");
        let (to, missing) = (root.join("artwork.mp4"), root.join("gone.mp4"));
        std::fs::write(&to, b"old video").unwrap();
        assert!(save_to(&missing, &to).is_err());
        assert_eq!(std::fs::read(&to).unwrap(), b"old video", "the old video was changed");
        assert_eq!(names(&root), vec!["artwork.mp4".to_owned()], "the save left a file of its own behind");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn saving_skips_a_staging_file_it_does_not_own() {
        let root = scratch("save-staging-collision");
        let (made, to) = (root.join("made.mp4"), root.join("artwork.mp4"));
        std::fs::write(&made, b"made video").unwrap();
        std::fs::write(&to, b"old video").unwrap();
        let theirs = save_staging(&to, 0);
        std::fs::write(&theirs, b"someone else's").unwrap();
        save_to(&made, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"made video", "the video was not saved");
        assert_eq!(std::fs::read(&made).unwrap(), b"made video", "the made video changed");
        assert_eq!(std::fs::read(&theirs).unwrap(), b"someone else's", "a file it did not own was overwritten");
        let mut want = vec![
            "artwork.mp4".to_owned(),
            "made.mp4".to_owned(),
            theirs.file_name().unwrap().to_string_lossy().into_owned(),
        ];
        want.sort();
        assert_eq!(names(&root), want, "a staging file of its own was left behind");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replaying_pictures_reads_them_without_changing_the_artwork() {
        let root = scratch("read-only");
        let art = root.join("artwork");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        for i in 0..3 {
            write_jpeg(&lapse.join(format!("frame-{i:06}.jpg")));
        }
        let before = manifest(&art);
        let artwork = art.to_string_lossy().into_owned();
        assert_eq!(recorded(&artwork), Recorded::Yes, "the pictures are a time-lapse");
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let mut player = TimeLapsePlayer::for_gallery(&artwork);
        for _ in 0..300 {
            frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
            if player.shown.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(player.shown.is_some(), "a frame was read");
        drop(player);
        assert_eq!(before, manifest(&art), "replaying left the artwork as it was");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_time_lapse_being_packaged_is_not_read_until_packaging_ends() {
        let root = scratch("packaging");
        let art = root.join("artwork");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        write_jpeg(&lapse.join("frame-000000.jpg"));
        let artwork = art.to_string_lossy().into_owned();
        let packaging = hold_packaging(&artwork);
        assert_eq!(recorded(&artwork), Recorded::Packaging);
        drop(packaging);
        assert_eq!(recorded(&artwork), Recorded::Yes);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dropping_a_replay_stops_the_frames_being_read() {
        let root = scratch("cancelled");
        let art = root.join("artwork");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        // enough frames that the reader is still at it when the replay goes
        for i in 0..200 {
            write_jpeg(&lapse.join(format!("frame-{i:06}.jpg")));
        }
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        let mut player = TimeLapsePlayer::for_gallery(&art.to_string_lossy());
        frame(&mut player, &ctx, &mut look, Vec::new(), &mut time);
        let stop = player.reading_stop().expect("the replay reads its frames ahead");
        assert!(!stop.load(Ordering::Relaxed));
        drop(player);
        assert!(stop.load(Ordering::Relaxed), "dropping the replay stops its reader");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_editor_s_replay_still_plays_the_artwork_it_opened() {
        let root = scratch("editor-replay");
        let art = root.join("artwork");
        let lapse = art.join("timelapse");
        std::fs::create_dir_all(&lapse).unwrap();
        for i in 0..3 {
            write_jpeg(&lapse.join(format!("frame-{i:06}.jpg")));
        }
        let dir = art.to_string_lossy().into_owned();
        let mut lapse_ui = TimeLapseUi::default();
        lapse_ui.lapse = Some((dir.clone(), TimeLapse::new(dir)));
        lapse_ui.replay();
        assert!(lapse_ui.replaying());
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        crate::look::install_fonts(&ctx);
        let mut time = 0.0;
        for _ in 0..60 {
            time += 1.0 / 60.0;
            let raw = RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                time: Some(time),
                ..RawInput::default()
            };
            let _ = ctx.run_ui(raw, |ui| _ = lapse_ui.show(ui.ctx(), &mut look, "Artwork")).textures_delta.clear();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(lapse_ui.replaying(), "the editor's replay is still up");
        let escape = Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
            time: Some(time + 1.0 / 60.0),
            events: vec![escape],
            ..RawInput::default()
        };
        let _ = ctx.run_ui(raw, |ui| _ = lapse_ui.show(ui.ctx(), &mut look, "Artwork")).textures_delta.clear();
        assert!(!lapse_ui.replaying(), "Escape closes the editor's replay too");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    #[ignore = "requires external FFmpeg/FFprobe for the real export roundtrip"]
    fn a_time_lapse_export_thread_makes_a_playable_mp4() {
        let _decoder_test = decoder_test_guard();
        // The real UI export thread and card, with the system's encoder: no mocked writer, no
        // dialog, and no skip path (a missing encoder must fail this test, not pass it).
        let root = scratch("native-export");
        let art = root.join("artwork");
        std::fs::create_dir_all(&art).unwrap();
        let lapse = TimeLapse::new(art.to_string_lossy().into_owned());
        // The UI records at the settings' side (1920 for 1080p); these source frames stay small.
        lapse.set_settings(sumi_core::TimeLapseSettings { resolution: 0, quality: 0, hevc: false });
        let side = time_lapse_side(lapse.settings());
        assert_eq!(side, 1920, "the default video side is 1080p");
        // The writer fits a picture within `side` but never upscales it: 64 x 48 stays 64 x 48.
        assert_eq!(crate::video::fit(64, 48, side, |_, _| true), Some((64, 48)), "fit keeps a small source");
        let colours = [[255u8, 0, 0], [0, 255, 0], [0, 0, 255]];
        for colour in colours {
            let rgba: Vec<u8> = colour.iter().copied().chain(std::iter::once(255)).cycle().take(64 * 48 * 4).collect();
            assert!(lapse.record(rgba, 64, 48), "native frame recorded");
        }
        assert_eq!(lapse.count(), 3, "three distinct frames are stored");
        for i in 0..3 {
            let stored = lapse.frame(i).expect("the picture is stored");
            assert_eq!((stored.width, stored.height), (64, 48), "frame {i} is stored at the source size");
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let name = format!("proof-native-export-{}-{nanos}", std::process::id());
        let mut ui = TimeLapseUi::default();
        ui.lapse = Some((art.to_string_lossy().into_owned(), Arc::clone(&lapse)));
        assert!(ui.export(None, &name).is_none(), "the export starts");
        assert!(ui.card.busy(), "the export card is up while the video is made");
        let (rx, exporting) = ui.exporting.as_ref().expect("an export is waiting");
        assert!(!exporting.cancelled(), "the export was not cancelled");
        let made =
            rx.recv_timeout(Duration::from_secs(120)).expect("the export thread ends").expect("the MP4 was made");
        assert!(made.exists(), "the export wrote {}", made.display());
        let exports = std::env::temp_dir().join("sumi-exports");
        let folder = made.parent().and_then(Path::parent);
        assert_eq!(folder, Some(exports.as_path()), "the export wrote to the app's export folder");
        assert_eq!(made.file_name().and_then(|n| n.to_str()), Some(format!("{name}.mp4").as_str()));
        let mut frames = Vec::new();
        let count = crate::video::read_frames(&made, |rgba, width, height, at| {
            let pixels = rgba.as_chunks::<4>().0;
            let samples = [(32, 24), (8, 6), (56, 6), (8, 42), (56, 42)]
                .map(|(x, y)| pixels.get(y as usize * width as usize + x as usize).copied().unwrap_or([0; 4]));
            frames.push((width, height, at, samples));
            true
        })
        .expect("the made MP4 is readable by the actual decoder");
        assert_eq!(count, 3, "all frames read back");
        assert_eq!(frames.len(), 3);
        for (i, (width, height, at, _)) in frames.iter().enumerate() {
            assert_eq!((*width, *height), (64, 48), "frame {i} keeps the source size, within the {side} side");
            assert!((*at - i as f64 / 30.0).abs() < 1e-3, "frame {i} at {at}");
        }
        for (i, (_, _, _, samples)) in frames.iter().enumerate() {
            let want = colours[i];
            for pixel in samples {
                assert!(
                    (0..3).all(|c| (i32::from(pixel[c]) - i32::from(want[c])).abs() <= 12),
                    "frame {i}: {pixel:?}, want {want:?}"
                );
            }
        }
        // The three pictures are distinct, not one frame read back three times.
        for a in 0..3 {
            for b in a + 1..3 {
                let (first, second) = (frames[a].3[0], frames[b].3[0]);
                assert!(
                    (0..3).any(|c| (i32::from(first[c]) - i32::from(second[c])).abs() > 100),
                    "frames {a} and {b} differ: {first:?} vs {second:?}"
                );
            }
        }
        crate::video::discard(&made);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A real package's time-lapse (its path in SUMI_PACKAGE) taken in as its videos, then
    /// frames read back from them in order.
    #[test]
    #[ignore = "needs a .procreate with a time-lapse, and ffmpeg"]
    fn a_packages_videos_come_in_as_they_are_and_play_back() {
        let _decoder_test = decoder_test_guard();
        let Ok(package) = std::env::var("SUMI_PACKAGE") else { return };
        let dir = std::env::temp_dir().join(format!("sumi-take-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let artwork = dir.to_string_lossy().into_owned();
        let started = Instant::now();
        take_package(std::path::Path::new(&package), &artwork);
        let lapse = TimeLapse::new(artwork);
        let clips = lapse.clips();
        eprintln!(
            "{} frames in {} videos, {} MB, in {:?}",
            lapse.count(),
            clips.len(),
            lapse.bytes() >> 20,
            started.elapsed()
        );
        assert!(!clips.is_empty());
        assert_eq!(lapse.count(), clips.last().map_or(0, |c| c.first + c.frames));
        // across the first video's end, and one far on
        let first = clips[0].frames;
        let wanted = vec![0, first - 1, first, first + 1, lapse.count() - 1];
        let started = Instant::now();
        let got: Vec<(u32, u32, u32)> = read_in_order(
            Arc::clone(&lapse),
            wanted.clone(),
            ReadPurpose::Replay,
            None,
            Some(Arc::new(ReaderMetrics::default())),
        )
        .frames
        .iter()
        .map(|(i, f)| (i, f.width, f.height))
        .collect();
        eprintln!("{got:?} in {:?}", started.elapsed());
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), wanted);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
