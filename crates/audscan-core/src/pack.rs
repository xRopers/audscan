//! Putting edited sounds back into a copy of the file.
//!
//! An edit replaces a whole file the scan found (a WAV, WEM, bank, package or Ogg, by a
//! file of the same kind) or some tracks of a bank or package: WEMs and SoundBanks in a
//! Wwise `.bnk` or `.pck`, sounds in an FMOD FSB5 bank (a one-track FSB5 of the bank's
//! codec, or for a PCM bank a WAV). The bank is rebuilt around the new tracks, so it can
//! change size.
//!
//! A file whose size changes then has to fit its place in the input:
//! - the same size is written over the original;
//! - smaller is padded back to the original size, inside the file where its format allows
//!   (a RIFF `JUNK` chunk, a SoundBank's DATA section, an FSB5 bank's sample data) and
//!   with zeros after it otherwise;
//! - at the end of the input it can grow or shrink freely: the output changes length, and
//!   size fields that measure to the end of the input (a RIFF header, the chunk holding an
//!   FSB5 bank in an FMOD `.bank`) or the file's own size just before it are updated;
//! - bigger, anywhere else, is an error saying by how much: the game's index of the
//!   archive would need rewriting, which audscan can't do for a format it doesn't know.
//!
//! [`pack`] builds and checks everything in memory; [`PackResult::write_file`] writes the
//! output through a temporary file, reads it back and checks it again before renaming.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::{Error, Result, io_err};
use crate::extract::{audio_bytes, split_files, track_bytes};
use crate::format::{Container, Reject, format_for};
use crate::formats::{bnk, fsb5, pck, riff};
use crate::input;
use crate::manifest::{AudioEntry, Manifest};
use crate::output::write_via_temp;

/// A change to one found file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioEdit {
    /// A whole new file of the same kind.
    File(Vec<u8>),
    /// New files for some tracks of a bank or package, by track index: as `extract --split`
    /// writes them (a WEM or SoundBank; a one-track FSB5, or a WAV for a PCM FSB5 bank).
    Tracks(BTreeMap<usize, Vec<u8>>),
}

/// Edits by audio id.
pub type Edits = BTreeMap<u32, AudioEdit>;

#[derive(Debug, Clone)]
pub struct PackOptions {
    /// Refuse to pack if the input's size or CRC differs from the manifest's.
    pub verify_source: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self { verify_source: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The whole file was replaced.
    Replaced,
    /// These tracks were replaced and the bank or package rebuilt around them.
    TracksReplaced(Vec<usize>),
    /// The edit gives the same bytes as the original.
    Unchanged,
}

/// How a changed file was fitted into the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The same size: written over the original.
    InPlace,
    /// Smaller, and padded by `by` bytes to its original size: inside the file (`inside`)
    /// or with zeros after it.
    Padded { by: u64, inside: bool },
    /// It ends the input, so the output grows or shrinks with it.
    Resized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPlan {
    pub id: u32,
    /// Where it is, in the input and in the output alike.
    pub offset: u64,
    pub old_size: u64,
    /// Its size in the output, as its header gives it (padding inside it included).
    pub new_size: u64,
    /// The bytes it takes in the output: `new_size` plus any zeros after it.
    pub slot: u64,
    pub outcome: Outcome,
    pub placement: Placement,
    /// Things worth knowing, such as a codec that changed.
    pub notes: Vec<String>,
    /// CRC-32 of its `slot` bytes in the output.
    pub crc32: u32,
    pub format: Container,
}

/// A size field outside the audio, updated because a file at the end of the input changed
/// size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldUpdate {
    pub offset: u64,
    pub big_endian: bool,
    pub old: u32,
    pub new: u32,
    /// What it measures: `the rest of the input`, `the input`, or `audio N`.
    pub measures: String,
}

/// New bytes for `old_len` bytes of the input at `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Patch {
    offset: u64,
    old_len: u64,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PackResult {
    /// One per edited file, in file order.
    pub audio: Vec<AudioPlan>,
    pub fields: Vec<FieldUpdate>,
    patches: Vec<Patch>,
    pub input_len: u64,
    pub output_len: u64,
}

impl PackResult {
    pub fn changed(&self) -> usize {
        self.audio.iter().filter(|a| a.outcome != Outcome::Unchanged).count()
    }

    /// The whole output in memory (tests and small files; [`write_file`](Self::write_file)
    /// streams instead).
    pub fn output(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.output_len as usize);
        self.write_to(data, &mut out).expect("writing to memory cannot fail");
        out
    }

    fn write_to(&self, data: &[u8], out: &mut impl Write) -> std::io::Result<()> {
        let mut pos = 0usize;
        for p in &self.patches {
            let offset = p.offset as usize;
            out.write_all(&data[pos..offset])?;
            out.write_all(&p.bytes)?;
            pos = offset + p.old_len as usize;
        }
        out.write_all(&data[pos..])
    }

    /// Write the input with the changes applied to `path`, via a temporary file that is
    /// read back and checked before it replaces `path`.
    pub fn write_file(&self, data: &[u8], path: &Path) -> Result<()> {
        write_via_temp(path, |tmp| {
            let file = fs::File::create(tmp).map_err(io_err(tmp))?;
            let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
            self.write_to(data, &mut out).map_err(io_err(tmp))?;
            out.into_inner().map_err(|e| io_err(tmp)(e.into_error()))?.sync_all().map_err(io_err(tmp))?;
            let written = input::open(tmp)?;
            self.verify(&written)
        })
    }

    /// Check a packed file: the right length, every changed file where it belongs with its
    /// new CRC and still reading as its format at its new size, and every size field
    /// holding its new value.
    pub fn verify(&self, packed: &[u8]) -> Result<()> {
        if packed.len() as u64 != self.output_len {
            return Err(Error::Pack(format!("output is {} bytes, expected {}", packed.len(), self.output_len)));
        }
        for plan in self.audio.iter().filter(|p| p.outcome != Outcome::Unchanged) {
            let fail = |reason: String| Error::Verify { id: plan.id, offset: plan.offset, reason };
            let bytes = &packed[plan.offset as usize..(plan.offset + plan.slot) as usize];
            let crc = crc32fast::hash(bytes);
            if crc != plan.crc32 {
                return Err(fail(format!("CRC-32 is {crc:08x}, expected {:08x}", plan.crc32)));
            }
            match format_for(plan.format).parse(&packed[plan.offset as usize..]) {
                Ok(info) if info.size == plan.new_size => {}
                Ok(info) => return Err(fail(format!("reads as {} bytes, expected {}", info.size, plan.new_size))),
                Err(e) => return Err(fail(format!("no longer reads as {}: {e:?}", plan.format))),
            }
        }
        for f in &self.fields {
            let b: [u8; 4] = packed[f.offset as usize..f.offset as usize + 4].try_into().unwrap();
            let v = if f.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) };
            if v != f.new {
                return Err(Error::Pack(format!("the size field at {:#x} holds {v}, expected {}", f.offset, f.new)));
            }
        }
        Ok(())
    }
}

/// Build the new bytes of every edited file, fit them into the input, and check them.
pub fn pack(data: &[u8], manifest: &Manifest, edits: &Edits, opts: &PackOptions) -> Result<PackResult> {
    if opts.verify_source {
        manifest.source.check(data)?;
    }
    if let Some(id) = edits.keys().find(|id| !manifest.audio.iter().any(|a| a.id == **id)) {
        return Err(Error::Pack(format!("there is no audio {id} in the manifest")));
    }
    let input_len = data.len() as u64;
    let mut result = PackResult { audio: Vec::new(), fields: Vec::new(), patches: Vec::new(), input_len, output_len: input_len };
    for entry in &manifest.audio {
        let Some(edit) = edits.get(&entry.id) else { continue };
        let fail = |reason: String| Error::Edit { id: entry.id, offset: entry.offset, reason };
        let (bytes, outcome, mut notes) = pack_entry(audio_bytes(data, entry)?, entry, edit)?;
        let new_len = bytes.len() as u64;
        let (bytes, placement, new_size) = if outcome == Outcome::Unchanged || new_len == entry.size {
            (bytes, Placement::InPlace, new_len)
        } else if entry.end() == input_len {
            (bytes, Placement::Resized, new_len)
        } else if new_len < entry.size {
            let by = entry.size - new_len;
            match pad(entry.format, &bytes, by as usize) {
                Some(padded) => (padded, Placement::Padded { by, inside: true }, entry.size),
                None => {
                    let mut padded = bytes;
                    padded.resize(entry.size as usize, 0);
                    notes.push(format!("{by} zero bytes after it fill the rest of its place"));
                    (padded, Placement::Padded { by, inside: false }, new_len)
                }
            }
        } else {
            return Err(fail(format!(
                "{} bytes too big: the new {} is {new_len} bytes, but its place in the file holds {}. \
                 Only a file at the end of the input can grow; make it smaller to fit",
                new_len - entry.size,
                entry.label(),
                entry.size
            )));
        };
        if placement == Placement::Resized {
            result.fields = length_fields(data, entry, new_len).map_err(fail)?;
            result.output_len = input_len - entry.size + new_len;
            for f in &result.fields {
                let bytes = if f.big_endian { f.new.to_be_bytes() } else { f.new.to_le_bytes() };
                result.patches.push(Patch { offset: f.offset, old_len: 4, bytes: bytes.to_vec() });
            }
        }
        let crc32 = crc32fast::hash(&bytes);
        let slot = bytes.len() as u64;
        if outcome != Outcome::Unchanged {
            result.patches.push(Patch { offset: entry.offset, old_len: entry.size, bytes });
        }
        result.audio.push(AudioPlan {
            id: entry.id,
            offset: entry.offset,
            old_size: entry.size,
            new_size,
            slot,
            outcome,
            placement,
            notes,
            crc32,
            format: entry.format,
        });
    }
    result.patches.sort_by_key(|p| p.offset);
    Ok(result)
}

/// The new bytes of one file: `original` (its current bytes) with `edit` applied, what
/// happened, and notes. Any size; [`pack`] fits it into the input.
pub fn pack_entry(original: &[u8], entry: &AudioEntry, edit: &AudioEdit) -> Result<(Vec<u8>, Outcome, Vec<String>)> {
    let fail = |reason: String| Error::Edit { id: entry.id, offset: entry.offset, reason };
    let (bytes, outcome, notes) = match edit {
        AudioEdit::File(new) => (new.clone(), Outcome::Replaced, check_file(new, entry).map_err(fail)?),
        AudioEdit::Tracks(tracks) => {
            if tracks.is_empty() {
                return Ok((original.to_vec(), Outcome::Unchanged, Vec::new()));
            }
            let (bytes, notes) = match entry.format {
                Container::Bnk => bnk::replace_media(original, tracks),
                Container::Pck => pck::replace_files(original, tracks),
                Container::Fsb5 => fsb5::replace_tracks(original, tracks),
                _ => Err(format!("a {} has no tracks to replace one by one; replace the whole file", entry.label())),
            }
            .map_err(fail)?;
            check_tracks(original, &bytes, entry, tracks).map_err(fail)?;
            (bytes, Outcome::TracksReplaced(tracks.keys().copied().collect()), notes)
        }
    };
    if bytes == original {
        return Ok((bytes, Outcome::Unchanged, Vec::new()));
    }
    Ok((bytes, outcome, notes))
}

/// Check a whole replacement file: the same format (and for RIFF, WAV for WAV and WEM for
/// WEM in the same byte order), exactly one file long. Returns notes.
fn check_file(new: &[u8], entry: &AudioEntry) -> std::result::Result<Vec<String>, String> {
    let label = entry.label();
    let info = format_for(entry.format).parse(new).map_err(|e| match e {
        Reject::Bad(reason) => format!("the replacement isn't a usable {label} file: {reason}"),
        Reject::NoMatch => format!("the replacement isn't a {label} file"),
    })?;
    if info.size != new.len() as u64 {
        return Err(format!("the replacement is {} bytes, but its header says {}", new.len(), info.size));
    }
    if entry.format == Container::Riff {
        match (entry.wwise, info.wwise) {
            (true, false) => return Err(format!("the replacement is a plain WAV ({}), not a WEM; make a WEM with Wwise first", info.codec)),
            (false, true) => return Err("the replacement is a Wwise WEM, but this is a plain WAV".into()),
            _ => {}
        }
    }
    if info.big_endian != entry.big_endian {
        return Err("the replacement's byte order differs from the original's".into());
    }
    let mut notes = Vec::new();
    if info.codec != entry.codec {
        notes.push(format!("the codec changes from {} to {}", entry.codec, info.codec));
    }
    if let Some(note) = info.note {
        notes.push(format!("the replacement: {note}"));
    }
    Ok(notes)
}

/// Check a rebuilt bank or package: it reads as the same format with the same tracks,
/// the replaced Wwise files are exactly the new ones and the others exactly as they were.
/// (FSB5 checks its own tracks as it rebuilds, since they change form when split out.)
fn check_tracks(original: &[u8], rebuilt: &[u8], entry: &AudioEntry, new: &BTreeMap<usize, Vec<u8>>) -> std::result::Result<(), String> {
    let info = format_for(entry.format).parse(rebuilt).map_err(|e| format!("the rebuilt {} doesn't read back: {e:?}", entry.label()))?;
    if info.size != rebuilt.len() as u64 || info.tracks.len() != entry.tracks.len() {
        return Err(format!("the rebuilt {} reads back differently", entry.label()));
    }
    if entry.format == Container::Fsb5 {
        return Ok(());
    }
    let slice = |bytes: &'_ [u8], t: &crate::format::Track| bytes[t.offset as usize..(t.offset + t.size) as usize].to_vec();
    for (i, (old, now)) in entry.tracks.iter().zip(&info.tracks).enumerate() {
        let expected = new.get(&i).cloned().unwrap_or_else(|| slice(original, old));
        if slice(rebuilt, now) != expected || (now.id, &now.language) != (old.id, &old.language) {
            return Err(format!("track {i} reads back differently from the rebuilt {}", entry.label()));
        }
    }
    Ok(())
}

/// `bytes` grown by `extra` bytes inside the file, where its format allows it.
fn pad(format: Container, bytes: &[u8], extra: usize) -> Option<Vec<u8>> {
    match format {
        Container::Riff => riff::pad(bytes, extra),
        Container::Bnk => bnk::pad(bytes, extra),
        Container::Fsb5 => fsb5::pad(bytes, extra),
        Container::Fsb4 | Container::Ogg | Container::Pck => None,
    }
}

/// Size fields to update when `entry`, which ends the input, becomes `new_size` bytes: a
/// 32-bit integer (either byte order) in the input's first 16 bytes or the 64 bytes before
/// the entry that measures the rest of the input from just after itself (a RIFF chunk
/// size), the whole input, or the entry itself; and anywhere before the entry, its size
/// right after its offset (an index entry, like the `SNDH` chunk of an FMOD Studio bank).
/// Found by value, so only values of 16 or more count.
fn length_fields(data: &[u8], entry: &AudioEntry, new_size: u64) -> std::result::Result<Vec<FieldUpdate>, String> {
    if entry.offset == 0 {
        return Ok(Vec::new());
    }
    let (len, offset) = (data.len() as u64, entry.offset);
    let delta = new_size as i64 - entry.size as i64;
    let near = offset.saturating_sub(64)..offset.saturating_sub(3);
    let mut places: Vec<u64> = (0..16.min(offset.saturating_sub(3))).chain(near.clone()).collect();
    places.sort_unstable();
    places.dedup();
    let mut fields = Vec::new();
    for p in places {
        let b: [u8; 4] = data[p as usize..p as usize + 4].try_into().unwrap();
        for big_endian in [false, true] {
            let v = u64::from(if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) });
            let measures = if v >= 16 && p + 4 + v == len {
                "the rest of the input"
            } else if v >= 16 && v == len {
                "the input"
            } else if v >= 16 && v == entry.size && near.contains(&p) {
                "it"
            } else {
                continue;
            };
            let new = u32::try_from(v as i64 + delta).map_err(|_| format!("the size field at {p:#x} can't hold the new size"))?;
            let measures = if measures == "it" { format!("audio {}", entry.id) } else { measures.to_string() };
            fields.push(FieldUpdate { offset: p, big_endian, old: v as u32, new, measures });
            break;
        }
    }
    // An index entry anywhere before it: its offset, then its size (an FMOD bank's SNDH).
    if let (Ok(at), Ok(size), Ok(new)) = (u32::try_from(offset), u32::try_from(entry.size), u32::try_from(new_size)) {
        for big_endian in [false, true] {
            let bytes = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
            let pattern = [bytes(at), bytes(size)].concat();
            for p in memchr::memmem::find_iter(&data[..offset as usize], &pattern) {
                let p = p as u64 + 4;
                if !fields.iter().any(|f| f.offset == p) {
                    let measures = format!("audio {} (after its offset, as in an index)", entry.id);
                    fields.push(FieldUpdate { offset: p, big_endian, old: size, new, measures });
                }
            }
        }
    }
    fields.sort_by_key(|f| f.offset);
    Ok(fields)
}

/// What [`load_edits`] found in an extract folder.
#[derive(Debug, Default)]
pub struct FoundEdits {
    pub edits: Edits,
    /// Files that were there but match the original (extracted and not edited).
    pub unchanged: usize,
}

/// Find edits in a folder written by `extract` (with `--split` for tracks): a file whose
/// bytes no longer match the original, or a track's file in its bank's folder that no
/// longer matches what `--split` wrote. Only one kind per file.
pub fn load_edits(data: &[u8], manifest: &Manifest, dir: &Path) -> Result<FoundEdits> {
    let mut found = FoundEdits::default();
    for entry in &manifest.audio {
        let fail = |reason: String| Error::Edit { id: entry.id, offset: entry.offset, reason };
        let original = audio_bytes(data, entry)?;
        let path = dir.join(&entry.file);
        let whole = match read_if_there(&path)? {
            Some(bytes) if bytes.as_slice() != original => Some(bytes),
            Some(_) => {
                found.unchanged += 1;
                None
            }
            None => None,
        };
        let mut tracks = BTreeMap::new();
        let folder = dir.join(entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(stem, _)| stem));
        for (name, index) in split_files(entry)? {
            let Some(bytes) = read_if_there(&folder.join(&name))? else { continue };
            if track_bytes(entry, original, index)?.as_ref() == bytes.as_slice() {
                found.unchanged += 1;
            } else if entry.format == Container::Fsb4 {
                return Err(fail(format!("{name} was edited, but FSB4 tracks can't be put back one by one yet; replace the whole .fsb")));
            } else {
                tracks.insert(index, bytes);
            }
        }
        match (whole, tracks.is_empty()) {
            (Some(_), false) => {
                return Err(fail(format!("both {} and tracks split out of it were edited; keep one kind of edit per file", entry.file)));
            }
            (Some(bytes), true) => {
                found.edits.insert(entry.id, AudioEdit::File(bytes));
            }
            (None, false) => {
                found.edits.insert(entry.id, AudioEdit::Tracks(tracks));
            }
            (None, true) => {}
        }
    }
    Ok(found)
}

fn read_if_there(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_catches_a_wrong_crc_and_length() {
        let pcm = riff::Pcm { bytes: 2, float: false, signed_8bit: false, big_endian: false };
        let wav = riff::pad(&riff::pcm_wav(pcm, 1, 8000, 4, &[0; 8]).0, 8).unwrap();
        let mut data = b"..".to_vec();
        data.extend_from_slice(&wav);
        let result = PackResult {
            audio: vec![AudioPlan {
                id: 0,
                offset: 2,
                old_size: wav.len() as u64,
                new_size: wav.len() as u64,
                slot: wav.len() as u64,
                outcome: Outcome::Replaced,
                placement: Placement::InPlace,
                notes: Vec::new(),
                crc32: crc32fast::hash(&wav),
                format: Container::Riff,
            }],
            fields: Vec::new(),
            patches: Vec::new(),
            input_len: data.len() as u64,
            output_len: data.len() as u64,
        };
        assert!(result.verify(&data).is_ok());
        let mut bad = data.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(matches!(result.verify(&bad), Err(Error::Verify { id: 0, .. })));
        assert!(matches!(result.verify(&data[..data.len() - 1]), Err(Error::Pack(_))));
    }
}
