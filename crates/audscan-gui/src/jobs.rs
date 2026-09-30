//! Running slow work off the UI thread. A job runs a closure on a worker thread; the
//! closure returns another closure that applies its result to the [`Session`], which the
//! UI thread calls when it polls. One job runs at a time, so a result always applies to
//! the state it was started from.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use crate::session::Session;

pub type Apply = Box<dyn FnOnce(&mut Session) + Send>;

pub struct Job {
    pub label: String,
    pub started: Instant,
    rx: Receiver<Apply>,
}

#[derive(Default)]
pub struct Jobs {
    current: Option<Job>,
    /// Called from the worker when it finishes (to wake the UI).
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Jobs {
    pub fn set_waker(&mut self, waker: impl Fn() + Send + Sync + 'static) {
        self.waker = Some(Arc::new(waker));
    }

    pub fn busy(&self) -> bool {
        self.current.is_some()
    }

    pub fn current(&self) -> Option<&Job> {
        self.current.as_ref()
    }

    /// Start `work` on a worker thread. Returns false (and does nothing) if a job is
    /// already running.
    pub fn start(&mut self, label: impl Into<String>, work: impl FnOnce() -> Apply + Send + 'static) -> bool {
        if self.busy() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let _ = tx.send(work());
            if let Some(wake) = waker {
                wake();
            }
        });
        self.current = Some(Job { label: label.into(), started: Instant::now(), rx });
        true
    }

    /// Apply the current job's result if it has finished. Returns true if it had.
    pub fn poll(&mut self, session: &mut Session) -> bool {
        let Some(job) = &self.current else { return false };
        match job.rx.try_recv() {
            Ok(apply) => {
                self.current = None;
                apply(session);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                session.error(format!("{} failed unexpectedly (the worker panicked)", job.label));
                self.current = None;
                true
            }
        }
    }

    /// Block until the current job finishes and apply it (for tests).
    pub fn wait(&mut self, session: &mut Session) {
        if let Some(job) = self.current.take() {
            match job.rx.recv() {
                Ok(apply) => apply(session),
                Err(_) => session.error(format!("{} failed unexpectedly (the worker panicked)", job.label)),
            }
        }
    }
}

/// The usual shape of a job result: on success apply `ok`, on failure log the error.
pub fn finish<T: Send + 'static>(
    what: &'static str,
    result: anyhow::Result<T>,
    ok: impl FnOnce(&mut Session, T) + Send + 'static,
) -> Apply {
    match result {
        Ok(value) => Box::new(move |s| ok(s, value)),
        Err(e) => Box::new(move |s| s.error(format!("{what}: {e:#}"))),
    }
}
