//! Decoding a found file (or a bank's track, from [`crate::track_bytes`]) to 16-bit PCM,
//! for previews and playback: WAV (PCM and float), WEMs that [`convert_wem`] handles (via
//! its Ogg or WAV), and Ogg Vorbis (with `lewton`, MIT/Apache). Opus, FLAC, MS/IMA ADPCM
//! WAVs, FMOD's own codecs and console codecs aren't decoded.

use std::io::Cursor;

use crate::convert::{ConvertError, convert_wem};
use crate::format::{AudioFormat, Container};
use crate::formats::ogg::Ogg;
use crate::formats::riff::Riff;

/// Decoded audio: interleaved 16-bit samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    /// Samples per channel.
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate.max(1))
    }
}

/// Decode a whole file: a RIFF/RIFX WAVE (WAV or WEM) or an Ogg Vorbis stream.
pub fn decode_file(bytes: &[u8]) -> Result<Pcm, ConvertError> {
    match Container::sniff(bytes) {
        Some(Container::Riff) => {
            let converted = convert_wem(bytes)?;
            match converted.extension {
                "ogg" => decode_ogg(&converted.bytes),
                _ => decode_wav(&converted.bytes),
            }
        }
        Some(Container::Ogg) => decode_ogg(bytes),
        Some(c) => Err(ConvertError::Unsupported(format!("playing a whole {} (pick one of its tracks)", c.name()))),
        None => Err(ConvertError::NotWem("not a file audscan can play".into())),
    }
}

/// A PCM or float WAV, as [`convert_wem`] writes them.
fn decode_wav(wav: &[u8]) -> Result<Pcm, ConvertError> {
    let info = Riff.parse(wav).map_err(|_| ConvertError::Invalid("the WAV doesn't read".into()))?;
    let at = wav.windows(4).position(|w| w == b"data").ok_or_else(|| ConvertError::Invalid("no data chunk".into()))? + 8;
    let len = u32::from_le_bytes(wav[at - 4..at].try_into().unwrap()) as usize;
    let data = wav.get(at..at + len).unwrap_or(&wav[at..]);
    let samples = match info.codec.as_str() {
        "PCM 8-bit" => data.iter().map(|&b| (i16::from(b) - 128) << 8).collect(),
        "PCM 16-bit" => data.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect(),
        "PCM 24-bit" => data.as_chunks::<3>().0.iter().map(|&[_, m, h]| i16::from_le_bytes([m, h])).collect(),
        "PCM 32-bit" => data.as_chunks::<4>().0.iter().map(|&[_, _, m, h]| i16::from_le_bytes([m, h])).collect(),
        "IEEE float 32-bit" => {
            data.as_chunks::<4>().0.iter().map(|&b| (f32::from_le_bytes(b).clamp(-1.0, 1.0) * 32767.0) as i16).collect()
        }
        other => return Err(ConvertError::Unsupported(format!("playing {other}"))),
    };
    Ok(Pcm { channels: info.channels, sample_rate: info.sample_rate, samples })
}

/// An Ogg Vorbis stream, decoded with lewton and trimmed to its last granule position.
fn decode_ogg(ogg: &[u8]) -> Result<Pcm, ConvertError> {
    let info = Ogg.parse(ogg).map_err(|_| ConvertError::Invalid("the Ogg stream doesn't read".into()))?;
    if !info.codec.starts_with("Vorbis") {
        return Err(ConvertError::Unsupported(format!("playing Ogg {}", info.codec)));
    }
    let invalid = |e: lewton::VorbisError| ConvertError::Invalid(format!("Vorbis: {e}"));
    let mut reader = lewton::inside_ogg::OggStreamReader::new(Cursor::new(ogg)).map_err(invalid)?;
    let (channels, sample_rate) = (u16::from(reader.ident_hdr.audio_channels), reader.ident_hdr.audio_sample_rate);
    let mut samples = Vec::new();
    while let Some(packet) = reader.read_dec_packet_itl().map_err(invalid)? {
        samples.extend(packet);
    }
    if let Some(frames) = info.samples {
        samples.truncate(frames as usize * usize::from(channels));
    }
    Ok(Pcm { channels, sample_rate, samples })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wav_decodes_to_its_samples() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x04\0\0\0".to_vec();
        wav.extend_from_slice(&[0x34, 0x12, 0xFF, 0xFF]);
        let size = (wav.len() - 8) as u32;
        wav[4..8].copy_from_slice(&size.to_le_bytes());
        let pcm = decode_file(&wav).unwrap();
        assert_eq!((pcm.channels, pcm.sample_rate, pcm.samples), (1, 8000, vec![0x1234, -1]));
    }

    #[test]
    fn what_cant_be_played_says_why() {
        assert!(matches!(decode_file(b"FSB5 a whole bank"), Err(ConvertError::Unsupported(r)) if r.contains("fsb5")));
        assert!(matches!(decode_file(b"nothing audscan knows"), Err(ConvertError::NotWem(_))));
    }
}
