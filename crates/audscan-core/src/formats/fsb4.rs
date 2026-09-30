//! FMOD sound banks, version 4 (`FSB4`): FMOD Ex's `.fsb` (games from about 2006 to 2013).
//! A 0x30-byte header gives the track count, the size of the track headers and of the
//! sample data, so the bank's size is their sum. Each track has a header of its own: its
//! size (0x50 and up, with codec extras such as DSP coefficients after), a 30-byte name,
//! its length in samples and in bytes, loop points, mode flags (which say the codec),
//! sample rate and channels. With "basic headers" only the first track has a full one;
//! the others give just their two lengths and share the rest.
//!
//! The data follows, one track after another, each padded to an alignment the header
//! doesn't state: 32 bytes is usual, 16 and none are seen. It's worked out from the data
//! size, the one alignment for which the tracks' lengths add up to it.
//!
//! Layout as documented by vgmstream's `fsb.c` and FMOD Ex's `fmod.h` flags.

use std::ops::Range;

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track, u16_le, u32_le};
use crate::formats::riff::{Pcm, Riff, pcm_wav};

pub struct Fsb4;

const HEADER: usize = 0x30;
/// The smallest full track header: up to and including the variation fields.
const FULL_HEADER: usize = 0x50;

/// Bank flags.
const BASIC_HEADERS: u32 = 0x02;
const BIG_ENDIAN_PCM: u32 = 0x08;
const NOT_INTERLEAVED: u32 = 0x10;

/// Track mode flags.
const BITS_8: u32 = 0x0000_0008;
const STEREO: u32 = 0x0000_0040;
const UNSIGNED: u32 = 0x0000_0080;
const MPEG: u32 = 0x0000_0200;
const MPEG_LAYER2: u32 = 0x0004_0000;
const BITS_32: u32 = 0x0020_0000;
const IMA_ADPCM: u32 = 0x0040_0000;
const VAG: u32 = 0x0080_0000;
const XMA: u32 = 0x0100_0000;
const GC_ADPCM: u32 = 0x0200_0000;
const CELT: u32 = 0x0800_0000;
const MPEG_LAYER3: u32 = 0x1000_0000;

/// The codec a track's mode flags name, and for PCM its sample width in bytes.
fn codec(mode: u32) -> (&'static str, Option<u16>) {
    let has = |flag: u32| mode & flag != 0;
    if has(MPEG | MPEG_LAYER2 | MPEG_LAYER3) {
        ("MPEG", None)
    } else if has(IMA_ADPCM) {
        ("IMA ADPCM", None)
    } else if has(VAG) {
        ("VAG", None)
    } else if has(XMA) {
        ("XMA", None)
    } else if has(GC_ADPCM) {
        ("GameCube ADPCM", None)
    } else if has(CELT) {
        ("CELT", None)
    } else if has(BITS_8) {
        ("PCM 8-bit", Some(1))
    } else if has(BITS_32) {
        ("PCM 32-bit", Some(4))
    } else {
        ("PCM 16-bit", Some(2))
    }
}

/// What splitting a track needs besides its [`Track`].
struct Layout {
    flags: u32,
    /// Each track's header bytes (a full header, or 8 bytes of a basic one).
    entries: Vec<Range<usize>>,
    /// Each track's mode flags (a basic header shares the first track's).
    modes: Vec<u32>,
}

impl AudioFormat for Fsb4 {
    fn container(&self) -> Container {
        Container::Fsb4
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"FSB4"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
        read(data).map(|(info, _)| info)
    }
}

fn read(data: &[u8]) -> Result<(AudioInfo, Layout), Reject> {
    if data.len() < HEADER {
        return Err(Reject::NoMatch);
    }
    let (count, headers, sample_data) = (u32_le(data, 4), u32_le(data, 8) as usize, u32_le(data, 0xC) as u64);
    let (version, flags) = (u32_le(data, 0x10), u32_le(data, 0x14));
    if version >> 16 != 4 {
        return Err(Reject::NoMatch);
    }
    if count == 0 {
        return Err(Reject::Bad("no tracks".into()));
    }
    let size = (HEADER + headers) as u64 + sample_data;
    if size > data.len() as u64 {
        return Err(Reject::Bad(format!("runs past the end of the file ({size} bytes declared, {} left)", data.len())));
    }

    let table_end = HEADER + headers;
    let cut_off = |i: u32| Reject::Bad(format!("track {i}'s header is cut off"));
    let (mut tracks, mut entries, mut modes, mut lengths) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut first: Option<(u32, u32, u16)> = None; // mode, rate, channels of the full header
    let mut pos = HEADER;
    for i in 0..count {
        let (samples, length, name, mode, rate, channels, entry);
        match first {
            Some((m, r, c)) if flags & BASIC_HEADERS != 0 => {
                if pos + 8 > table_end {
                    return Err(cut_off(i));
                }
                (samples, length, name, mode, rate, channels) = (u32_le(data, pos), u32_le(data, pos + 4), None, m, r, c);
                entry = pos..pos + 8;
            }
            _ => {
                if pos + 2 > table_end {
                    return Err(cut_off(i));
                }
                let len = usize::from(u16_le(data, pos));
                if len < FULL_HEADER || pos + len > table_end {
                    return Err(cut_off(i));
                }
                let h = &data[pos..pos + len];
                let raw_name = &h[2..0x20];
                let end = raw_name.iter().position(|&b| b == 0).unwrap_or(raw_name.len());
                name = std::str::from_utf8(&raw_name[..end]).ok().filter(|n| !n.is_empty()).map(String::from);
                (samples, length, mode, rate) = (u32_le(h, 0x20), u32_le(h, 0x24), u32_le(h, 0x30), u32_le(h, 0x34));
                channels = match u16_le(h, 0x3E) {
                    0 if mode & STEREO != 0 => 2,
                    0 => 1,
                    c => c,
                };
                first.get_or_insert((mode, rate, channels));
                entry = pos..pos + len;
            }
        }
        pos = entry.end;
        entries.push(entry);
        modes.push(mode);
        lengths.push(u64::from(length));
        let (codec_name, pcm) = codec(mode);
        // Planar multichannel PCM isn't a WAV's layout: keep it in a bank.
        let wav = pcm.is_some() && (channels == 1 || flags & NOT_INTERLEAVED == 0);
        tracks.push(Track {
            name,
            codec: Some(codec_name.into()),
            extension: Some(if wav { "wav" } else { "fsb" }.into()),
            channels,
            sample_rate: rate,
            samples: Some(samples.into()),
            size: length.into(),
            ..Track::default()
        });
    }

    // Place the data: each track padded to the alignment that makes the lengths add up
    // to the data size (the last one's padding, if any, is inside it too).
    let total = |align: u64| {
        let (last, rest) = lengths.split_last().unwrap();
        rest.iter().map(|l| l.next_multiple_of(align)).sum::<u64>() + last
    };
    let fits = |align: u64| total(align) <= sample_data && sample_data - total(align) < align;
    let (align, note) = match [32, 16, 1].into_iter().find(|&a| fits(a)) {
        Some(a) => (a, None),
        None if total(1) <= sample_data => (1, Some("the tracks' lengths don't add up to the data size".to_string())),
        None => return Err(Reject::Bad(format!("the tracks need {} bytes of data; the header says {sample_data}", total(1)))),
    };
    let mut offset = table_end as u64;
    for t in &mut tracks {
        t.offset = offset;
        offset += t.size.next_multiple_of(align);
    }

    let codecs: Vec<&str> = tracks.iter().filter_map(|t| t.codec.as_deref()).collect();
    let codec = if codecs.iter().all(|&c| c == codecs[0]) { codecs[0] } else { "mixed" };
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
        note,
    };
    Ok((info, Layout { flags, entries, modes }))
}

/// Track `index` of a bank as a file of its own: interleaved PCM as a WAV, anything else
/// as a bank of one track (the bank's header, the track's full header and its data). A
/// track with a basic header gets the first track's full header with its own lengths, no
/// name, and loop points reset to the whole track. The file is read back and must
/// describe the same sound.
pub fn split_track(bank: &[u8], index: usize) -> Result<Vec<u8>, String> {
    let (info, layout) = read(bank).map_err(|_| "the bank no longer reads as FSB4".to_string())?;
    let track = info.tracks.get(index).ok_or_else(|| format!("the bank has no track {index}"))?;
    let data = &bank[track.offset as usize..(track.offset + track.size) as usize];
    let mode = layout.modes[index];
    let (file, back, samples) = if track.extension.as_deref() == Some("wav") {
        let pcm = Pcm {
            bytes: codec(mode).1.unwrap_or(2),
            float: false,
            signed_8bit: mode & UNSIGNED == 0,
            big_endian: layout.flags & BIG_ENDIAN_PCM != 0,
        };
        let (file, frames) = pcm_wav(pcm, track.channels, track.sample_rate, track.samples.unwrap_or(0), data);
        let back = Riff.parse(&file);
        (file, back, Some(frames))
    } else {
        let file = one_track_bank(bank, &layout, index, track, data);
        let back = Fsb4.parse(&file);
        (file, back, track.samples)
    };
    let back = back.map_err(|e| format!("track {index} doesn't read back once split: {e:?}"))?;
    let name = back.tracks.first().and_then(|t| t.name.clone());
    let same = back.size == file.len() as u64
        && (back.channels, back.sample_rate, back.samples) == (track.channels, track.sample_rate, samples)
        && (track.extension.as_deref() == Some("wav") || name == track.name);
    if !same {
        return Err(format!("track {index} reads back differently once split"));
    }
    Ok(file)
}

fn one_track_bank(bank: &[u8], layout: &Layout, index: usize, track: &Track, data: &[u8]) -> Vec<u8> {
    let entry = &bank[layout.entries[index].clone()];
    let full = if entry.len() >= FULL_HEADER {
        entry.to_vec()
    } else {
        // A basic header: the first track's full one, with this track's lengths.
        let mut full = bank[layout.entries[0].clone()].to_vec();
        full[2..0x20].fill(0);
        full[0x20..0x24].copy_from_slice(&entry[0..4]);
        full[0x24..0x28].copy_from_slice(&entry[4..8]);
        let samples = u32::from_le_bytes(entry[0..4].try_into().unwrap());
        full[0x28..0x2C].copy_from_slice(&0u32.to_le_bytes());
        full[0x2C..0x30].copy_from_slice(&samples.saturating_sub(1).to_le_bytes());
        full
    };
    let mut out = bank[..HEADER].to_vec();
    out[4..8].copy_from_slice(&1u32.to_le_bytes());
    out[8..12].copy_from_slice(&(full.len() as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
    let flags = layout.flags & !BASIC_HEADERS;
    out[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
    out.extend(full);
    out.extend_from_slice(data);
    debug_assert_eq!(track.size, data.len() as u64);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codecs_from_mode_flags() {
        assert_eq!(codec(MPEG | STEREO), ("MPEG", None));
        assert_eq!(codec(0x10 | STEREO), ("PCM 16-bit", Some(2)));
        assert_eq!(codec(BITS_8), ("PCM 8-bit", Some(1)));
        assert_eq!(codec(GC_ADPCM), ("GameCube ADPCM", None));
    }

    #[test]
    fn text_is_not_a_bank() {
        assert_eq!(Fsb4.parse(b"FSB4 is FMOD Ex's sound bank format, from 2006 on"), Err(Reject::NoMatch));
    }
}
