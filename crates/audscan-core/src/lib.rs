//! audscan-core: find audio (RIFF/RIFX WAVE including Wwise `.wem`, FMOD FSB5 banks, Ogg)
//! inside arbitrary binary files and extract it; later, reinject edited versions.
//!
//! The typical flow is [`input::open`] → [`scan()`] → [`Manifest::new`] → [`extract_all`].
//! Front ends (CLI, later a GUI) call this library directly.

pub mod error;
pub mod extract;
pub mod format;
pub mod formats;
pub mod input;
pub mod manifest;
pub mod scan;

pub use error::{Error, Result};
pub use extract::{ExtractOptions, ExtractedFile, audio_bytes, extract_all};
pub use format::{AudioFormat, AudioInfo, Container, Reject, Track, format_for};
pub use manifest::{AudioEntry, Manifest, SourceInfo};
pub use scan::{FoundAudio, Rejected, ScanOptions, ScanReport, audio_at, scan};
