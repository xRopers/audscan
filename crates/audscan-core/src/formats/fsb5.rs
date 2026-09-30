//! FMOD sound banks, version 5 (`FSB5`): FMOD Studio's `.fsb`, also found inside `.bank`
//! files. The header gives the sizes of the track headers, the name table and the sample
//! data, so the bank's size is their sum. One codec for the whole bank. Each track header
//! packs its sample rate, channels, data offset and length into 64 bits, followed by
//! optional extra chunks (channels, sample rate, loop points, codec setup...).
//!
//! Layout as documented by vgmstream's `fsb5.c`: a 0x3C-byte header (0x40 in version 0),
//! then the track headers, the name table (one offset per track, then NUL-terminated
//! names) and the sample data.

use memchr::memchr;

use crate::format::{AudioFormat, AudioInfo, Container, Reject, Track, u32_le, u64_le};

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
        let mut pos = header;
        for i in 0..count {
            if pos + 8 > table_end {
                return Err(cut_off(i));
            }
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
            tracks.push(Track { channels, sample_rate, samples: Some(samples), offset: data_start + offset, ..Track::default() });
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
        Ok(AudioInfo {
            size,
            codec: codec.to_string(),
            channels: first.channels,
            sample_rate: first.sample_rate,
            samples: if count == 1 { first.samples } else { None },
            big_endian: false,
            wwise: false,
            tracks,
            note: None,
        })
    }
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
