//! Everything the GUI knows, without any drawing code: the open file, what the scan found,
//! and the log.
//!
//! Slow work (opening and scanning, extracting, saving, decoding) is done by the free
//! functions here, on a worker thread started by [`crate::jobs`] or [`crate::preview`];
//! their results are applied to the [`Session`] on the UI thread. Tests drive the same
//! functions directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use audscan_core::input::{self, Input};
use audscan_core::{
    AudioEntry, ExtractOptions, Manifest, Pcm, Rejected, ScanOptions, SourceInfo, audio_bytes, convert_wem, decode_file, extract_all,
    scan, track_bytes,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    pub text: String,
}

/// A file open for reading (memory-mapped; never written to).
pub struct OpenFile {
    pub path: PathBuf,
    pub data: Arc<Input>,
}

impl OpenFile {
    pub fn len(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// What a scan found.
pub struct Scanned {
    pub manifest: Manifest,
    pub rejected: Vec<Rejected>,
    pub seconds: f64,
}

/// A found file, or one track of a bank or package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection {
    pub id: u32,
    pub track: Option<usize>,
}

#[derive(Default)]
pub struct Session {
    pub file: Option<OpenFile>,
    pub scanned: Option<Scanned>,
    pub scan_options: ScanOptions,
    pub log: Vec<LogLine>,
    /// Bumped whenever the file or scan changes, so views can drop caches.
    pub generation: u64,
}

impl Session {
    pub fn info(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Info, text: text.into() });
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Warn, text: text.into() });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.log.push(LogLine { level: Level::Error, text: text.into() });
    }

    pub fn audio(&self) -> &[AudioEntry] {
        self.scanned.as_ref().map_or(&[], |s| &s.manifest.audio)
    }

    pub fn entry(&self, id: u32) -> Option<&AudioEntry> {
        self.audio().iter().find(|a| a.id == id)
    }

    pub fn rejected(&self) -> &[Rejected] {
        self.scanned.as_ref().map_or(&[], |s| &s.rejected)
    }

    pub fn set_opened(&mut self, (file, scanned): (OpenFile, Scanned)) {
        self.info(format!(
            "{}: {} audio file(s) in {:.2} s{}",
            file.path.display(),
            scanned.manifest.audio.len(),
            scanned.seconds,
            match scanned.rejected.len() {
                0 => String::new(),
                n => format!(", {n} header(s) that look like audio but can't be used"),
            }
        ));
        self.file = Some(file);
        self.scanned = Some(scanned);
        self.generation += 1;
    }

    pub fn set_scanned(&mut self, scanned: Scanned) {
        self.info(format!("scan: {} audio file(s) in {:.2} s", scanned.manifest.audio.len(), scanned.seconds));
        self.scanned = Some(scanned);
        self.generation += 1;
    }

    pub fn close(&mut self) {
        self.file = None;
        self.scanned = None;
        self.generation += 1;
    }
}

pub fn open_file(path: &Path) -> Result<OpenFile> {
    // The core's error already names the file.
    let data = input::open(path)?;
    Ok(OpenFile { path: path.to_path_buf(), data: Arc::new(data) })
}

pub fn run_scan(file: &OpenFile, opts: &ScanOptions) -> Scanned {
    let start = Instant::now();
    let report = scan(&file.data, opts);
    let manifest = Manifest::new(SourceInfo::describe(&file.path, &file.data), opts.clone(), &report.audio);
    Scanned { manifest, rejected: report.rejected, seconds: start.elapsed().as_secs_f64() }
}

pub fn open_and_scan(path: &Path, opts: &ScanOptions) -> Result<(OpenFile, Scanned)> {
    let file = open_file(path)?;
    let scanned = run_scan(&file, opts);
    Ok((file, scanned))
}

/// The bytes of a found file, or of one of its tracks as a file of its own.
pub fn item_bytes(data: &[u8], entry: &AudioEntry, track: Option<usize>) -> Result<Vec<u8>> {
    let bytes = audio_bytes(data, entry)?;
    Ok(match track {
        Some(i) => track_bytes(entry, bytes, i)?.into_owned(),
        None => bytes.to_vec(),
    })
}

/// A file name for an item: the name extract would give it.
pub fn item_file_name(entry: &AudioEntry, track: Option<usize>) -> String {
    match track.and_then(|i| entry.tracks.get(i).and_then(|t| t.split_filename(i))) {
        Some(name) => name.rsplit('/').next().unwrap_or(&name).to_string(),
        None => entry.file.clone(),
    }
}

/// A decoded sound and its waveform, for the preview.
pub struct Preview {
    pub pcm: Arc<Pcm>,
    /// (min, max) of each slice of the sound, -1 to 1, all channels together.
    pub peaks: Vec<(f32, f32)>,
}

/// Slices in a waveform.
const PEAKS: usize = 1600;

pub fn decode_item(data: &[u8], entry: &AudioEntry, track: Option<usize>) -> Result<Preview> {
    let bytes = item_bytes(data, entry, track)?;
    let pcm = decode_file(&bytes)?;
    let frames = pcm.frames();
    let channels = usize::from(pcm.channels.max(1));
    let per = frames.div_ceil(PEAKS).max(1);
    let peaks = pcm
        .samples
        .chunks(per * channels)
        .map(|slice| {
            let (lo, hi) = slice.iter().fold((0i16, 0i16), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            (f32::from(lo) / 32768.0, f32::from(hi) / 32767.0)
        })
        .collect();
    Ok(Preview { pcm: Arc::new(pcm), peaks })
}

/// Save an item as it is in the file (a track as a file of its own).
pub fn save_item(data: &[u8], entry: &AudioEntry, track: Option<usize>, path: &Path) -> Result<()> {
    let bytes = item_bytes(data, entry, track)?;
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

/// Convert an item (a WEM) to Ogg or WAV and save it. `path`'s extension is replaced by
/// the one the conversion gives; the path written is returned.
pub fn save_converted(data: &[u8], entry: &AudioEntry, track: Option<usize>, path: &Path) -> Result<PathBuf> {
    let bytes = item_bytes(data, entry, track)?;
    let converted = convert_wem(&bytes)?;
    let out = path.with_extension(converted.extension);
    std::fs::write(&out, &converted.bytes).with_context(|| format!("writing {}", out.display()))?;
    Ok(out)
}

/// What an extract did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractReport {
    pub files: usize,
    pub split: usize,
    pub converted: usize,
    pub not_converted: usize,
}

/// Extract every file the manifest lists (or just `only`), optionally splitting banks and
/// converting WEMs.
pub fn extract(data: &[u8], manifest: &Manifest, only: Option<u32>, dir: &Path, split: bool, convert: bool) -> Result<ExtractReport> {
    let mut manifest = manifest.clone();
    if let Some(id) = only {
        manifest.audio.retain(|a| a.id == id);
    }
    let files = extract_all(data, &manifest, dir, &ExtractOptions { verify_source: false, split, convert })?;
    Ok(ExtractReport {
        files: files.len(),
        split: files.iter().map(|f| f.split.len()).sum(),
        converted: files.iter().map(|f| f.converted.len()).sum(),
        not_converted: files.iter().map(|f| f.not_converted.len()).sum(),
    })
}

pub fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MiB", b as f64 / 1048576.0),
        b => format!("{:.2} GiB", b as f64 / 1073741824.0),
    }
}

/// A length for lists: `230 ms` under a second, `1:05.2`, `6:52:06` over an hour, or
/// empty when unknown.
pub fn length(seconds: Option<f64>) -> String {
    match seconds {
        Some(s) if s < 1.0 => format!("{:.0} ms", s * 1000.0),
        Some(s) => clock(s),
        None => String::new(),
    }
}

/// A time on the player's clock: `0:00.0`, `1:05.2`, or `6:52:06` over an hour.
pub fn clock(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    let (h, m, s) = (tenths / 36000, tenths / 600 % 60, tenths / 10 % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}.{}", tenths % 10) }
}


/// An entry's length in seconds (a bank's is its tracks' total).
pub fn entry_seconds(entry: &AudioEntry) -> Option<f64> {
    let seconds = |samples: Option<u64>, rate: u32| (rate > 0).then_some(samples? as f64 / f64::from(rate));
    if entry.tracks.is_empty() {
        return seconds(entry.samples, entry.sample_rate);
    }
    let audio: Vec<_> = entry.tracks.iter().filter(|t| t.sample_rate > 0).collect();
    if audio.is_empty() {
        return None;
    }
    audio.iter().map(|t| seconds(t.samples, t.sample_rate)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert_eq!(length(Some(0.023)), "23 ms");
        assert_eq!(length(Some(65.24)), "1:05.2");
        assert_eq!(length(Some(24726.1)), "6:52:06");
        assert_eq!(length(None), "");
        assert_eq!(clock(0.0), "0:00.0");
    }
}
