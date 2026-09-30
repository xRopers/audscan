//! RIFF and RIFX WAVE: `.wav` files and Wwise `.wem` (RIFX is the big-endian form Wwise
//! writes for big-endian consoles). The RIFF header gives the whole size; the `fmt ` chunk
//! gives the codec, channels and sample rate. Other RIFF forms (AVI, WebP, FMOD's `FEV `
//! banks...) aren't audio and are left alone, so an FSB5 inside a bank is still found.
//!
//! Wwise is recognised by its own codec tags (Vorbis 0xFFFF, Opus, PTADPCM), an `akd `
//! chunk, or by being RIFX at all: Wwise is practically the only writer of RIFX WAVE.

use std::ops::Range;

use crate::format::{AudioFormat, AudioInfo, Container, Reject};

pub struct Riff;

/// Codec tags only Wwise uses.
const WWISE_CODECS: [u16; 5] = [0xFFFF, 0x3039, 0x3040, 0x3041, 0x8311];

/// Reads numbers in the file's byte order.
#[derive(Clone, Copy)]
struct Reader<'a> {
    data: &'a [u8],
    big_endian: bool,
}

impl Reader<'_> {
    fn u16(self, pos: usize) -> u16 {
        let b = [self.data[pos], self.data[pos + 1]];
        if self.big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
    }

    fn u32(self, pos: usize) -> u32 {
        let b = self.data[pos..pos + 4].try_into().unwrap();
        if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    }
}

impl AudioFormat for Riff {
    fn container(&self) -> Container {
        Container::Riff
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"RIFF", b"RIFX"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
        parse_wave(data, false)
    }
}

/// Read a WAVE header whose file may be cut short: Wwise banks keep only the start of a
/// streamed WEM ("prefetch" media) and stream the rest. The size is what's there, the
/// details (and the length) come from the header, and the note says it's partial.
pub(crate) fn parse_prefix(data: &[u8]) -> Result<AudioInfo, Reject> {
    parse_wave(data, true)
}

fn parse_wave(data: &[u8], prefix: bool) -> Result<AudioInfo, Reject> {
    if data.len() < 12 || !matches!(&data[8..12], b"WAVE" | b"XWMA") {
        return Err(Reject::NoMatch);
    }
    let r = Reader { data, big_endian: data[3] == b'X' };
    let declared = u64::from(r.u32(4));
    if declared < 4 {
        return Err(Reject::Bad(format!("the RIFF size is {declared}")));
    }
    let declared_size = 8 + declared;
    let mut note = None;
    let mut end = if declared_size <= data.len() as u64 {
        declared_size as usize
    } else if prefix {
        note = Some(format!("prefetch: the first {} of {declared_size} bytes (the rest is streamed)", data.len()));
        data.len()
    } else {
        return Err(Reject::Bad(format!("runs past the end of the file ({declared_size} bytes declared, {} left)", data.len())));
    };

    let mut chunks = Chunks::default();
    let mut pos = 12;
    while pos + 8 <= end {
        let id: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
        let body = pos + 8;
        let len = r.u32(pos + 4);
        let mut body_end = body.saturating_add(len as usize);
        if body_end > end {
            // Some writers get the RIFF size wrong (Unreal's test WAVs count a 20-byte
            // fmt chunk as 16), but a data chunk that still fits shows the real end.
            if &id == b"data" && body_end <= data.len() {
                note = Some(format!("the RIFF header says {declared_size} bytes; the data chunk runs to {body_end}"));
                end = body_end;
            } else if prefix && data.len() < declared_size as usize {
                body_end = end;
            } else {
                let id = id.escape_ascii().to_string();
                return Err(Reject::Bad(format!("the {id:?} chunk runs past the end of the RIFF data")));
            }
        }
        let range = Some(body..body_end);
        match &id {
            b"fmt " => chunks.fmt = chunks.fmt.or(range),
            b"data" if chunks.data.is_none() => (chunks.data, chunks.data_len) = (range, u64::from(len)),
            b"fact" => chunks.fact = chunks.fact.or(range),
            b"vorb" => chunks.vorb = chunks.vorb.or(range),
            b"akd " => chunks.akd = true,
            _ => {}
        }
        // Chunks are padded to an even length.
        pos = body_end + (body_end - body) % 2;
    }

    let fmt = chunks.fmt.ok_or_else(|| Reject::Bad("no fmt chunk".into()))?;
    if chunks.data.is_none() {
        return Err(Reject::Bad("no data chunk".into()));
    }
    if fmt.len() < 14 {
        return Err(Reject::Bad(format!("the fmt chunk is only {} bytes", fmt.len())));
    }
    let f = fmt.start;
    let mut tag = r.u16(f);
    let (channels, sample_rate, block_align) = (r.u16(f + 2), r.u32(f + 4), r.u16(f + 12));
    let bits = if fmt.len() >= 16 { r.u16(f + 14) } else { 0 };
    // WAVE_FORMAT_EXTENSIBLE: the real tag starts the subformat GUID.
    if tag == 0xFFFE && fmt.len() >= 40 {
        tag = r.u16(f + 24);
    }
    if channels == 0 || sample_rate == 0 {
        return Err(Reject::Bad(format!("the fmt chunk says {channels} channels at {sample_rate} Hz")));
    }
    let wwise = WWISE_CODECS.contains(&tag) || chunks.akd || r.big_endian;

    // As declared: prefetch media hold only the start of it.
    let data_len = chunks.data_len;
    let samples = match tag {
        // PCM, float, A-law, mu-law: fixed-size frames.
        0x0001 | 0x0003 | 0x0006 | 0x0007 if block_align > 0 => Some(data_len / u64::from(block_align)),
        // Wwise Vorbis keeps the count first in its `vorb` data: its own chunk, or
        // inside a 0x42-byte fmt chunk from 0x18 (Wwise 2012 and later).
        0xFFFF => {
            let vorb = chunks.vorb.filter(|v| v.len() >= 4).map(|v| v.start);
            vorb.or((fmt.len() >= 0x42).then_some(f + 0x18)).map(|p| u64::from(r.u32(p)))
        }
        _ => chunks.fact.filter(|c| c.len() >= 4).map(|c| u64::from(r.u32(c.start))),
    };

    Ok(AudioInfo {
        size: end as u64,
        codec: codec_name(tag, bits, wwise),
        channels,
        sample_rate,
        samples,
        big_endian: r.big_endian,
        wwise,
        tracks: Vec::new(),
        note,
    })
}

/// The chunks that matter, by where their bodies are (the first of each kind).
#[derive(Default)]
struct Chunks {
    fmt: Option<Range<usize>>,
    data: Option<Range<usize>>,
    data_len: u64,
    fact: Option<Range<usize>>,
    vorb: Option<Range<usize>>,
    akd: bool,
}

fn codec_name(tag: u16, bits: u16, wwise: bool) -> String {
    let name = match tag {
        0x0001 => return format!("PCM {bits}-bit"),
        0x0003 => return format!("IEEE float {bits}-bit"),
        0x0002 if wwise => "Wwise IMA ADPCM",
        0x0002 => "MS ADPCM",
        0x0006 => "A-law",
        0x0007 => "mu-law",
        0x0011 => "IMA ADPCM",
        0x0050 => "MPEG",
        0x0055 => "MP3",
        0x0069 => "Xbox ADPCM",
        0x0160 => "WMA v1",
        0x0161 => "WMA",
        0x0162 => "WMA Pro",
        0x0163 => "WMA Lossless",
        0x0165 => "XMA",
        0x0166 => "XMA2",
        0x0270 => "ATRAC3",
        // KSDATAFORMAT_SUBTYPE_ATRAC9 starts 0x47E142D2.
        0x42D2 => "ATRAC9",
        0xFFFF => "Wwise Vorbis",
        0x3039 => "Wwise Opus (NX)",
        0x3040 => "Wwise Opus",
        0x3041 => "Wwise Opus (WEM)",
        0x8311 => "Wwise PTADPCM",
        _ => return format!("codec 0x{tag:04x}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wave(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WAVE".to_vec();
        for (id, c) in chunks {
            body.extend_from_slice(*id);
            body.extend_from_slice(&(c.len() as u32).to_le_bytes());
            body.extend_from_slice(c);
            if c.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    const PCM16_MONO: [u8; 16] = [1, 0, 1, 0, 0x44, 0xAC, 0, 0, 0x88, 0x58, 1, 0, 2, 0, 16, 0];

    #[test]
    fn odd_chunks_are_padded() {
        let file = wave(&[(b"LIST", b"odd"), (b"fmt ", &PCM16_MONO), (b"data", &[0; 10])]);
        let info = Riff.parse(&file).unwrap();
        assert_eq!((info.size, info.samples, info.codec.as_str()), (file.len() as u64, Some(5), "PCM 16-bit"));
        assert!(!info.wwise);
    }

    #[test]
    fn what_is_missing_is_reported() {
        assert_eq!(Riff.parse(&wave(&[(b"data", &[0; 4])])), Err(Reject::Bad("no fmt chunk".into())));
        assert_eq!(Riff.parse(&wave(&[(b"fmt ", &PCM16_MONO)])), Err(Reject::Bad("no data chunk".into())));
        let mut avi = wave(&[(b"avih", &[0; 8])]);
        avi[8..12].copy_from_slice(b"AVI ");
        assert_eq!(Riff.parse(&avi), Err(Reject::NoMatch));
    }

    #[test]
    fn a_data_chunk_past_a_short_riff_size_sets_the_end() {
        let mut file = wave(&[(b"fmt ", &PCM16_MONO), (b"data", &[0; 10])]);
        let true_size = file.len();
        file[4] -= 4;
        file.extend_from_slice(b"next");
        let info = Riff.parse(&file).unwrap();
        assert_eq!((info.size, info.samples), (true_size as u64, Some(5)));
        assert!(info.note.unwrap().contains("the data chunk runs to"));
        // Past the end of the input it's still an error.
        let err = Riff.parse(&file[..true_size - 1]).unwrap_err();
        assert!(matches!(err, Reject::Bad(r) if r.contains("\"data\" chunk runs past")));
    }

    #[test]
    fn prefetch_media_are_read_from_their_header() {
        let file = wave(&[(b"fmt ", &PCM16_MONO), (b"data", &[0; 1000])]);
        assert!(matches!(Riff.parse(&file[..100]), Err(Reject::Bad(_))));
        let info = parse_prefix(&file[..100]).unwrap();
        assert_eq!((info.size, info.samples), (100, Some(500)));
        assert!(info.note.unwrap().starts_with("prefetch: the first 100 of"));
        // A whole file reads the same either way.
        assert_eq!(parse_prefix(&file), Riff.parse(&file));
    }

    #[test]
    fn an_akd_chunk_means_wwise() {
        let file = wave(&[(b"fmt ", &PCM16_MONO), (b"akd ", &[0; 16]), (b"data", &[0; 4])]);
        assert!(Riff.parse(&file).unwrap().wwise);
    }
}
