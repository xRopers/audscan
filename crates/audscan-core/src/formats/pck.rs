//! Wwise file packages (`.pck`, magic `AKPK`): the files a game streams or loads, packed
//! together. The header gives the sizes of a language map and of lookup tables for
//! SoundBanks, streamed WEMs and (in newer versions) "external" WEMs with 64-bit IDs.
//! Each table entry is `id, block size, file size, start block, language id`, and a file
//! starts at `start block × block size` from the start of the package, so the package
//! ends where its furthest file does. Big-endian packages (older consoles) are told by
//! the version field, which is always 1.
//!
//! Layout as in the Wwise SDK's sample `AkFilePackageLUT` and vgmstream's notes.

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track};
use crate::formats::bnk::{info_from_tracks, summarize, u32_at, u64_at, wem_track};

pub struct Pck;

impl AudioFormat for Pck {
    fn container(&self) -> Container {
        Container::Pck
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"AKPK"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
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
        let mut pos = start + languages;
        for (size, extension) in [(banks, "bnk"), (streams, "wem"), (externals, "wem")] {
            let table = &data[pos as usize..(pos + size) as usize];
            lookup_table(data, table, extension, big_endian, &names, &mut tracks)?;
            pos += size;
        }
        let size = tracks.iter().map(|t| t.offset + t.size).max().unwrap_or(0).max(header_end);
        let codec = summarize(&tracks, if tracks.is_empty() { "empty" } else { "SoundBanks" });
        Ok(info_from_tracks(size, codec, big_endian, tracks))
    }
}

/// Read one lookup table into `tracks`. Entries are 20 bytes (32-bit IDs) or 24 (64-bit),
/// told apart by the table's size.
fn lookup_table(
    data: &[u8],
    table: &[u8],
    extension: &str,
    big_endian: bool,
    names: &[(u32, String)],
    tracks: &mut Vec<Track>,
) -> Result<(), Reject> {
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
    }
    Ok(())
}

/// The language map: a count, then (string offset, language id) pairs, then the strings,
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
