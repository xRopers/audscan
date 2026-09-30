//! Writing the audio a manifest lists to files, exactly as it is in the input.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::error::{Error, Result, io_err};
use crate::format::Container;
use crate::formats::fsb5::split_track;
use crate::manifest::{AudioEntry, Manifest};

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Refuse to extract if the input's size or CRC differs from the manifest's.
    pub verify_source: bool,
    /// Also write each track of a bank or package to a folder named after it: Wwise WEMs
    /// and SoundBanks as they are, FSB5 tracks as WAV (PCM) or one-track FSB5 files. See
    /// [`Track::split_filename`] and [`crate::formats::fsb5::split_track`].
    pub split: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self { verify_source: true, split: false }
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
            let folder = out_dir.join(entry.file.rsplit_once('.').map_or(entry.file.as_str(), |(stem, _)| stem));
            let mut split = Vec::with_capacity(splits.len());
            for (name, index) in splits {
                let track = &entry.tracks[index];
                let file = match entry.format {
                    Container::Fsb5 => Cow::Owned(
                        split_track(bytes, index).map_err(|reason| Error::Audio { id: entry.id, offset: entry.offset, reason })?,
                    ),
                    _ => Cow::Borrowed(&bytes[track.offset as usize..(track.offset + track.size) as usize]),
                };
                let path = folder.join(&name);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(io_err(parent))?;
                }
                fs::write(&path, &file).map_err(io_err(&path))?;
                split.push(path);
            }
            Ok(ExtractedFile { id: entry.id, offset: entry.offset, path, size: bytes.len() as u64, split })
        })
        .collect()
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
