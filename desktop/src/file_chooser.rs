//! Linux file dialogs drawn inside Sumi, independent of a session bus, portal or Zenity.
//! Requests can originate in nested panels; the root egui frame renders one modal at
//! a time and reports a result to the requester on the next frame.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use egui::Context;

use crate::gallery_file_picker::{Choice, Picker, Selection};

pub type Response = Receiver<Option<Vec<PathBuf>>>;

struct Request {
    picker: Picker,
    response: Option<Sender<Option<Vec<PathBuf>>>>,
    callback: Option<Box<dyn FnOnce(Option<Vec<PathBuf>>)>>,
}

#[derive(Default)]
struct Hub {
    pending: VecDeque<Request>,
    active: Option<Request>,
}

thread_local! {
    static HUB: RefCell<Hub> = RefCell::new(Hub::default());
}

fn request(title: &str, selection: Selection, extensions: &[&str], suggested: Option<&str>) -> Response {
    let (sender, receiver) = mpsc::channel();
    HUB.with_borrow_mut(|hub| {
        hub.pending.push_back(Request {
            picker: Picker::custom(
                title.to_owned(),
                selection,
                extensions.iter().map(|s| (*s).to_owned()).collect(),
                suggested.map(str::to_owned),
            ),
            response: Some(sender),
            callback: None,
        });
    });
    receiver
}

pub fn open(title: &str, extensions: &[&str]) -> Response {
    request(title, Selection::OpenOne, extensions, None)
}

pub fn open_many(title: &str, extensions: &[&str]) -> Response {
    request(title, Selection::OpenMany, extensions, None)
}

pub fn save(title: &str, extension: &str, suggested: &str) -> Response {
    request(title, Selection::Save, &[extension], Some(suggested))
}

/// A small file already in memory can be written when the user chooses its
/// destination, without creating an orphan thread waiting for the dialog.
pub fn save_with(title: &str, extension: &str, suggested: &str, callback: impl FnOnce(PathBuf) + 'static) {
    save_with_result(title, extension, suggested, move |selected| {
        if let Some(path) = selected {
            callback(path);
        }
    });
}

pub fn save_with_result(
    title: &str,
    extension: &str,
    suggested: &str,
    callback: impl FnOnce(Option<PathBuf>) + 'static,
) {
    HUB.with_borrow_mut(|hub| {
        hub.pending.push_back(Request {
            picker: Picker::custom(
                title.to_owned(),
                Selection::Save,
                vec![extension.to_owned()],
                Some(suggested.to_owned()),
            ),
            response: None,
            callback: Some(Box::new(move |paths| callback(paths.and_then(|paths| paths.into_iter().next())))),
        });
    });
}

pub fn folder(title: &str) -> Response {
    request(title, Selection::Folder, &[], None)
}

/// Returns the selected paths on success, clears a cancelled or disconnected request,
/// and leaves a chooser in progress alone. No user files are read by this broker.
pub fn poll(waiting: &mut Option<Response>) -> Option<Vec<PathBuf>> {
    match waiting.as_ref()?.try_recv() {
        Ok(paths) => {
            *waiting = None;
            paths
        }
        Err(TryRecvError::Disconnected) => {
            *waiting = None;
            None
        }
        Err(TryRecvError::Empty) => None,
    }
}

pub fn busy() -> bool {
    HUB.with_borrow(|hub| hub.active.is_some() || !hub.pending.is_empty())
}

pub fn show(ctx: &Context) {
    let mut completed = None;
    HUB.with_borrow_mut(|hub| {
        if hub.active.is_none() {
            hub.active = hub.pending.pop_front();
        }
        let Some(request) = hub.active.as_mut() else { return };
        let choice = request.picker.show(ctx);
        if let Some(choice) = choice {
            let Some(request) = hub.active.take() else { return };
            let paths = match choice {
                Choice::Cancel => None,
                Choice::Files(paths) => Some(paths),
            };
            completed = Some((request, paths));
            ctx.request_repaint();
        }
    });
    if let Some((request, paths)) = completed {
        if let Some(callback) = request.callback {
            callback(paths);
        } else if let Some(sender) = request.response {
            let _ = sender.send(paths);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_file_request_replies_to_the_original_callback() {
        let mut result = Some(open("Import source", &["png"]));
        assert!(busy());
        let picture = std::env::temp_dir().join("sumi-test-source.png");
        HUB.with_borrow_mut(|hub| {
            let request = hub.pending.pop_front().expect("in-window picker queued");
            assert!(request.callback.is_none());
            request.response.expect("a receiver exists").send(Some(vec![picture.clone()])).unwrap();
        });
        assert_eq!(poll(&mut result), Some(vec![picture]));
        assert!(result.is_none());
        assert!(!busy());
    }

    #[test]
    fn save_callback_handles_success_and_cancel_without_leaking_queued_requests() {
        let result = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        for destination in [Some(PathBuf::from("example.sumi")), None] {
            let checked = std::rc::Rc::clone(&result);
            save_with_result("Save", "sumi", "example.sumi", move |picked| {
                checked.borrow_mut().push(picked);
            });
            let request = HUB.with_borrow_mut(|hub| hub.pending.pop_front().unwrap());
            let reply = destination.clone().map(|path| vec![path]);
            request.callback.unwrap()(reply);
        }
        assert_eq!(*result.borrow(), vec![Some(PathBuf::from("example.sumi")), None]);
        assert!(!busy());
    }
}
