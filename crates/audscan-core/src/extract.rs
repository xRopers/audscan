//! Writing the audio a manifest lists to files, exactly as it is in the input.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::error::{Error, Result, io_err};
use crate::convert::{ConvertError, convert_wem};
use crate::format::Container;
use crate::formats::{fsb4, fsb5};
use crate::manifest::{AudioEntry, Manifest};

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Refuse to extract if the input's size or CRC differs from the manifest's.
    pub verify_source: bool,
    /// Also write each track of a bank or package to a folder named after it: Wwise WEMs
    /// and SoundBanks as they are, FSB4 and FSB5 tracks as WAV (PCM) or one-track banks.
    /// See [`Track::split_filename`], [`fsb4::split_track`] and [`fsb5::split_track`].
    pub split: bool,
    /// Also convert every WEM written (extracted or split out) to Ogg or WAV next to it,
    /// with [`convert_wem`]. A WEM that can't be converted is noted, not an error.
    pub convert: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self { verify_source: true, split: false, convert: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFile {
    pub id: u32,
    pub offset: u64,
    pub path: PathBuf,
    pub size: u64,
    /// Files split out of it (with `split`).
    pub split: Vec<PathBuf>,
    /// Ogg or WAV files converted from it or from WEMs split out of it (with `convert`).
    pub converted: Vec<PathBuf>,
    /// WEMs that couldn't be converted, and why.
    pub not_converted: Vec<(PathBuf, ConvertError)>,
    /// Notes on converted files (see [`crate::Converted::note`]).
    pub convert_notes: Vec<(PathBuf, String)>,
}

impl ExtractedFile {
    /// Convert the WEM just written to `path` (its bytes are `wem`), recording the result.
    fn convert(&mut self, path: &Path, wem: &[u8]) -> Result<()> {
        match convert_wem(wem) {
            Ok(c) => {
                let out = path.with_extension(c.extension);
                fs::write(&out, &c.bytes).map_err(io_err(&out))?;
                if let Some(note) = c.note {
                    self.convert_notes.push((out.clone(), note));
                }
                self.converted.push(out);
            }
            Err(e) => self.not_converted.push((path.to_path_buf(), e)),
        }
        Ok(())
    }
}

/// The bytes of one file, checked against the manifest's CRC.
pub fn audio_bytes<'a>(data: &'a [u8], entry: &AudioEntry) -> Result<&'a [u8]> {
    let fail = |reason: String| Error::Audio { id: entry.id, offset: entry.offset, reason };
    let bytes = usize::try_from(entry.end())
        .ok()
        .and_then(|end| data.get(entry.offset as usize..end))
        .ok_or_else(|| fail(format!("runs past the end of the input ({} bytes)", data.len())))?;
    let crc = crc32fast::hash(bytes);
    if crc != entry.crc32 {
        return Err(fail(format!("CRC-32 is {crc:08x}, manifest expects {:08x}", entry.crc32)));
    }
    Ok(bytes)
}

pub fn extract_all(data: &[u8], manifest: &Manifest, out_dir: &Path, opts: &ExtractOptions) -> Result<Vec<ExtractedFile>> {
    if opts.verify_source {
        manifest.source.check(data)?;
    }
    // Validate everything before writing anything.
    if let Some(bad) = manifest.audio.iter().find(|a| !is_safe_filename(&a.file)) {
        return Err(Error::BadFilename(bad.file.clone()));
    }
    let splits: Vec<Vec<(String, usize)>> = manifest
        .audio
        .iter()
        .map(|a| if opts.split { split_files(a) } else { Ok(Vec::new()) })
        .collect::<Result<_>>()?;
    let slices = manifest.audio.iter().map(|a| audio_bytes(data, a)).collect::<Result<Vec<_>>>()?;
    fs::create_dir_all(out_dir).map_err(io_err(out_dir))?;
    manifest
        .audio
        .par_iter()
        .zip(slices)
        .zip(splits)
        .map(|((entry, bytes), splits)| {
            let path = out_dir.join(&entry.file);
            fs::write(&path, bytes).map_err(io_err(&path))?;
            let mut done = ExtractedFile {
                id: entry.id,
                offset: entry.offset,
                path: path.clone(),
                size: bytes.len() as u64,
                split: Vec::new(),
                converted: Vec::new(),
                not_converted: Vec::new(),
                convert_notes: Vec::new(),
            };
            if opts.convert && entry.format == Container::Riff && entry.wwise {
                done.convert(&path, bytes)?;
            }
            let folder = out_dir.join(entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(stem, _)| stem));
            for (name, index) in splits {
                let track = &entry.tracks[index];
                let file = track_bytes(entry, bytes, index)?;
                let path = folder.join(&name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(io_err(parent))?;
                }
                fs::write(&path, &file).map_err(io_err(&path))?;
                if opts.convert && track.extension.as_deref() == Some("wem") {
                    done.convert(&path, &file)?;
                }
                done.split.push(path);
            }
            Ok(done)
        })
        .collect()
}

/// Track `index` of a bank or package as a file of its own, as `split` writes it: a Wwise
/// WEM or SoundBank as it is, an FSB track as a WAV or one-track bank. `bytes` is the
/// entry's whole file (from [`audio_bytes`]).
pub fn track_bytes<'a>(entry: &AudioEntry, bytes: &'a [u8], index: usize) -> Result<Cow<'a, [u8]>> {
    let fail = |reason: String| Error::Audio { id: entry.id, offset: entry.offset, reason };
    let track = entry.tracks.get(index).ok_or_else(|| fail(format!("has no track {index}")))?;
    match entry.format {
        Container::Fsb4 => fsb4::split_track(bytes, index).map(Cow::Owned).map_err(fail),
        Container::Fsb5 => fsb5::split_track(bytes, index).map(Cow::Owned).map_err(fail),
        _ => {
            let range = usize::try_from(track.offset).ok().zip(usize::try_from(track.offset + track.size).ok());
            let slice = range.and_then(|(start, end)| bytes.get(start..end));
            slice.map(Cow::Borrowed).ok_or_else(|| fail(format!("track {index} lies outside it")))
        }
    }
}

/// The files `split` writes for one entry, by relative path and track index: every track
/// that can be split out, checked to stay inside the bank's folder and to lie inside the
/// bank. Tracks that would get the same name (ignoring case, as Windows does) get their
/// index added.
fn split_files(entry: &AudioEntry) -> Result<Vec<(String, usize)>> {
    let fail = |reason: String| Error::Audio { id: entry.id, offset: entry.offset, reason };
    let mut files = Vec::new();
    let mut used = HashSet::new();
    for (index, track) in entry.tracks.iter().enumerate() {
        let Some(mut name) = track.split_filename(index) else { continue };
        if !used.insert(name.to_lowercase()) {
            let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
            name = format!("{stem}_{index}.{ext}");
            used.insert(name.to_lowercase());
        }
        let plain_extension = track.extension.as_deref().is_some_and(|e| e.bytes().all(|b| b.is_ascii_alphanumeric()));
        if !plain_extension || !name.split('/').all(is_safe_filename) {
            return Err(Error::BadFilename(name));
        }
        if track.offset.checked_add(track.size).is_none_or(|end| end > entry.size) {
            return Err(fail(format!("{name} lies outside it")));
        }
        files.push((name, index));
    }
    Ok(files)
}

/// A single file name with no directory parts, so a manifest can't write outside the
/// extract directory.
pub fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && Path::new(name).file_name().is_some_and(|f| f == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filenames() {
        assert!(is_safe_filename("00001234.wem"));
        for bad in ["", ".", "..", "../x.wav", "a/b.ogg", "a\\b.ogg", "C:x.wav"] {
            assert!(!is_safe_filename(bad), "{bad:?}");
        }
    }
}
