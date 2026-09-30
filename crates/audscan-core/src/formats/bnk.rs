//! Wwise SoundBanks (`.bnk`): a run of sections, each a 4-byte tag and a 32-bit size,
//! starting with `BKHD` (bank version and ID). `DIDX` lists the embedded media (WEM ID,
//! offset into `DATA` and size, 12 bytes each) and `DATA` holds them; `HIRC` and the rest
//! describe events and sound objects and are kept, not read. The bank ends after its last
//! section. Banks from big-endian consoles have big-endian sizes; the byte order is the
//! one that gives a sensible bank version.
//!
//! [`replace_media`] rebuilds a bank around new WEMs, for pack.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track};
use crate::formats::riff::{Riff, parse_prefix};
use crate::formats::{Piece, relayout};

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
        let bank = walk(data)?;
        let tracks: Vec<Track> = media(data, &bank)?.into_iter().map(|m| wem_track(&data[m.range.clone()], m.id.into(), m.range.start as u64)).collect();
        let codec = summarize(&tracks, "no media");
        Ok(info_from_tracks(bank.end as u64, codec, bank.big_endian, tracks))
    }
}

/// A bank's sections, as [`walk`] finds them.
struct Sections {
    big_endian: bool,
    /// Each section's tag and where its body is (its 8-byte header is just before).
    list: Vec<([u8; 4], Range<usize>)>,
    /// Where the bank ends: after its last section.
    end: usize,
}

impl Sections {
    /// The first section with this tag.
    fn body(&self, tag: &[u8; 4]) -> Option<Range<usize>> {
        self.list.iter().find(|(t, _)| t == tag).map(|(_, r)| r.clone())
    }
}

fn walk(data: &[u8]) -> Result<Sections, Reject> {
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
    let mut list = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let tag: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
        // A second BKHD is the next bank.
        if !SECTIONS.contains(&&tag) || (pos > 0 && &tag == b"BKHD") {
            break;
        }
        let body = pos + 8;
        let end = body.saturating_add(u32_at(data, pos + 4, big_endian) as usize);
        if end > data.len() {
            let tag = tag.escape_ascii();
            return Err(Reject::Bad(format!("the {tag} section runs past the end of the file")));
        }
        list.push((tag, body..end));
        pos = end;
    }
    Ok(Sections { big_endian, list, end: pos })
}

/// One DIDX entry with media: its WEM ID, where its bytes are in the bank, and where the
/// entry itself is.
struct Media {
    id: u32,
    range: Range<usize>,
    entry: usize,
}

/// The media DIDX lists, in its order, leaving out empty entries: the bank's tracks.
fn media(data: &[u8], bank: &Sections) -> Result<Vec<Media>, Reject> {
    let Some(didx) = bank.body(b"DIDX") else { return Ok(Vec::new()) };
    let data_section = bank.body(b"DATA").ok_or_else(|| Reject::Bad("a DIDX section but no DATA".into()))?;
    if didx.len() % 12 != 0 {
        return Err(Reject::Bad(format!("the DIDX section is {} bytes, not a multiple of 12", didx.len())));
    }
    let r = |pos: usize| u32_at(data, pos, bank.big_endian);
    let mut list = Vec::new();
    for at in didx.step_by(12) {
        let (id, offset, len) = (r(at), r(at + 4), r(at + 8));
        // Some banks' indexes carry empty entries (ID 0, size 0): nothing to list.
        if len == 0 {
            continue;
        }
        let start = data_section.start + offset as usize;
        let end = start.saturating_add(len as usize);
        if end > data_section.end {
            return Err(Reject::Bad(format!("media {id} runs past the end of the DATA section")));
        }
        list.push(Media { id, range: start..end, entry: at });
    }
    Ok(list)
}

/// Check a replacement for a WEM in a bank or package: a whole WEM in the same byte
/// order. `old` is the WEM it replaces. Returns a note if the codec changes.
pub(crate) fn check_wem(new: &[u8], old: &[u8], big_endian: bool) -> Result<Option<String>, String> {
    let old_info = parse_prefix(old).ok();
    if let Some(note) = old_info.as_ref().and_then(|i| i.note.as_deref()).filter(|n| n.starts_with("prefetch")) {
        return Err(format!("it's prefetch media ({note}); replace the whole sound where it's streamed from, a .wem or .pck"));
    }
    let info = Riff.parse(new).map_err(|e| match e {
        Reject::Bad(reason) => format!("the replacement isn't a usable WEM: {reason}"),
        Reject::NoMatch => "the replacement isn't a WEM (a Wwise RIFF file)".to_string(),
    })?;
    if info.size != new.len() as u64 {
        return Err(format!("the replacement is {} bytes, but its RIFF header says {}", new.len(), info.size));
    }
    if !info.wwise {
        return Err(format!("the replacement is a plain WAV ({}), not a WEM; make a WEM with Wwise first", info.codec));
    }
    if info.big_endian != big_endian {
        let order = |be| if be { "big-endian (RIFX)" } else { "little-endian (RIFF)" };
        return Err(format!("the replacement is {}, but the bank is {}", order(info.big_endian), order(big_endian)));
    }
    Ok(old_info.filter(|o| o.codec != info.codec).map(|o| {
        format!("the codec changes from {} to {}; the game's sound objects may still expect {}", o.codec, info.codec, o.codec)
    }))
}

/// Write a u32 in the given byte order.
pub(crate) fn put_u32(out: &mut [u8], at: usize, v: u32, big_endian: bool) {
    out[at..at + 4].copy_from_slice(&if big_endian { v.to_be_bytes() } else { v.to_le_bytes() });
}

/// The bank with some of its media (by track index) replaced by new WEMs. DIDX and DATA
/// are rewritten with the media moved to fit, keeping their order and alignment; the
/// sections after DATA move along. A changed size is also updated where a sound object in
/// HIRC records it (the source ID followed by its in-memory size). Returns the bank and
/// notes.
pub(crate) fn replace_media(bank: &[u8], new: &BTreeMap<usize, Vec<u8>>) -> Result<(Vec<u8>, Vec<String>), String> {
    let sections = walk(bank).map_err(|_| "the bank no longer reads as a SoundBank".to_string())?;
    let list = media(bank, &sections).map_err(|e| format!("the bank can't be read: {e:?}"))?;
    let be = sections.big_endian;
    let mut notes = Vec::new();
    for (&t, bytes) in new {
        let m = list.get(t).ok_or_else(|| format!("the bank has no track {t}"))?;
        let what = format!("track {t} (media {})", m.id);
        if let Some(note) = check_wem(bytes, &bank[m.range.clone()], be).map_err(|e| format!("{what}: {e}"))? {
            notes.push(format!("{what}: {note}"));
        }
    }
    let (Some(didx_section), Some(data_section)) = (sections.body(b"DIDX"), sections.body(b"DATA")) else {
        return Err("the bank holds no media".into());
    };

    // The distinct ranges in DATA, in order; entries sharing one move together.
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by_key(|&i| (list[i].range.start, list[i].range.end));
    let mut ranges: Vec<(Range<usize>, Vec<usize>)> = Vec::new();
    for i in order {
        match ranges.last_mut() {
            Some((r, tracks)) if *r == list[i].range => tracks.push(i),
            Some((r, _)) if r.end > list[i].range.start => {
                return Err(format!("media {} overlaps other media in DATA, so they can't be moved", list[i].id));
            }
            _ => ranges.push((list[i].range.clone(), vec![i])),
        }
    }
    if let Some((_, shared)) = ranges.iter().find(|(_, tracks)| tracks.len() > 1 && tracks.iter().any(|t| new.contains_key(t))) {
        return Err(format!("tracks {shared:?} share their data, so one can't be replaced alone"));
    }
    // Wwise aligns media to 16 bytes in DATA; keep whatever alignment they all have, up to that.
    let align = ranges
        .iter()
        .map(|(r, _)| r.start - data_section.start)
        .filter(|&o| o > 0)
        .map(|o| 1u64 << o.trailing_zeros().min(4))
        .min()
        .unwrap_or(16);
    let pieces: Vec<Piece> = ranges
        .iter()
        .map(|(r, tracks)| Piece {
            start: (r.start - data_section.start) as u64,
            end: (r.end - data_section.start) as u64,
            bytes: new.get(&tracks[0]).map_or(&bank[r.clone()], Vec::as_slice),
            align,
        })
        .collect();
    let (new_data, starts) = relayout(&bank[data_section.clone()], 0, &pieces);

    let mut didx = bank[didx_section.clone()].to_vec();
    for ((_, tracks), (start, piece)) in ranges.iter().zip(starts.iter().zip(&pieces)) {
        let start = u32::try_from(*start).map_err(|_| "the DATA section would pass 4 GiB".to_string())?;
        for &i in tracks {
            let at = list[i].entry - didx_section.start;
            put_u32(&mut didx, at + 4, start, be);
            put_u32(&mut didx, at + 8, piece.bytes.len() as u32, be);
        }
    }

    let mut hirc_updates = 0;
    let mut out = Vec::with_capacity(bank.len());
    for (tag, body) in &sections.list {
        let mut contents = match tag {
            _ if *body == didx_section => didx.clone(),
            _ if *body == data_section => new_data.clone(),
            _ => bank[body.clone()].to_vec(),
        };
        if tag == b"HIRC" {
            for (&t, bytes) in new {
                let m = &list[t];
                let (old_len, new_len) = (m.range.len() as u32, bytes.len() as u32);
                if old_len != new_len {
                    hirc_updates += update_hirc_size(&mut contents, m.id, old_len, new_len, be);
                }
            }
        }
        let size = u32::try_from(contents.len()).map_err(|_| "a section would pass 4 GiB".to_string())?;
        out.extend_from_slice(tag);
        out.extend_from_slice(&[0; 4]);
        let at = out.len() - 4;
        put_u32(&mut out, at, size, be);
        out.extend(contents);
    }
    if hirc_updates > 0 {
        notes.push(format!("{hirc_updates} sound object(s) in HIRC updated with the new media size"));
    }
    Ok((out, notes))
}

/// Where HIRC holds `id` followed by `old` (a sound object's source ID and in-memory media
/// size), put `new`. Returns how many it changed.
fn update_hirc_size(hirc: &mut [u8], id: u32, old: u32, new: u32, be: bool) -> usize {
    let bytes = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
    let mut pattern = bytes(id).to_vec();
    pattern.extend_from_slice(&bytes(old));
    let found: Vec<usize> = memchr::memmem::find_iter(hirc, &pattern).collect();
    for &at in &found {
        hirc[at + 4..at + 8].copy_from_slice(&bytes(new));
    }
    found.len()
}

/// The bank grown by `extra` bytes, as zeros at the end of its DATA section (where nothing
/// points), or `None` if it has no DATA section.
pub(crate) fn pad(bank: &[u8], extra: usize) -> Option<Vec<u8>> {
    let sections = walk(bank).ok()?;
    let data = sections.body(b"DATA")?;
    let size = u32::try_from(data.len() + extra).ok()?;
    let mut out = bank[..data.end].to_vec();
    put_u32(&mut out, data.start - 4, size, sections.big_endian);
    out.resize(out.len() + extra, 0);
    out.extend_from_slice(&bank[data.end..]);
    Some(out)
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
