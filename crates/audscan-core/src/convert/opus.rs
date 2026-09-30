//! Wwise Opus (tag 0x3041) to Ogg Opus, without re-encoding.
//!
//! The data chunk holds raw Opus packets back to back; the `seek` chunk lists each one's
//! size (a u16 each, as many as fmt+0x1C says), and fmt+0x18 and +0x20 give the sample
//! count and the pre-skip. Checked on real files: the sizes add up to the data exactly and
//! the packets are standard 20 ms CELT frames. The packets go into an Ogg stream behind an
//! `OpusHead` and `OpusTags`, each page's granule position counting 48 kHz samples, the
//! last one set to pre-skip + sample count so a decoder trims the end.
//!
//! Only 1 and 2 channels: more need Wwise's channel layout turned into an Opus mapping,
//! which isn't done yet.

use super::ogg_out::OggWriter;
use super::wem::Wem;
use super::ConvertError;

/// Samples (at 48 kHz) in one Opus packet, from its TOC byte (RFC 6716, 3.1).
fn packet_samples(packet: &[u8]) -> Result<u64, ConvertError> {
    let toc = *packet.first().ok_or_else(|| ConvertError::Invalid("an empty Opus packet".into()))?;
    let config = usize::from(toc >> 3);
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][config % 4],
        12..=15 => [480, 960][config % 2],
        _ => [120, 240, 480, 960][config % 4],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => u64::from(packet.get(1).ok_or_else(|| ConvertError::Invalid("an Opus packet is cut off".into()))? & 0x3F),
    };
    Ok(frame * frames)
}

pub(crate) fn to_ogg(w: &Wem) -> Result<Vec<u8>, ConvertError> {
    if !(1..=2).contains(&w.channels) {
        return Err(ConvertError::Unsupported(format!("Wwise Opus with {} channels", w.channels)));
    }
    if w.fmt_len() < 0x22 {
        return Err(ConvertError::Invalid("the Opus fmt chunk is too short".into()));
    }
    let f = w.fmt.start;
    let (samples, count, preskip) = (w.u32(f + 0x18)?, w.u32(f + 0x1C)? as usize, w.u16(f + 0x20)?);
    let seek = w.seek.clone().ok_or_else(|| ConvertError::Invalid("no seek chunk listing the Opus packets".into()))?;
    if seek.len() < count * 2 {
        return Err(ConvertError::Invalid("the seek chunk lists fewer packets than the fmt chunk says".into()));
    }

    let mut ogg = OggWriter::new(crc32fast::hash(w.data));
    let mut head = b"OpusHead\x01".to_vec();
    head.push(w.channels as u8);
    head.extend_from_slice(&preskip.to_le_bytes());
    head.extend_from_slice(&w.sample_rate.to_le_bytes());
    head.extend_from_slice(&[0, 0, 0]); // output gain, channel mapping family 0
    ogg.packet(&head, 0);
    ogg.flush();
    let vendor = b"audscan: converted from Audiokinetic Wwise";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    ogg.packet(&tags, 0);
    ogg.flush();

    let end = u64::from(preskip) + u64::from(samples);
    let (mut pos, mut granule) = (w.body.start, 0u64);
    for i in 0..count {
        let size = w.u16(seek.start + 2 * i)? as usize;
        let packet = w.data.get(pos..pos + size).filter(|_| pos + size <= w.body.end);
        let packet = packet.ok_or_else(|| ConvertError::Invalid("an Opus packet runs past the end of the data".into()))?;
        granule += packet_samples(packet)?;
        let last = i + 1 == count;
        ogg.packet(packet, if last { granule.min(end) } else { granule } as i64);
        pos += size;
    }
    if pos != w.body.end {
        return Err(ConvertError::Invalid("the Opus packets don't fill the data exactly".into()));
    }
    Ok(ogg.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes_from_the_toc() {
        assert_eq!(packet_samples(&[0xF8]).unwrap(), 960); // CELT FB 20 ms, one frame
        assert_eq!(packet_samples(&[0xF9]).unwrap(), 1920); // two frames
        assert_eq!(packet_samples(&[0x1B, 0x03]).unwrap(), 2880 * 3); // SILK 60 ms, three
        assert_eq!(packet_samples(&[0x60]).unwrap(), 480); // hybrid 10 ms
        assert!(packet_samples(&[]).is_err());
    }
}
