//! FMOD sound banks, version 5 (`FSB5`): FMOD Studio's `.fsb`, also found inside `.bank`
//! files. The header gives the sizes of the track headers, the name table and the sample
//! data, so the bank's size is their sum. One codec for the whole bank. Each track header
//! packs its sample rate, channels, data offset and length into 64 bits, followed by
//! optional extra chunks (channels, sample rate, loop points, codec setup...).
//!
//! Layout as documented by vgmstream's `fsb5.c`: a 0x3C-byte header (0x40 in version 0),
//! then the track headers, the name table (one offset per track, then NUL-terminated
//! names) and the sample data.
//!
//! A track can be split out ([`split_track`]): PCM as a WAV, anything else as a bank of
//! one track, since the raw codec data (Vorbis without its setup headers, XMA, ADPCM...)
//! can't be played without the header describing it. [`replace_tracks`] puts new tracks
//! in, for pack.

use std::collections::BTreeMap;
use std::ops::Range;

use memchr::memchr;

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track, u32_le, u64_le};
use crate::formats::riff::{Pcm, Riff, pcm_wav, wav_data};
use crate::formats::{Piece, relayout};

pub struct Fsb5;

/// Sample rates by the 4-bit index in a track header.
const RATES: [u32; 11] = [4000, 8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000];

/// Channels by the 2-bit code in a track header.
const CHANNELS: [u16; 4] = [1, 2, 6, 8];

/// Extra-chunk types that override the header's packed values.
const CHUNK_CHANNELS: u32 = 1;
const CHUNK_RATE: u32 = 2;

fn codec_name(mode: u32) -> Option<&'static str> {
    Some(match mode {
        1 => "PCM 8-bit",
        2 => "PCM 16-bit",
        3 => "PCM 24-bit",
        4 => "PCM 32-bit",
        5 => "PCM float",
        6 => "GameCube ADPCM",
        7 => "IMA ADPCM",
        8 => "VAG",
        9 => "HEVAG",
        10 => "XMA",
        11 => "MPEG",
        12 => "CELT",
        13 => "ATRAC9",
        14 => "xWMA",
        15 => "Vorbis",
        16 => "FMOD ADPCM",
        17 => "Opus",
        _ => return None,
    })
}

impl AudioFormat for Fsb5 {
    fn container(&self) -> Container {
        Container::Fsb5
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"FSB5"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
        read(data).map(|(info, _)| info)
    }
}

/// What splitting a track needs besides its [`Track`]: where things are in the bank.
struct Layout {
    /// Bytes of the fixed header (0x3C, or 0x40 in version 0).
    header: usize,
    /// The codec number.
    mode: u32,
    /// Each track's header bytes: the packed u64 and its extra chunks.
    entries: Vec<Range<usize>>,
}

/// Codec numbers of the PCM formats (8-, 16-, 24-, 32-bit and float), split out as WAV.
const PCM: std::ops::RangeInclusive<u32> = 1..=5;

fn read(data: &[u8]) -> Result<(AudioInfo, Layout), Reject> {
    if data.len() < 8 {
        return Err(Reject::NoMatch);
    }
    let version = u32_le(data, 4);
    if version > 1 {
        return Err(Reject::NoMatch);
    }
    let header = if version == 0 { 0x40 } else { 0x3C };
    if data.len() < header {
        return Err(Reject::Bad("the header is cut off by the end of the file".into()));
    }
    let count = u32_le(data, 8);
    let (headers, names, sample_data) = (u32_le(data, 0xC) as usize, u32_le(data, 0x10) as usize, u32_le(data, 0x14));
    let mode = u32_le(data, 0x18);
    let codec = codec_name(mode).ok_or_else(|| Reject::Bad(format!("unknown codec {mode}")))?;
    if count == 0 {
        return Err(Reject::Bad("no tracks".into()));
    }
    if (headers as u64) < u64::from(count) * 8 {
        return Err(Reject::Bad(format!("{headers} bytes of track headers can't hold {count} tracks")));
    }
    let size = (header + headers + names) as u64 + u64::from(sample_data);
    if size > data.len() as u64 {
        return Err(Reject::Bad(format!("runs past the end of the file ({size} bytes declared, {} left)", data.len())));
    }

    let table_end = header + headers;
    let data_start = (table_end + names) as u64;
    let cut_off = |i: u32| Reject::Bad(format!("track {i}'s header is cut off"));
    let mut tracks = Vec::with_capacity(count as usize);
    let mut entries = Vec::with_capacity(count as usize);
    let extension = if PCM.contains(&mode) { "wav" } else { "fsb" };
    let mut pos = header;
    for i in 0..count {
        if pos + 8 > table_end {
            return Err(cut_off(i));
        }
        let entry_start = pos;
        let packed = u64_le(data, pos);
        pos += 8;
        let mut channels = CHANNELS[(packed >> 5 & 3) as usize];
        let mut sample_rate = RATES.get((packed >> 1 & 0xF) as usize).copied().unwrap_or(44100);
        let offset = (packed >> 7 & 0x07FF_FFFF) << 5;
        let samples = packed >> 34;
        let mut more = packed & 1 != 0;
        while more {
            if pos + 4 > table_end {
                return Err(cut_off(i));
            }
            let chunk = u32_le(data, pos);
            let (len, kind) = ((chunk >> 1 & 0xFF_FFFF) as usize, chunk >> 25);
            more = chunk & 1 != 0;
            let body = pos + 4;
            if body + len > table_end {
                return Err(cut_off(i));
            }
            match kind {
                CHUNK_CHANNELS if len >= 1 => channels = u16::from(data[body]),
                CHUNK_RATE if len >= 4 => sample_rate = u32_le(data, body),
                _ => {}
            }
            pos = body + len;
        }
        if offset > u64::from(sample_data) {
            return Err(Reject::Bad(format!("track {i}'s data starts past the end of the sample data")));
        }
        entries.push(entry_start..pos);
        tracks.push(Track {
            extension: Some(extension.into()),
            channels,
            sample_rate,
            samples: Some(samples),
            offset: data_start + offset,
            ..Track::default()
        });
    }

    // Each track runs to the next one's data (or the end of the sample data).
    let mut starts: Vec<u64> = tracks.iter().map(|t| t.offset).collect();
    starts.sort_unstable();
    starts.push(size);
    for t in &mut tracks {
        let next = starts[starts.partition_point(|&s| s <= t.offset)];
        t.size = next - t.offset;
    }

    if names >= 4 * count as usize {
        let table = &data[table_end..table_end + names];
        for (i, t) in tracks.iter_mut().enumerate() {
            let at = u32_le(table, 4 * i) as usize;
            if let Some(rest) = table.get(at..) {
                let name = &rest[..memchr(0, rest).unwrap_or(rest.len())];
                t.name = std::str::from_utf8(name).ok().filter(|n| !n.is_empty()).map(String::from);
            }
        }
    }

    let first = &tracks[0];
    let info = AudioInfo {
        size,
        codec: codec.to_string(),
        channels: first.channels,
        sample_rate: first.sample_rate,
        samples: if count == 1 { first.samples } else { None },
        big_endian: false,
        wwise: false,
        tracks,
        note: None,
    };
    Ok((info, Layout { header, mode, entries }))
}

/// Track `index` of a bank as a file of its own: PCM as a WAV, anything else as a bank of
/// one track (the bank's header, the track's header with its data offset zeroed, its name
/// and its data, all copied). The file is read back and must describe the same sound.
pub fn split_track(bank: &[u8], index: usize) -> Result<Vec<u8>, String> {
    let (info, layout) = read(bank).map_err(|_| "the bank no longer reads as FSB5".to_string())?;
    let track = info.tracks.get(index).ok_or_else(|| format!("the bank has no track {index}"))?;
    let data = &bank[track.offset as usize..(track.offset + track.size) as usize];
    let (file, back, samples) = if PCM.contains(&layout.mode) {
        let pcm = Pcm { bytes: (layout.mode as u16).min(4), float: layout.mode == 5, signed_8bit: true, big_endian: false };
        let (file, frames) = pcm_wav(pcm, track.channels, track.sample_rate, track.samples.unwrap_or(0), data);
        let back = Riff.parse(&file);
        (file, back, Some(frames))
    } else {
        let file = one_track_bank(bank, &layout, index, track, data);
        let back = Fsb5.parse(&file);
        (file, back, track.samples)
    };
    let back = back.map_err(|e| format!("track {index} doesn't read back once split: {e:?}"))?;
    let name = back.tracks.first().and_then(|t| t.name.clone());
    let same = back.size == file.len() as u64
        && (back.channels, back.sample_rate, back.samples) == (track.channels, track.sample_rate, samples)
        && (PCM.contains(&layout.mode) || name == track.name);
    if !same {
        return Err(format!("track {index} reads back differently once split"));
    }
    Ok(file)
}

fn one_track_bank(bank: &[u8], layout: &Layout, index: usize, track: &Track, data: &[u8]) -> Vec<u8> {
    let entry = &bank[layout.entries[index].clone()];
    // The track's data now starts the sample data: offset 0.
    let packed = u64_le(entry, 0) & !(0x07FF_FFFF << 7);
    let mut headers = packed.to_le_bytes().to_vec();
    headers.extend_from_slice(&entry[8..]);
    let mut names = Vec::new();
    if let Some(name) = &track.name {
        names.extend_from_slice(&4u32.to_le_bytes());
        names.extend_from_slice(name.as_bytes());
        names.push(0);
        names.resize(names.len().next_multiple_of(4), 0);
    }
    let mut out = bank[..layout.header].to_vec();
    for (at, value) in [(0x8, 1), (0xC, headers.len()), (0x10, names.len()), (0x14, data.len())] {
        out[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    out.extend(headers);
    out.extend(names);
    out.extend_from_slice(data);
    out
}

/// The data-offset bits of a packed track header (offset / 32, bits 7 to 33).
const OFFSET_BITS: u64 = 0x07FF_FFFF << 7;

/// Extra-chunk type of loop points (start and end sample).
const CHUNK_LOOP: u32 = 3;

/// A track header's extra chunks, as (type, body); `entry` is the packed u64 and its
/// chunks, as [`read`] checked them.
fn extra_chunks(entry: &[u8]) -> Vec<(u32, &[u8])> {
    let mut chunks = Vec::new();
    let mut more = u64_le(entry, 0) & 1 != 0;
    let mut pos = 8;
    while more && pos + 4 <= entry.len() {
        let chunk = u32_le(entry, pos);
        let len = (chunk >> 1 & 0xFF_FFFF) as usize;
        more = chunk & 1 != 0;
        chunks.push((chunk >> 25, &entry[pos + 4..(pos + 4 + len).min(entry.len())]));
        pos += 4 + len;
    }
    chunks
}

/// A track header: the packed u64 (data offset 0) and its extra chunks.
fn track_entry(rate_index: u64, channel_code: u64, samples: u64, chunks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let packed = u64::from(!chunks.is_empty()) | rate_index << 1 | channel_code << 5 | samples << 34;
    let mut out = packed.to_le_bytes().to_vec();
    for (i, (kind, body)) in chunks.iter().enumerate() {
        let more = u32::from(i + 1 < chunks.len());
        out.extend_from_slice(&(more | (body.len() as u32) << 1 | kind << 25).to_le_bytes());
        out.extend_from_slice(body);
    }
    out
}

/// A replacement track: its header (data offset 0), its data, and what it should read as.
struct NewTrack {
    entry: Vec<u8>,
    data: Vec<u8>,
    channels: u16,
    sample_rate: u32,
    samples: u64,
}

/// The bank with some of its tracks (by index) replaced. A replacement is a one-track FSB5
/// bank of the same codec (its track header, extra chunks included, and its data are
/// used), or for a PCM bank also a WAV of the bank's sample format. Tracks keep their
/// names and order; the sample data is laid out again with each track 32-byte aligned.
/// The result is read back and checked. Returns the bank and notes.
pub(crate) fn replace_tracks(bank: &[u8], new: &BTreeMap<usize, Vec<u8>>) -> Result<(Vec<u8>, Vec<String>), String> {
    let (info, layout) = read(bank).map_err(|_| "the bank no longer reads as FSB5".to_string())?;
    let codec = codec_name(layout.mode).unwrap_or("?");
    let mut notes = Vec::new();
    let mut replaced = BTreeMap::new();
    for (&i, file) in new {
        let t = info.tracks.get(i).ok_or_else(|| format!("the bank has no track {i}"))?;
        let what = format!("track {i} ({})", t.name.as_deref().unwrap_or("no name"));
        let track = if file.starts_with(b"FSB5") {
            track_from_bank(file, layout.mode)
        } else if file.starts_with(b"RIFF") && PCM.contains(&layout.mode) {
            track_from_wav(file, layout.mode, &bank[layout.entries[i].clone()], &mut notes, &what)
        } else if file.starts_with(b"RIFF") {
            Err(format!("this bank is {codec}, so a WAV can't go in; give a one-track FSB5 bank of {codec} (FMOD's FSBank makes them)"))
        } else {
            Err("the replacement isn't an FSB5 bank or a WAV".to_string())
        };
        replaced.insert(i, track.map_err(|e| format!("{what}: {e}"))?);
    }

    let headers = u32_le(bank, 0xC) as usize;
    let names = u32_le(bank, 0x10) as usize;
    let data_start = layout.header + headers + names;
    // The distinct data offsets, in order; tracks sharing one move together.
    let mut order: Vec<usize> = (0..info.tracks.len()).collect();
    order.sort_by_key(|&i| info.tracks[i].offset);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in order {
        match groups.last_mut() {
            Some(g) if info.tracks[g[0]].offset == info.tracks[i].offset => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    if let Some(shared) = groups.iter().find(|g| g.len() > 1 && g.iter().any(|i| replaced.contains_key(i))) {
        return Err(format!("tracks {shared:?} share their data, so one can't be replaced alone"));
    }
    let pieces: Vec<Piece> = groups
        .iter()
        .map(|g| {
            let t = &info.tracks[g[0]];
            let (start, end) = (t.offset - data_start as u64, t.offset + t.size - data_start as u64);
            let old = &bank[t.offset as usize..(t.offset + t.size) as usize];
            Piece { start, end, bytes: replaced.get(&g[0]).map_or(old, |n: &NewTrack| &n.data), align: 32 }
        })
        .collect();
    let (sample_data, starts) = relayout(&bank[data_start..info.size as usize], 0, &pieces);

    let mut piece_of = vec![0; info.tracks.len()];
    for (p, g) in groups.iter().enumerate() {
        g.iter().for_each(|&i| piece_of[i] = p);
    }
    let mut table = Vec::with_capacity(headers);
    for (i, range) in layout.entries.iter().enumerate() {
        let entry = replaced.get(&i).map_or(&bank[range.clone()], |n| &n.entry);
        let units = starts[piece_of[i]] / 32;
        if units << 7 & !OFFSET_BITS != 0 {
            return Err("the sample data would grow past what FSB5 can address".into());
        }
        table.extend_from_slice(&(u64_le(entry, 0) & !OFFSET_BITS | units << 7).to_le_bytes());
        table.extend_from_slice(&entry[8..]);
    }
    let mut out = bank[..layout.header].to_vec();
    let too_big = |_| "the bank would pass 4 GiB".to_string();
    out[0xC..0x10].copy_from_slice(&u32::try_from(table.len()).map_err(too_big)?.to_le_bytes());
    out[0x14..0x18].copy_from_slice(&u32::try_from(sample_data.len()).map_err(too_big)?.to_le_bytes());
    out.extend(table);
    out.extend_from_slice(&bank[layout.header + headers..data_start]);
    out.extend(sample_data);

    // Read it back: every track where it should be, the new ones as intended.
    let (back, _) = read(&out).map_err(|e| format!("the new bank doesn't read back: {e:?}"))?;
    if back.tracks.len() != info.tracks.len() {
        return Err("the new bank reads back with a different number of tracks".into());
    }
    for (i, (t, old)) in back.tracks.iter().zip(&info.tracks).enumerate() {
        let intended = pieces[piece_of[i]].bytes;
        let held = &out[t.offset as usize..(t.offset + t.size) as usize];
        // A track's data runs to the next one's, so either may have padding after it.
        let same_data = held.starts_with(intended) || intended.starts_with(held);
        let sound = match replaced.get(&i) {
            Some(n) => (n.channels, n.sample_rate, Some(n.samples)),
            None => (old.channels, old.sample_rate, old.samples),
        };
        if !same_data || (t.channels, t.sample_rate, t.samples) != sound || t.name != old.name {
            return Err(format!("track {i} reads back differently from the new bank"));
        }
    }
    Ok((out, notes))
}

/// A one-track bank's track, for a bank of codec `mode`.
fn track_from_bank(file: &[u8], mode: u32) -> Result<NewTrack, String> {
    let (info, layout) = read(file).map_err(|e| match e {
        Reject::Bad(reason) => format!("the replacement isn't a usable FSB5 bank: {reason}"),
        Reject::NoMatch => "the replacement isn't an FSB5 bank".to_string(),
    })?;
    if info.size != file.len() as u64 {
        return Err(format!("the replacement has {} bytes after the end of its bank", file.len() as u64 - info.size));
    }
    if info.tracks.len() != 1 {
        return Err(format!("the replacement bank has {} tracks; give a bank of one", info.tracks.len()));
    }
    if layout.mode != mode {
        let name = |m| codec_name(m).unwrap_or("?");
        return Err(format!("the replacement is {}, but the bank is {}; they must match", name(layout.mode), name(mode)));
    }
    let t = &info.tracks[0];
    let entry = file[layout.entries[0].clone()].to_vec();
    let data = file[t.offset as usize..(t.offset + t.size) as usize].to_vec();
    Ok(NewTrack { entry, data, channels: t.channels, sample_rate: t.sample_rate, samples: t.samples.unwrap_or(0) })
}

/// A WAV's samples as a track of a PCM bank of codec `mode`, keeping the old track
/// header's extra chunks (loop points only if they still fit).
fn track_from_wav(file: &[u8], mode: u32, old_entry: &[u8], notes: &mut Vec<String>, what: &str) -> Result<NewTrack, String> {
    let (info, data) = wav_data(file).map_err(|e| match e {
        Reject::Bad(reason) => format!("the replacement isn't a usable WAV: {reason}"),
        Reject::NoMatch => "the replacement isn't a WAV".to_string(),
    })?;
    let bits = [8, 16, 24, 32, 32][(mode - 1) as usize];
    let expected = if mode == 5 { "IEEE float 32-bit".to_string() } else { format!("PCM {bits}-bit") };
    if info.codec != expected || info.wwise {
        return Err(format!("the replacement is {}{}, but this bank holds {expected}; convert it to that", info.codec, if info.wwise { " (a WEM)" } else { "" }));
    }
    let frame = bits / 8 * usize::from(info.channels);
    let frames = data.len() / frame;
    let mut samples = file[data.start..data.start + frames * frame].to_vec();
    if mode == 1 {
        // WAV's 8-bit samples are unsigned, FSB's signed.
        samples.iter_mut().for_each(|b| *b ^= 0x80);
    }
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();
    for (kind, body) in extra_chunks(old_entry) {
        match kind {
            CHUNK_CHANNELS | CHUNK_RATE => {}
            CHUNK_LOOP if body.len() >= 8 && u64::from(u32_le(body, 4)) >= frames as u64 => {
                notes.push(format!("{what}: its loop points were past the new end, so they were dropped"));
            }
            _ => chunks.push((kind, body.to_vec())),
        }
    }
    let rate_index = match RATES.iter().position(|&r| r == info.sample_rate) {
        Some(i) => i as u64,
        None => {
            chunks.push((CHUNK_RATE, info.sample_rate.to_le_bytes().to_vec()));
            u64_le(old_entry, 0) >> 1 & 0xF
        }
    };
    let channel_code = match CHANNELS.iter().position(|&c| c == info.channels) {
        Some(i) => i as u64,
        None => {
            let channels = u8::try_from(info.channels).map_err(|_| format!("{} channels is more than FSB5 holds", info.channels))?;
            chunks.push((CHUNK_CHANNELS, vec![channels]));
            0
        }
    };
    if frames as u64 >= 1 << 30 {
        return Err("the replacement is longer than FSB5 can hold".into());
    }
    Ok(NewTrack {
        entry: track_entry(rate_index, channel_code, frames as u64, &chunks),
        data: samples,
        channels: info.channels,
        sample_rate: info.sample_rate,
        samples: frames as u64,
    })
}

/// The bank grown by `extra` zero bytes at the end of its sample data.
pub(crate) fn pad(bank: &[u8], extra: usize) -> Option<Vec<u8>> {
    let (info, _) = read(bank).ok()?;
    let size = u32::try_from(u64::from(u32_le(bank, 0x14)) + extra as u64).ok()?;
    let mut out = bank[..info.size as usize].to_vec();
    out[0x14..0x18].copy_from_slice(&size.to_le_bytes());
    out.resize(out.len() + extra, 0);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version 1 bank of PCM16 with one track header, as given.
    fn bank(packed: u64, extra: &[u8], data_len: u32) -> Vec<u8> {
        let mut out = b"FSB5".to_vec();
        let headers = 8 + extra.len() as u32;
        for v in [1, 1, headers, 0, data_len, 2] {
            out.extend_from_slice(&u32::to_le_bytes(v));
        }
        out.resize(0x3C, 0);
        out.extend_from_slice(&packed.to_le_bytes());
        out.extend_from_slice(extra);
        out.resize(out.len() + data_len as usize, 0);
        out
    }

    #[test]
    fn packed_track_header() {
        // 48 kHz (index 9), stereo (code 1), 1000 samples.
        let info = Fsb5.parse(&bank(9 << 1 | 1 << 5 | 1000 << 34, &[], 64)).unwrap();
        assert_eq!((info.channels, info.sample_rate, info.samples), (2, 48000, Some(1000)));
        assert_eq!((info.tracks[0].offset, info.tracks[0].size), (0x3C + 8, 64));
    }

    #[test]
    fn extra_chunks_override_it() {
        // A channels chunk (3) and then a rate chunk (12345).
        let mut chunks = (1u32 | 1 << 1 | CHUNK_CHANNELS << 25).to_le_bytes().to_vec();
        chunks.push(3);
        chunks.extend_from_slice(&(4u32 << 1 | CHUNK_RATE << 25).to_le_bytes());
        chunks.extend_from_slice(&12345u32.to_le_bytes());
        let info = Fsb5.parse(&bank(1 | 10 << 34, &chunks, 32)).unwrap();
        assert_eq!((info.channels, info.sample_rate), (3, 12345));
        // A chunk running past the headers is an error, not a panic.
        let err = Fsb5.parse(&bank(1, &chunks[..6], 32)).unwrap_err();
        assert_eq!(err, Reject::Bad("track 0's header is cut off".into()));
    }
}
