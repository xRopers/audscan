//! audscan-core: find audio (RIFF/RIFX WAVE including Wwise `.wem`, Wwise SoundBanks and
//! packages, FMOD FSB4 and FSB5 banks, Ogg) inside arbitrary binary files, extract it, and put
//! edited versions back.
//!
//! The typical flow is [`input::open`] → [`scan()`] → [`Manifest::new`] → [`extract_all`];
//! [`convert_wem`] turns a WEM into Ogg or WAV; [`load_edits`], [`pack`] and
//! [`PackResult::write_file`] put edited sounds back into a copy of the file.
//! Front ends (the CLI and the GUI) call this library directly.

pub mod convert;
pub mod decode;
pub mod error;
pub mod extract;
pub mod format;
pub mod formats;
pub mod input;
pub mod manifest;
pub mod output;
pub mod pack;
pub mod scan;

pub use convert::{ConvertError, Converted, convert_wem};
pub use decode::{Pcm, decode_file};
pub use error::{Error, Result};
pub use extract::{ExtractOptions, ExtractedFile, audio_bytes, extract_all, track_bytes};
pub use format::{AudioFormat, AudioInfo, Container, Reject, Track, format_for};
pub use manifest::{AudioEntry, Manifest, SourceInfo};
pub use pack::{AudioEdit, AudioPlan, Edits, FieldUpdate, FoundEdits, Outcome, PackOptions, PackResult, Placement, load_edits, pack, pack_entry};
pub use scan::{FoundAudio, Rejected, ScanOptions, ScanReport, audio_at, scan};
