//! Wwise SoundBanks (`.bnk`): a run of sections, each a 4-byte tag and a 32-bit size,
//! starting with `BKHD` (bank version and ID). `DIDX` lists the embedded media (WEM ID,
//! offset into `DATA` and size, 12 bytes each) and `DATA` holds them; `HIRC` and the rest
//! describe events and sound objects and are kept, not read. The bank ends after its last
//! section. Banks from big-endian consoles have big-endian sizes; the byte order is the
//! one that gives a sensible bank version.

use std::ops::Range;

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track};
use crate::formats::riff::parse_prefix;

pub struct Bnk;

/// Section tags a bank can hold, across Wwise versions.
const SECTIONS: [&[u8; 4]; 10] = [b"BKHD", b"DIDX", b"DATA", b"HIRC", b"STID", b"STMG", b"ENVS", b"FXPR", b"PLAT", b"INIT"];

/// Bank versions go up to about 0x9A (Wwise 2024); a larger value isn't a bank version in
/// this byte order.
const MAX_VERSION: u32 = 0x1000;

/// A u32 at `pos` in the given byte order; the caller has checked the length.
pub(crate) fn u32_at(data: &[u8], pos: usize, big_endian: bool) -> u32 {
    let b = data[pos..pos + 4].try_into().unwrap();
    if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
}

/// A u64 at `pos` in the given byte order; the caller has checked the length.
pub(crate) fn u64_at(data: &[u8], pos: usize, big_endian: bool) -> u64 {
    let b = data[pos..pos + 8].try_into().unwrap();
    if big_endian { u64::from_be_bytes(b) } else { u64::from_le_bytes(b) }
}

/// A WEM inside a bank or package, described from its own RIFF header when it has one.
/// Banks can hold just the start of a streamed WEM (prefetch media): its header still
/// describes the whole sound, and the track's note says it's partial.
pub(crate) fn wem_track(bytes: &[u8], id: u64, offset: u64) -> Track {
    let track = Track { id: Some(id), extension: Some("wem".into()), offset, size: bytes.len() as u64, ..Track::default() };
    match parse_prefix(bytes) {
        Ok(info) => Track {
            codec: Some(info.codec),
            note: info.note,
            channels: info.channels,
            sample_rate: info.sample_rate,
            samples: info.samples,
            ..track
        },
        Err(_) => Track { codec: Some("unknown".into()), ..track },
    }
}

/// The codec of every WEM among `tracks` if they share one, `mixed` if not, or `none`.
pub(crate) fn summarize(tracks: &[Track], none: &str) -> String {
    let mut codecs = tracks.iter().filter(|t| t.extension.as_deref() == Some("wem")).filter_map(|t| t.codec.as_deref());
    match codecs.next() {
        None => none.to_string(),
        Some(first) if codecs.all(|c| c == first) => first.to_string(),
        Some(_) => "mixed".to_string(),
    }
}

/// Bank-level details from its tracks: channels and rate of the first audio one, and the
/// length when there's exactly one.
pub(crate) fn info_from_tracks(size: u64, codec: String, big_endian: bool, tracks: Vec<Track>) -> AudioInfo {
    let first = tracks.iter().find(|t| t.sample_rate > 0);
    AudioInfo {
        size,
        codec,
        channels: first.map_or(0, |t| t.channels),
        sample_rate: first.map_or(0, |t| t.sample_rate),
        samples: if tracks.len() == 1 { tracks[0].samples } else { None },
        big_endian,
        wwise: true,
        tracks,
        note: None,
    }
}

impl AudioFormat for Bnk {
    fn container(&self) -> Container {
        Container::Bnk
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"BKHD"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
        if data.len() < 16 {
            return Err(Reject::NoMatch);
        }
        let sensible = |v: u32| (1..=MAX_VERSION).contains(&v);
        let big_endian = match (u32_at(data, 8, false), u32_at(data, 8, true)) {
            (v, _) if sensible(v) => false,
            (_, v) if sensible(v) => true,
            _ => return Err(Reject::NoMatch),
        };
        // BKHD holds a few fields plus alignment padding: never large.
        if !(8..=0x10000).contains(&u32_at(data, 4, big_endian)) {
            return Err(Reject::NoMatch);
        }

        let (mut didx, mut media): (Option<Range<usize>>, Option<Range<usize>>) = (None, None);
        let mut pos = 0;
        while pos + 8 <= data.len() {
            let tag: &[u8; 4] = data[pos..pos + 4].try_into().unwrap();
            // A second BKHD is the next bank.
            if !SECTIONS.contains(&tag) || (pos > 0 && tag == b"BKHD") {
                break;
            }
            let body = pos + 8;
            let end = body.saturating_add(u32_at(data, pos + 4, big_endian) as usize);
            if end > data.len() {
                let tag = tag.escape_ascii();
                return Err(Reject::Bad(format!("the {tag} section runs past the end of the file")));
            }
            match tag {
                b"DIDX" => didx = didx.or(Some(body..end)),
                b"DATA" => media = media.or(Some(body..end)),
                _ => {}
            }
            pos = end;
        }

        let mut tracks = Vec::new();
        if let Some(didx) = didx {
            let media = media.ok_or_else(|| Reject::Bad("a DIDX section but no DATA".into()))?;
            if didx.len() % 12 != 0 {
                return Err(Reject::Bad(format!("the DIDX section is {} bytes, not a multiple of 12", didx.len())));
            }
            for at in didx.step_by(12) {
                let (id, offset, len) = (u32_at(data, at, big_endian), u32_at(data, at + 4, big_endian), u32_at(data, at + 8, big_endian));
                // Some banks' indexes carry empty entries (ID 0, size 0): nothing to list.
                if len == 0 {
                    continue;
                }
                let start = media.start + offset as usize;
                let end = start.saturating_add(len as usize);
                if end > media.end {
                    return Err(Reject::Bad(format!("media {id} runs past the end of the DATA section")));
                }
                tracks.push(wem_track(&data[start..end], id.into(), start as u64));
            }
        }
        let codec = summarize(&tracks, "no media");
        Ok(info_from_tracks(pos as u64, codec, big_endian, tracks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = tag.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn header() -> Vec<u8> {
        section(b"BKHD", &[0x88, 0, 0, 0, 1, 2, 3, 4])
    }

    #[test]
    fn a_bank_without_media_ends_after_its_last_section() {
        let mut bank = header();
        bank.extend(section(b"HIRC", &[0; 20]));
        let size = bank.len();
        bank.extend_from_slice(b"junk after the bank");
        let info = Bnk.parse(&bank).unwrap();
        assert_eq!((info.size, info.codec.as_str(), info.tracks.len()), (size as u64, "no media", 0));
        assert_eq!(info.duration(), None);
    }

    #[test]
    fn media_must_be_inside_data() {
        let mut bank = header();
        let entry: Vec<u8> = [7u32, 8, 16].iter().flat_map(|v| v.to_le_bytes()).collect();
        bank.extend(section(b"DIDX", &entry));
        bank.extend(section(b"DATA", &[0; 20]));
        assert_eq!(Bnk.parse(&bank), Err(Reject::Bad("media 7 runs past the end of the DATA section".into())));
    }

    #[test]
    fn empty_index_entries_are_skipped() {
        let mut bank = header();
        let entry: Vec<u8> = [0u32, 0, 0, 7, 0, 4].iter().flat_map(|v| v.to_le_bytes()).collect();
        bank.extend(section(b"DIDX", &entry));
        bank.extend(section(b"DATA", &[0; 4]));
        let info = Bnk.parse(&bank).unwrap();
        assert_eq!(info.tracks.iter().map(|t| t.id).collect::<Vec<_>>(), [Some(7)]);
        // Media that isn't a WEM is listed, as unknown.
        assert_eq!(info.codec, "unknown");
    }

    #[test]
    fn text_is_not_a_bank() {
        assert_eq!(Bnk.parse(b"BKHD is the bank header section"), Err(Reject::NoMatch));
    }
}
