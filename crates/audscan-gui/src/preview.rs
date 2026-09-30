//! Decoding the selected sound off the UI thread. A newer request replaces a pending one:
//! only the latest selection's result is kept.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use audscan_core::AudioEntry;
use audscan_core::input::Input;

use crate::session::{self, Preview, Selection};

/// What a preview shows: a file or track as it is in the input, or the replacement chosen
/// for it (read from disk, so `generation` says which state of the edits it was).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewKey {
    pub selection: Selection,
    pub edit: Option<PathBuf>,
    pub generation: u64,
}

#[derive(Default)]
pub struct Previewer {
    pending: Option<(PreviewKey, Receiver<Result<Preview, String>>)>,
    current: Option<(PreviewKey, Result<Arc<Preview>, String>)>,
}

impl Previewer {
    /// Decode what `key` names (the selection's entry is `entry`) unless it's already shown
    /// or on its way.
    pub fn request(&mut self, key: PreviewKey, data: Arc<Input>, entry: AudioEntry) {
        if self.shows(&key) || self.pending.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (track, edit) = (key.selection.track, key.edit.clone());
        std::thread::spawn(move || {
            let result = match edit {
                Some(path) => session::decode_file_at(&path),
                None => session::decode_item(&data, &entry, track),
            };
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
        self.pending = Some((key, rx));
    }

    /// Take a finished decode, if there is one. Returns true if the preview changed.
    pub fn poll(&mut self) -> bool {
        let Some((key, rx)) = &self.pending else { return false };
        match rx.try_recv() {
            Ok(result) => {
                self.current = Some((key.clone(), result.map(Arc::new)));
                self.pending = None;
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.current = Some((key.clone(), Err("decoding failed unexpectedly".into())));
                self.pending = None;
                true
            }
        }
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    fn shows(&self, key: &PreviewKey) -> bool {
        self.current.as_ref().is_some_and(|(k, _)| k == key)
    }

    /// The preview for `key`, if it has been decoded (or failed to).
    pub fn get(&self, key: &PreviewKey) -> Option<&Result<Arc<Preview>, String>> {
        self.current.as_ref().filter(|(k, _)| k == key).map(|(_, r)| r)
    }

    pub fn clear(&mut self) {
        self.pending = None;
        self.current = None;
    }
}
