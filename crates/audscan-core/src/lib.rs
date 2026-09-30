//! audscan-core: find audio (RIFF/RIFX WAVE including Wwise `.wem`, Wwise SoundBanks and
//! packages, FMOD FSB4 and FSB5 banks, Ogg) inside arbitrary binary files and extract it; later,
//! reinject edited versions.
//!
//! The typical flow is [`input::open`] → [`scan()`] → [`Manifest::new`] → [`extract_all`];
//! [`convert_wem`] turns a WEM into Ogg or WAV.
//! Front ends (CLI, later a GUI) call this library directly.

pub mod convert;
pub mod decode;
pub mod error;
pub mod extract;
pub mod format;
pub mod formats;
pub mod input;
pub mod manifest;
pub mod scan;

pub use convert::{ConvertError, Converted, convert_wem};
pub use decode::{Pcm, decode_file};
pub use error::{Error, Result};
pub use extract::{ExtractOptions, ExtractedFile, audio_bytes, extract_all, track_bytes};
pub use format::{AudioFormat, AudioInfo, Container, Reject, Track, format_for};
pub use manifest::{AudioEntry, Manifest, SourceInfo};
pub use scan::{FoundAudio, Rejected, ScanOptions, ScanReport, audio_at, scan};
