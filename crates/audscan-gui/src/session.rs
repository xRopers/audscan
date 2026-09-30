//! Everything the GUI knows, without any drawing code: the open file, what the scan found,
//! the edits, and the log.
//!
//! Edits are kept as file paths and read when they're previewed or packed, so a sound can
//! still be changed in another program after it was chosen.
//!
//! Slow work (opening and scanning, extracting, saving, decoding, packing) is done by the
//! free functions here, on a worker thread started by [`crate::jobs`] or
//! [`crate::preview`]; their results are applied to the [`Session`] on the UI thread.
//! Tests drive the same functions directly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use audscan_core::input::{self, Input};
use audscan_core::extract::split_files;
use audscan_core::{
    AudioEdit, AudioEntry, AudioPlan, Edits, ExtractOptions, FieldUpdate, Manifest, Outcome, PackOptions, Pcm, Placement,
    Rejected, ScanOptions, SourceInfo, audio_bytes, convert_wem, decode_file, extract_all, load_edits, pack, scan, track_bytes,
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

/// Where a found file's new contents come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditSource {
    /// A whole replacement file of the same kind.
    File(PathBuf),
    /// Replacements for some tracks of a bank or package, by track index.
    Tracks(BTreeMap<usize, PathBuf>),
}

impl EditSource {
    /// The replacement for `track` (`None`: the whole file), if this edit has one.
    pub fn path(&self, track: Option<usize>) -> Option<&PathBuf> {
        match (self, track) {
            (EditSource::File(p), None) => Some(p),
            (EditSource::Tracks(m), Some(t)) => m.get(&t),
            _ => None,
        }
    }

    /// The files, for display, with the track each replaces.
    pub fn files(&self) -> Vec<(Option<usize>, &Path)> {
        match self {
            EditSource::File(p) => vec![(None, p.as_path())],
            EditSource::Tracks(m) => m.iter().map(|(&t, p)| (Some(t), p.as_path())).collect(),
        }
    }
}

/// What the last pack did or would do.
#[derive(Debug, Clone)]
pub struct PackReport {
    pub audio: Vec<AudioPlan>,
    pub fields: Vec<FieldUpdate>,
    pub input_len: u64,
    pub output_len: u64,
    /// Where the packed file was written and checked, or `None` for a dry run.
    pub written: Option<PathBuf>,
}

impl PackReport {
    pub fn changed(&self) -> usize {
        self.audio.iter().filter(|a| a.outcome != Outcome::Unchanged).count()
    }
}

#[derive(Default)]
pub struct Session {
    pub file: Option<OpenFile>,
    pub scanned: Option<Scanned>,
    pub scan_options: ScanOptions,
    /// Audio id -> where its new contents come from.
    pub edits: BTreeMap<u32, EditSource>,
    /// Bumped whenever the edits change, so the preview reloads.
    pub edits_generation: u64,
    pub last_pack: Option<PackReport>,
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
        self.clear_edits();
    }

    pub fn set_scanned(&mut self, scanned: Scanned) {
        self.info(format!("scan: {} audio file(s) in {:.2} s", scanned.manifest.audio.len(), scanned.seconds));
        self.scanned = Some(scanned);
        self.generation += 1;
        self.clear_edits();
    }

    pub fn close(&mut self) {
        self.file = None;
        self.scanned = None;
        self.generation += 1;
        self.clear_edits();
    }

    fn clear_edits(&mut self) {
        self.edits.clear();
        self.edits_changed();
    }

    fn edits_changed(&mut self) {
        self.last_pack = None;
        self.edits_generation += 1;
    }

    /// The replacement chosen for a file or track, if any.
    pub fn edit_path(&self, selection: Selection) -> Option<&PathBuf> {
        self.edits.get(&selection.id)?.path(selection.track)
    }

    /// Replace a file (or one of its tracks) with the file at `path`. A whole-file edit and
    /// track edits of the same file don't mix: the newer one replaces the other.
    pub fn set_edit(&mut self, selection: Selection, path: PathBuf) {
        let what = describe(selection);
        match (selection.track, self.edits.get_mut(&selection.id)) {
            (Some(t), Some(EditSource::Tracks(tracks))) => {
                tracks.insert(t, path.clone());
            }
            (track, previous) => {
                if previous.is_some() {
                    self.warn(format!("{what}: its earlier edit was dropped (edit a file whole or by its tracks, not both)"));
                }
                let edit = match track {
                    Some(t) => EditSource::Tracks(BTreeMap::from([(t, path.clone())])),
                    None => EditSource::File(path.clone()),
                };
                self.edits.insert(selection.id, edit);
            }
        }
        self.info(format!("{what}: replaced by {}", path.display()));
        self.edits_changed();
    }

    pub fn revert(&mut self, selection: Selection) {
        let removed = match (selection.track, self.edits.get_mut(&selection.id)) {
            (None, Some(_)) => self.edits.remove(&selection.id).is_some(),
            (Some(t), Some(EditSource::Tracks(tracks))) => {
                let removed = tracks.remove(&t).is_some();
                if tracks.is_empty() {
                    self.edits.remove(&selection.id);
                }
                removed
            }
            _ => false,
        };
        if removed {
            self.info(format!("{}: edit reverted", describe(selection)));
            self.edits_changed();
        }
    }

    pub fn set_imported_edits(&mut self, dir: &Path, (edits, unchanged): (BTreeMap<u32, EditSource>, usize)) {
        self.info(format!(
            "{} edited file(s) found in {} ({unchanged} unedited file(s) skipped)",
            edits.len(),
            dir.display()
        ));
        self.edits = edits;
        self.edits_changed();
    }

    pub fn set_pack_report(&mut self, report: PackReport) {
        match &report.written {
            Some(path) => self.info(format!("{} file(s) packed into {} (verified)", report.changed(), path.display())),
            None => self.info(format!("dry run: {} file(s) would change", report.changed())),
        }
        for plan in &report.audio {
            for note in &plan.notes {
                self.warn(format!("audio {} at {:#x}: {note}", plan.id, plan.offset));
            }
        }
        self.last_pack = Some(report);
    }
}

/// `audio 3` or `audio 3, track 7`, for the log.
pub fn describe(selection: Selection) -> String {
    match selection.track {
        Some(t) => format!("audio {}, track {t}", selection.id),
        None => format!("audio {}", selection.id),
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
    preview_of(decode_file(&bytes)?)
}

fn preview_of(pcm: Pcm) -> Result<Preview> {
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

/// Decode a replacement file for the preview, the way [`decode_item`] decodes the original.
pub fn decode_file_at(path: &Path) -> Result<Preview> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    preview_of(decode_file(&bytes)?)
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

/// Read an edit's files.
pub fn read_edit(source: &EditSource) -> Result<AudioEdit> {
    let read = |p: &Path| std::fs::read(p).with_context(|| p.display().to_string());
    Ok(match source {
        EditSource::File(path) => AudioEdit::File(read(path)?),
        EditSource::Tracks(paths) => AudioEdit::Tracks(paths.iter().map(|(&t, p)| Ok((t, read(p)?))).collect::<Result<_>>()?),
    })
}

pub fn read_edits(edits: &BTreeMap<u32, EditSource>) -> Result<Edits> {
    edits.iter().map(|(&id, source)| Ok((id, read_edit(source)?))).collect()
}

/// Check a replacement before taking it: pack just this change (without writing) and
/// return what pack would say about it. Errors say why it can't go in.
pub fn check_replacement(data: &[u8], manifest: &Manifest, selection: Selection, path: &Path) -> Result<AudioPlan> {
    let source = match selection.track {
        Some(t) => EditSource::Tracks(BTreeMap::from([(t, path.to_path_buf())])),
        None => EditSource::File(path.to_path_buf()),
    };
    let edits = BTreeMap::from([(selection.id, read_edit(&source)?)]);
    // The whole file's CRC is checked when packing for real.
    let result = pack(data, manifest, &edits, &PackOptions { verify_source: false })?;
    Ok(result.audio.into_iter().next().expect("one edit, one plan"))
}

/// Pack the edits: a dry run, or written to `output` and checked.
pub fn run_pack(data: &[u8], manifest: &Manifest, edits: &Edits, output: Option<&Path>) -> Result<PackReport> {
    let result = pack(data, manifest, edits, &PackOptions::default())?;
    if let Some(out) = output {
        if result.changed() == 0 {
            anyhow::bail!("nothing to write: every edit gives the original bytes");
        }
        result.write_file(data, out)?;
    }
    Ok(PackReport {
        audio: result.audio,
        fields: result.fields,
        input_len: result.input_len,
        output_len: result.output_len,
        written: output.map(Path::to_path_buf),
    })
}

/// Edits found in an extract folder (see [`load_edits`]), as file paths, and how many
/// unedited files were skipped.
pub fn import_edits(data: &[u8], manifest: &Manifest, dir: &Path) -> Result<(BTreeMap<u32, EditSource>, usize)> {
    let found = load_edits(data, manifest, dir)?;
    let mut edits = BTreeMap::new();
    for (&id, edit) in &found.edits {
        let entry = manifest.audio.iter().find(|a| a.id == id).expect("edits are of listed audio");
        let source = match edit {
            AudioEdit::File(_) => EditSource::File(dir.join(&entry.file)),
            AudioEdit::Tracks(tracks) => {
                let folder = dir.join(entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(stem, _)| stem));
                let names: BTreeMap<usize, String> = split_files(entry)?.into_iter().map(|(name, i)| (i, name)).collect();
                EditSource::Tracks(tracks.keys().map(|t| (*t, folder.join(&names[t]))).collect())
            }
        };
        edits.insert(id, source);
    }
    Ok((edits, found.unchanged))
}

/// A sentence on how a changed file fits, for the pack window and the log.
pub fn placement_text(plan: &AudioPlan) -> String {
    match plan.placement {
        _ if plan.outcome == Outcome::Unchanged => "unchanged (the same bytes as the original)".to_string(),
        Placement::InPlace => "the same size: written in place".to_string(),
        Placement::Padded { by, inside: true } => format!("{} smaller: padded inside to keep its place", human_size(by)),
        Placement::Padded { by, inside: false } => format!("{} smaller: zeros after it keep its place", human_size(by)),
        Placement::Resized => format!(
            "{} {}: it ends the file, so the output changes size",
            human_size(plan.new_size.abs_diff(plan.old_size)),
            if plan.new_size > plan.old_size { "bigger" } else { "smaller" }
        ),
    }
}

/// `game.pak` -> `game.packed.pak`.
pub fn default_output_name(input: &Path) -> String {
    let stem = input.file_stem().map_or_else(|| "output".into(), |s| s.to_string_lossy().into_owned());
    match input.extension() {
        Some(ext) => format!("{stem}.packed.{}", ext.to_string_lossy()),
        None => format!("{stem}.packed"),
    }
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
