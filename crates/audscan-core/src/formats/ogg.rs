//! Ogg: a run of pages, each `OggS`, a 27-byte header, a segment table and a body,
//! checked by a CRC. A logical stream starts with a beginning-of-stream (BOS) page and
//! ends with an end-of-stream (EOS) page; several can be multiplexed, their BOS pages
//! coming first. A file is found from its first BOS page and runs until every stream
//! begun there has ended, so a chained file (one stream after another) is found as one
//! entry per link. The codec comes from each stream's first packet: Vorbis, Opus, FLAC
//! or Speex (Theora video and Skeleton streams are named too).

use crate::format::{AudioFormat, AudioInfo, Container, Reject, u16_le, u32_le};

pub struct Ogg;

const HEADER: usize = 27;
const CONTINUED: u8 = 0x01;
const BOS: u8 = 0x02;
const EOS: u8 = 0x04;

struct Page<'a> {
    flags: u8,
    /// -1 when no packet ends on this page.
    granule: i64,
    serial: u32,
    len: usize,
    body: &'a [u8],
}

enum PageError {
    NotAPage,
    Truncated,
    BadCrc,
}

fn page_at(data: &[u8]) -> Result<Page<'_>, PageError> {
    if data.len() < HEADER || &data[..4] != b"OggS" || data[4] != 0 {
        return Err(PageError::NotAPage);
    }
    let table_end = HEADER + data[26] as usize;
    let table = data.get(HEADER..table_end).ok_or(PageError::Truncated)?;
    let len = table_end + table.iter().map(|&s| s as usize).sum::<usize>();
    let page = data.get(..len).ok_or(PageError::Truncated)?;
    if crc(page) != u32_le(page, 22) {
        return Err(PageError::BadCrc);
    }
    Ok(Page {
        flags: page[5],
        granule: i64::from_le_bytes(page[6..14].try_into().unwrap()),
        serial: u32_le(page, 14),
        len,
        body: &page[table_end..],
    })
}

/// Ogg's CRC-32: polynomial 0x04c11db7, not reflected, starting from 0, computed with the
/// CRC field as zeros.
fn crc(page: &[u8]) -> u32 {
    page.iter().enumerate().fold(0u32, |crc, (i, &b)| {
        let b = if (22..26).contains(&i) { 0 } else { b };
        (crc << 8) ^ CRC_TABLE[usize::from((crc >> 24) as u8 ^ b)]
    })
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04c1_1db7 } else { r << 1 };
            bit += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
};

/// What a stream's first packet says.
#[derive(Debug, Clone, PartialEq)]
struct Head {
    codec: &'static str,
    audio: bool,
    channels: u16,
    sample_rate: u32,
    /// Samples to drop from the start (Opus pre-skip), taken off the granule position.
    preskip: u64,
}

impl Head {
    fn other(codec: &'static str) -> Head {
        Head { codec, audio: false, channels: 0, sample_rate: 0, preskip: 0 }
    }

    fn audio(codec: &'static str, channels: u16, sample_rate: u32) -> Head {
        Head { codec, audio: true, channels, sample_rate, preskip: 0 }
    }
}

fn identify(packet: &[u8]) -> Head {
    let be24 = |p: usize| u32::from(packet[p]) << 16 | u32::from(packet[p + 1]) << 8 | u32::from(packet[p + 2]);
    if packet.starts_with(b"\x01vorbis") && packet.len() >= 30 {
        Head::audio("Vorbis", packet[11].into(), u32_le(packet, 12))
    } else if packet.starts_with(b"OpusHead") && packet.len() >= 19 {
        // Opus always decodes at 48 kHz; the header's rate is only the input's.
        Head { preskip: u16_le(packet, 10).into(), ..Head::audio("Opus", packet[9].into(), 48000) }
    } else if packet.starts_with(b"\x7fFLAC") && packet.len() >= 17 + 18 {
        // After the mapping header, "fLaC", a metadata block header and STREAMINFO, whose
        // bytes 10-12 hold a 20-bit sample rate and 3 bits of channels - 1.
        let rate = be24(27) >> 4;
        Head::audio("FLAC", u16::from(packet[29] >> 1 & 7) + 1, rate)
    } else if packet.starts_with(b"Speex   ") && packet.len() >= 52 {
        Head::audio("Speex", u32_le(packet, 48) as u16, u32_le(packet, 36))
    } else if packet.starts_with(b"\x80theora") {
        Head::other("Theora")
    } else if packet.starts_with(b"fishead\0") {
        Head::other("Skeleton")
    } else {
        Head::other("unknown")
    }
}

struct Stream {
    serial: u32,
    head: Head,
    granule: Option<i64>,
    ended: bool,
}

impl AudioFormat for Ogg {
    fn container(&self) -> Container {
        Container::Ogg
    }

    fn magics(&self) -> &'static [&'static [u8]] {
        &[b"OggS"]
    }

    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject> {
        // Only a stream's first page starts a file; other pages are skipped silently.
        if data.len() < HEADER || data[4] != 0 || data[5] & (BOS | CONTINUED) != BOS {
            return Err(Reject::NoMatch);
        }
        let mut page = page_at(data).map_err(|e| match e {
            PageError::NotAPage => Reject::NoMatch,
            PageError::Truncated => Reject::Bad("the first page is cut off by the end of the file".into()),
            PageError::BadCrc => Reject::Bad("the first page's CRC doesn't match".into()),
        })?;

        let mut streams: Vec<Stream> = Vec::new();
        let mut starting = true;
        let mut pos = 0;
        loop {
            if page.flags & BOS != 0 {
                // BOS pages after the first data page, or a serial seen before, start the
                // next link of a chain.
                if !starting || streams.iter().any(|s| s.serial == page.serial) {
                    break;
                }
                streams.push(Stream { serial: page.serial, head: identify(page.body), granule: None, ended: false });
            } else {
                starting = false;
            }
            let Some(stream) = streams.iter_mut().find(|s| s.serial == page.serial) else { break };
            if page.granule != -1 {
                stream.granule = Some(page.granule);
            }
            stream.ended |= page.flags & EOS != 0;
            pos += page.len;
            if streams.iter().all(|s| s.ended) {
                break;
            }
            match page_at(&data[pos..]) {
                Ok(next) => page = next,
                Err(_) => break,
            }
        }

        let note = (!streams.iter().all(|s| s.ended))
            .then(|| format!("no end-of-stream page; the pages stop after {pos} bytes"));
        let main = streams.iter().find(|s| s.head.audio).unwrap_or(&streams[0]);
        let samples = main.granule.map(|g| (g.max(0) as u64).saturating_sub(main.head.preskip));
        let codec = streams.iter().map(|s| s.head.codec).collect::<Vec<_>>().join(" + ");
        Ok(AudioInfo {
            size: pos as u64,
            codec,
            channels: main.head.channels,
            sample_rate: main.head.sample_rate,
            samples: samples.filter(|_| main.head.audio),
            big_endian: false,
            wwise: false,
            tracks: Vec::new(),
            note,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_a_known_value() {
        // CRC-32/CKSUM (check value 0x765e7680) without its final inversion.
        let crc_of = |bytes: &[u8]| bytes.iter().fold(0u32, |c, &b| (c << 8) ^ CRC_TABLE[usize::from((c >> 24) as u8 ^ b)]);
        assert_eq!(crc_of(b"123456789"), 0x89a1_897f);
    }

    #[test]
    fn identifies_first_packets() {
        let mut vorbis = b"\x01vorbis\0\0\0\0\x02".to_vec();
        vorbis.extend_from_slice(&44100u32.to_le_bytes());
        vorbis.resize(30, 0);
        assert_eq!(identify(&vorbis), Head::audio("Vorbis", 2, 44100));
        let mut opus = b"OpusHead\x01\x01".to_vec();
        opus.extend_from_slice(&312u16.to_le_bytes());
        opus.resize(19, 0);
        assert_eq!(identify(&opus).preskip, 312);
        assert_eq!(identify(b"\x01vorbis").codec, "unknown");
    }
}
