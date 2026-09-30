//! Wwise file packages (`.pck`, magic `AKPK`): the files a game streams or loads, packed
//! together. The header gives the sizes of a language map and of lookup tables for
//! SoundBanks, streamed WEMs and (in newer versions) "external" WEMs with 64-bit IDs.
//! Each table entry is `id, block size, file size, start block, language id`, and a file
//! starts at `start block × block size` from the start of the package, so the package
//! ends where its furthest file does. Big-endian packages (older consoles) are told by
//! the version field, which is always 1.
//!
//! [`replace_files`] rebuilds a package around new files, for pack.
//!
//! Layout as in the Wwise SDK's sample `AkFilePackageLUT` and vgmstream's notes.

use std::collections::BTreeMap;

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track};
use crate::formats::bnk::{Bnk, check_wem, info_from_tracks, put_u32, summarize, u32_at, u64_at, wem_track};
use crate::formats::{Piece, relayout};

pub struct Pck;

impl AudioFormat for Pck {
    fn container(&self) -> Container {
        Container::Pck
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"AKPK"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
        read(data).map(|p| p.info)
    }
}

/// A package as [`read`] finds it.
struct Package {
    info: AudioInfo,
    /// Where each track's table entry keeps its block size, file size, start block and
    /// language (4 bytes each), by track.
    fields: Vec<usize>,
    /// Where the header (and the lookup tables) end.
    header_end: u64,
}

fn read(data: &[u8]) -> Result<Package, Reject> {
    if data.len() < 28 {
        return Err(Reject::NoMatch);
    }
    let big_endian = match (u32_at(data, 8, false), u32_at(data, 8, true)) {
        (1, _) => false,
        (_, 1) => true,
        _ => return Err(Reject::NoMatch),
    };
    let r = |pos: usize| u64::from(u32_at(data, pos, big_endian));
    let header_end = 8 + r(4);
    if header_end > data.len() as u64 {
        return Err(Reject::Bad(format!("the header runs past the end of the file ({header_end} bytes)")));
    }
    let (languages, banks, streams) = (r(12), r(16), r(20));
    let tables = 24 + languages + banks + streams;
    let (start, externals) = if tables == header_end {
        (24, 0)
    } else if tables + 4 + r(24) == header_end {
        (28, r(24))
    } else {
        return Err(Reject::Bad("the header's table sizes don't add up to its size".into()));
    };

    let language_map = &data[start as usize..(start + languages) as usize];
    let names = language_names(language_map, big_endian);
    let mut tracks = Vec::new();
    let mut fields = Vec::new();
    let mut pos = start + languages;
    for (size, extension) in [(banks, "bnk"), (streams, "wem"), (externals, "wem")] {
        let table = pos as usize..(pos + size) as usize;
        lookup_table(data, table, extension, big_endian, &names, &mut tracks, &mut fields)?;
        pos += size;
    }
    let size = tracks.iter().map(|t| t.offset + t.size).max().unwrap_or(0).max(header_end);
    let codec = summarize(&tracks, if tracks.is_empty() { "empty" } else { "SoundBanks" });
    Ok(Package { info: info_from_tracks(size, codec, big_endian, tracks), fields, header_end })
}

/// Read one lookup table (at `range` in `data`) into `tracks`, and where each entry's
/// fields are into `fields`. Entries are 20 bytes (32-bit IDs) or 24 (64-bit), told apart
/// by the table's size.
fn lookup_table(
    data: &[u8],
    range: std::ops::Range<usize>,
    extension: &str,
    big_endian: bool,
    names: &[(u32, String)],
    tracks: &mut Vec<Track>,
    fields: &mut Vec<usize>,
) -> Result<(), Reject> {
    let table = &data[range.clone()];
    if table.is_empty() {
        return Ok(());
    }
    let count = if table.len() >= 4 { u32_at(table, 0, big_endian) as usize } else { usize::MAX };
    if count == 0 {
        return Ok(());
    }
    let entry = (table.len().saturating_sub(4)) / count.max(1);
    if count == usize::MAX || entry * count != table.len() - 4 || !(entry == 20 || entry == 24) {
        return Err(Reject::Bad(format!("a lookup table of {} bytes can't hold {count} entries", table.len())));
    }
    for i in 0..count {
        let at = 4 + i * entry;
        let id = if entry == 24 { u64_at(table, at, big_endian) } else { u32_at(table, at, big_endian).into() };
        let field = |n: usize| u64::from(u32_at(table, at + entry - 16 + 4 * n, big_endian));
        let (block, size, start_block, language) = (field(0), field(1), field(2), field(3));
        if block == 0 {
            return Err(Reject::Bad(format!("file {id} has a block size of 0")));
        }
        let offset = start_block * block;
        let end = offset + size;
        if end > data.len() as u64 {
            return Err(Reject::Bad(format!("file {id} runs past the end of the input")));
        }
        let bytes = &data[offset as usize..end as usize];
        let mut track = match extension {
            "wem" => wem_track(bytes, id, offset),
            _ => Track {
                id: Some(id),
                codec: Some("SoundBank".into()),
                extension: Some(extension.into()),
                offset,
                size,
                ..Track::default()
            },
        };
        track.language = names.iter().find(|(n, _)| u64::from(*n) == language).map(|(_, name)| name.clone());
        tracks.push(track);
        fields.push(range.start + at + entry - 16);
    }
    Ok(())
}

/// The package with some of its files (by track index) replaced: WEMs by WEMs, SoundBanks
/// by SoundBanks, in the package's byte order. The files after a changed one move to fit,
/// each keeping its block alignment, and the lookup tables get the new sizes and start
/// blocks. Returns the package and notes.
pub(crate) fn replace_files(package: &[u8], new: &BTreeMap<usize, Vec<u8>>) -> Result<(Vec<u8>, Vec<String>), String> {
    let pkg = read(package).map_err(|_| "the package no longer reads as a Wwise package".to_string())?;
    let (tracks, be) = (&pkg.info.tracks, pkg.info.big_endian);
    let range = |t: &Track| t.offset as usize..(t.offset + t.size) as usize;
    let mut notes = Vec::new();
    for (&i, bytes) in new {
        let t = tracks.get(i).ok_or_else(|| format!("the package has no track {i}"))?;
        let what = format!("track {i} ({}.{})", t.display_name(), t.extension.as_deref().unwrap_or(""));
        let note = if t.extension.as_deref() == Some("bnk") {
            check_bank(bytes, be)
        } else {
            check_wem(bytes, &package[range(t)], be)
        };
        if let Some(note) = note.map_err(|e| format!("{what}: {e}"))? {
            notes.push(format!("{what}: {note}"));
        }
    }

    // The distinct files, in order; entries sharing one move together.
    let block = |i: usize| u64::from(u32_at(package, pkg.fields[i], be));
    let mut order: Vec<usize> = (0..tracks.len()).collect();
    order.sort_by_key(|&i| (tracks[i].offset, tracks[i].size));
    let mut files: Vec<(std::ops::Range<usize>, Vec<usize>)> = Vec::new();
    for i in order {
        let r = range(&tracks[i]);
        match files.last_mut() {
            Some((prev, shared)) if *prev == r => shared.push(i),
            Some((prev, _)) if prev.end > r.start => {
                return Err(format!("file {} overlaps another file, so they can't be moved", tracks[i].display_name()));
            }
            _ => files.push((r, vec![i])),
        }
    }
    if let Some((_, shared)) = files.iter().find(|(_, s)| s.len() > 1 && s.iter().any(|i| new.contains_key(i))) {
        return Err(format!("tracks {shared:?} share their data, so one can't be replaced alone"));
    }
    let pieces: Vec<Piece> = files
        .iter()
        .map(|(r, shared)| Piece {
            start: r.start as u64,
            end: r.end as u64,
            bytes: new.get(&shared[0]).map_or(&package[r.clone()], Vec::as_slice),
            align: shared.iter().map(|&i| block(i)).max().unwrap_or(1),
        })
        .collect();
    let end = pkg.info.size as usize;
    let header_end = pkg.header_end as usize;
    if files.first().is_some_and(|(r, _)| r.start < header_end) {
        return Err("a file starts inside the header, so the files can't be moved".into());
    }
    let (region, starts) = relayout(&package[header_end..end], pkg.header_end, &pieces);

    let mut out = package[..header_end].to_vec();
    out.extend(region);
    for ((_, shared), (&start, piece)) in files.iter().zip(starts.iter().zip(&pieces)) {
        for &i in shared {
            let start_block = u32::try_from(start / block(i)).map_err(|_| "the package would pass its size limit".to_string())?;
            let size = u32::try_from(piece.bytes.len()).map_err(|_| "a file would pass 4 GiB".to_string())?;
            put_u32(&mut out, pkg.fields[i] + 4, size, be);
            put_u32(&mut out, pkg.fields[i] + 8, start_block, be);
        }
    }
    Ok((out, notes))
}

/// Check a replacement SoundBank: a whole bank in the package's byte order.
fn check_bank(new: &[u8], big_endian: bool) -> Result<Option<String>, String> {
    let info = Bnk.parse(new).map_err(|e| match e {
        Reject::Bad(reason) => format!("the replacement isn't a usable SoundBank: {reason}"),
        Reject::NoMatch => "the replacement isn't a SoundBank".to_string(),
    })?;
    if info.size != new.len() as u64 {
        return Err(format!("the replacement has {} bytes after the end of the bank", new.len() as u64 - info.size));
    }
    if info.big_endian != big_endian {
        return Err("the replacement bank's byte order differs from the package's".into());
    }
    Ok(None)
}

// The language map: a count, then (string offset, language id) pairs, then the strings,
/// UTF-16 in most packages and 8-bit in some. Anything unreadable is left out.
fn language_names(map: &[u8], big_endian: bool) -> Vec<(u32, String)> {
    if map.len() < 4 {
        return Vec::new();
    }
    let count = u32_at(map, 0, big_endian) as usize;
    (0..count)
        .map_while(|i| {
            let at = 4 + 8 * i;
            (at + 8 <= map.len()).then(|| (u32_at(map, at, big_endian) as usize, u32_at(map, at + 4, big_endian)))
        })
        .filter_map(|(offset, id)| Some((id, string_at(map.get(offset..)?)?)))
        .collect()
}

fn string_at(bytes: &[u8]) -> Option<String> {
    let text = match bytes {
        [_, 0, ..] => {
            let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).take_while(|&u| u != 0).collect();
            String::from_utf16(&units).ok()?
        }
        [0, _, ..] => {
            let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c)).take_while(|&u| u != 0).collect();
            String::from_utf16(&units).ok()?
        }
        _ => String::from_utf8(bytes.iter().copied().take_while(|&b| b != 0).collect()).ok()?,
    };
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_strings_in_either_width() {
        assert_eq!(string_at(b"s\0f\0x\0\0\0").as_deref(), Some("sfx"));
        assert_eq!(string_at(b"\0e\0n\0\0").as_deref(), Some("en"));
        assert_eq!(string_at(b"english(us)\0").as_deref(), Some("english(us)"));
        assert_eq!(string_at(b"\0\0"), None);
    }

    #[test]
    fn text_is_not_a_package() {
        assert_eq!(Pck.parse(b"AKPK is the magic of a Wwise file package"), Err(Reject::NoMatch));
    }
}
