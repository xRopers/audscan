//! Decoding the selected sound off the UI thread. A newer request replaces a pending one:
//! only the latest selection's result is kept.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use audscan_core::AudioEntry;
use audscan_core::input::Input;

use crate::session::{self, Preview, Selection};

#[derive(Default)]
pub struct Previewer {
    pending: Option<(Selection, Receiver<Result<Preview, String>>)>,
    current: Option<(Selection, Result<Arc<Preview>, String>)>,
}

impl Previewer {
    /// Decode `selection` (whose entry is `entry`) unless it's already shown or on its way.
    pub fn request(&mut self, selection: Selection, data: Arc<Input>, entry: AudioEntry) {
        if self.shows(selection) || self.pending.as_ref().is_some_and(|(s, _)| *s == selection) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(session::decode_item(&data, &entry, selection.track).map_err(|e| format!("{e:#}")));
        });
        self.pending = Some((selection, rx));
    }

    /// Take a finished decode, if there is one. Returns true if the preview changed.
    pub fn poll(&mut self) -> bool {
        let Some((selection, rx)) = &self.pending else { return false };
        match rx.try_recv() {
            Ok(result) => {
                self.current = Some((*selection, result.map(Arc::new)));
                self.pending = None;
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.current = Some((*selection, Err("decoding failed unexpectedly".into())));
                self.pending = None;
                true
            }
        }
    }

    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    fn shows(&self, selection: Selection) -> bool {
        self.current.as_ref().is_some_and(|(s, _)| *s == selection)
    }

    /// The preview of `selection`, if it has been decoded (or failed to).
    pub fn get(&self, selection: Selection) -> Option<&Result<Arc<Preview>, String>> {
        self.current.as_ref().filter(|(s, _)| *s == selection).map(|(_, r)| r)
    }

    pub fn clear(&mut self) {
        self.pending = None;
        self.current = None;
    }
}
