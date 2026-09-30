//! PCM, Wwise IMA ADPCM and PTADPCM WEMs to WAV.
//!
//! PCM is copied into a plain WAV (big-endian RIFX samples made little-endian). Wwise IMA
//! ADPCM is decoded to 16-bit PCM: blocks of `block_align` bytes, each channel's part a
//! 4-byte header (a 16-bit starting sample and a step index) followed by its nibbles, low
//! nibble first. A block gives 64 samples per channel for 36 bytes: the header's sample,
//! then 63 nibbles (the last is unused), each step `(2n + 1) * step / 8` as in Microsoft's
//! IMA ADPCM. Checked sample for sample against vgmstream's decoding of real files.

use super::wem::Wem;
use super::ConvertError;
use crate::formats::riff::{Pcm, pcm_wav};

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282,
    1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX_STEP: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// One IMA ADPCM channel's decoder state.
struct Ima {
    predictor: i32,
    index: i32,
}

impl Ima {
    fn next(&mut self, nibble: u8) -> i16 {
        let step = STEPS[self.index as usize];
        let diff = ((i32::from(nibble & 7) * 2 + 1) * step) >> 3;
        self.predictor = if nibble & 8 != 0 { self.predictor - diff } else { self.predictor + diff }.clamp(-32768, 32767);
        self.index = (self.index + INDEX_STEP[usize::from(nibble & 7)]).clamp(0, 88);
        self.predictor as i16
    }
}

/// Decode Wwise IMA ADPCM to interleaved 16-bit samples, `samples` per channel at most.
fn decode_ima(data: &[u8], channels: usize, block_align: usize, samples: usize) -> Result<Vec<i16>, ConvertError> {
    let per_channel = block_align / channels;
    if channels == 0 || per_channel <= 4 || !block_align.is_multiple_of(channels) {
        return Err(ConvertError::Invalid("an IMA ADPCM block layout that doesn't divide by channel".into()));
    }
    let mut out = Vec::with_capacity(samples * channels);
    let mut produced = 0;
    for block in data.chunks(block_align) {
        if block.len() < channels * (4 + 1) || produced >= samples {
            break;
        }
        let part = block.len() / channels;
        let frames = ((part - 4) * 2).min(samples - produced);
        let start = out.len();
        out.resize(start + frames * channels, 0);
        for ch in 0..channels {
            let p = &block[ch * part..(ch + 1) * part];
            let mut state = Ima { predictor: i16::from_le_bytes([p[0], p[1]]).into(), index: i32::from(p[2]).min(88) };
            for i in 0..frames {
                out[start + i * channels + ch] = if i == 0 {
                    state.predictor as i16
                } else {
                    let byte = p[4 + (i - 1) / 2];
                    state.next(if (i - 1) % 2 == 0 { byte & 0x0F } else { byte >> 4 })
                };
            }
        }
        produced += frames;
    }
    Ok(out)
}

/// PTADPCM steps and next indexes, by index (0–11; 12 and up give 0) and nibble. From
/// vgmstream's `ptadpcm_decoder.c` (reverse engineered from Platinum Games' executables),
/// ISC license: `data/COPYING-vgmstream`.
#[rustfmt::skip]
const PTADPCM: [[(i32, u8); 16]; 12] = [
    [(-14, 2), (-10, 2), (-7, 1), (-5, 1), (-3, 0), (-2, 0), (-1, 0), (0, 0), (0, 0), (1, 0), (2, 0), (3, 0), (5, 1), (7, 1), (10, 2), (14, 2)],
    [(-28, 3), (-20, 3), (-14, 2), (-10, 2), (-7, 1), (-5, 1), (-3, 1), (-1, 0), (1, 0), (3, 1), (5, 1), (7, 1), (10, 2), (14, 2), (20, 3), (28, 3)],
    [(-56, 4), (-40, 4), (-28, 3), (-20, 3), (-14, 2), (-10, 2), (-6, 2), (-2, 1), (2, 1), (6, 2), (10, 2), (14, 2), (20, 3), (28, 3), (40, 4), (56, 4)],
    [(-112, 5), (-80, 5), (-56, 4), (-40, 4), (-28, 3), (-20, 3), (-12, 3), (-4, 2), (4, 2), (12, 3), (20, 3), (28, 3), (40, 4), (56, 4), (80, 5), (112, 5)],
    [(-224, 6), (-160, 6), (-112, 5), (-80, 5), (-56, 4), (-40, 4), (-24, 4), (-8, 3), (8, 3), (24, 4), (40, 4), (56, 4), (80, 5), (112, 5), (160, 6), (224, 6)],
    [(-448, 7), (-320, 7), (-224, 6), (-160, 6), (-112, 5), (-80, 5), (-48, 5), (-16, 4), (16, 4), (48, 5), (80, 5), (112, 5), (160, 6), (224, 6), (320, 7), (448, 7)],
    [(-896, 8), (-640, 8), (-448, 7), (-320, 7), (-224, 6), (-160, 6), (-96, 6), (-32, 5), (32, 5), (96, 6), (160, 6), (224, 6), (320, 7), (448, 7), (640, 8), (896, 8)],
    [(-1792, 9), (-1280, 9), (-896, 8), (-640, 8), (-448, 7), (-320, 7), (-192, 7), (-64, 6), (64, 6), (192, 7), (320, 7), (448, 7), (640, 8), (896, 8), (1280, 9), (1792, 9)],
    [(-3584, 10), (-2560, 10), (-1792, 9), (-1280, 9), (-896, 8), (-640, 8), (-384, 8), (-128, 7), (128, 7), (384, 8), (640, 8), (896, 8), (1280, 9), (1792, 9), (2560, 10), (3584, 10)],
    [(-7168, 11), (-5120, 11), (-3584, 10), (-2560, 10), (-1792, 9), (-1280, 9), (-768, 9), (-256, 8), (256, 8), (768, 9), (1280, 9), (1792, 9), (2560, 10), (3584, 10), (5120, 11), (7168, 11)],
    [(-14336, 11), (-10240, 11), (-7168, 11), (-5120, 11), (-3584, 10), (-2560, 10), (-1536, 10), (-512, 9), (512, 9), (1536, 10), (2560, 10), (3584, 10), (5120, 11), (7168, 11), (10240, 11), (14336, 11)],
    [(-28672, 11), (-20480, 11), (-14336, 11), (-10240, 11), (-7168, 11), (-5120, 11), (-3072, 11), (-1024, 10), (1024, 10), (3072, 11), (5120, 11), (7168, 11), (10240, 11), (14336, 11), (20480, 11), (28672, 11)],
];

/// Decode Platinum's PTADPCM, as Wwise uses it: frames of `frame` bytes per channel,
/// channels taking turns frame by frame. A frame is two 16-bit samples (output first), a
/// table index, then nibbles, low first: each looks up a step and the next index, and the
/// sample is `step + 2 * previous - the one before`. `samples` per channel at most.
fn decode_ptadpcm(data: &[u8], channels: usize, frame: usize, samples: usize) -> Result<Vec<i16>, ConvertError> {
    if channels == 0 || frame < 6 {
        return Err(ConvertError::Invalid("a PTADPCM frame size that doesn't make sense".into()));
    }
    let per_frame = 2 + (frame - 5) * 2;
    let mut out = Vec::with_capacity(samples * channels);
    let mut produced = 0;
    for block in data.chunks_exact(frame * channels) {
        if produced >= samples {
            break;
        }
        let frames = per_frame.min(samples - produced);
        let start = out.len();
        out.resize(start + frames * channels, 0);
        for (ch, f) in block.chunks_exact(frame).enumerate() {
            let (mut older, mut last) = (i32::from(i16::from_le_bytes([f[0], f[1]])), i32::from(i16::from_le_bytes([f[2], f[3]])));
            let mut index = usize::from(f[4]);
            for i in 0..frames {
                let sample = match i {
                    0 => older,
                    1 => last,
                    _ => {
                        let byte = f[5 + (i - 2) / 2];
                        let nibble = usize::from(if i % 2 == 0 { byte & 0x0F } else { byte >> 4 });
                        let (step, next) = PTADPCM.get(index).map_or((0, 0), |row| row[nibble]);
                        index = usize::from(next);
                        let sample = (step + 2 * last - older).clamp(-32768, 32767);
                        (older, last) = (last, sample);
                        sample
                    }
                };
                out[start + i * channels + ch] = sample as i16;
            }
        }
        produced += frames;
    }
    Ok(out)
}

pub(crate) fn ptadpcm_to_wav(w: &Wem, samples: Option<u64>) -> Result<Vec<u8>, ConvertError> {
    let channels = usize::from(w.channels);
    let block = usize::from(w.block_align);
    if channels == 0 || block == 0 || !block.is_multiple_of(channels) {
        return Err(ConvertError::Invalid("a PTADPCM block that doesn't divide by channel".into()));
    }
    let frame = block / channels;
    let data = &w.data[w.body.clone()];
    let whole = data.len() / block * (2 + frame.saturating_sub(5) * 2);
    // Wwise stores the exact length; the last frame is padding past it.
    let samples = samples.map_or(whole, |s| (s as usize).min(whole));
    let decoded = decode_ptadpcm(data, channels, frame, samples)?;
    Ok(wav16(w.channels, w.sample_rate, &decoded))
}

/// A WAV of 16-bit samples.
fn wav16(channels: u16, sample_rate: u32, samples: &[i16]) -> Vec<u8> {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let pcm = Pcm { bytes: 2, float: false, signed_8bit: false, big_endian: false };
    pcm_wav(pcm, channels, sample_rate, (samples.len() / usize::from(channels.max(1))) as u64, &bytes).0
}

pub(crate) fn ima_to_wav(w: &Wem, samples: Option<u64>) -> Result<Vec<u8>, ConvertError> {
    let channels = usize::from(w.channels);
    let block = usize::from(w.block_align);
    let data = &w.data[w.body.clone()];
    let whole = block.checked_div(channels).map_or(0, |part| data.len() / block * (part.saturating_sub(4) * 2));
    let samples = samples.map_or(whole, |s| s as usize);
    let decoded = decode_ima(data, channels, block, samples)?;
    Ok(wav16(w.channels, w.sample_rate, &decoded))
}

pub(crate) fn pcm_to_wav(w: &Wem, float: bool) -> Result<Vec<u8>, ConvertError> {
    let bytes = w.bits / 8;
    if !(1..=4).contains(&bytes) || !w.bits.is_multiple_of(8) {
        return Err(ConvertError::Unsupported(format!("{}-bit PCM", w.bits)));
    }
    let data = &w.data[w.body.clone()];
    let frame = usize::from(bytes) * usize::from(w.channels.max(1));
    let pcm = Pcm { bytes, float, signed_8bit: false, big_endian: w.big_endian };
    Ok(pcm_wav(pcm, w.channels, w.sample_rate, (data.len() / frame) as u64, data).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptadpcm_frames() {
        // Index 0, nibble 0xF: +14 over the linear prediction 2 * last - older.
        let mut frame = vec![0u8; 36];
        frame[..4].copy_from_slice(&[10, 0, 20, 0]); // older 10, last 20
        frame[5] = 0x0F; // first nibble 15, then 0
        let out = decode_ptadpcm(&frame, 1, 36, 64).unwrap();
        // 14 + 2 * 20 - 10 = 44 (index becomes 2); then nibble 0 at index 2: -56 + 2 * 44 - 20.
        assert_eq!(out[..4], [10, 20, 44, 12]);
        assert_eq!(out.len(), 64);
    }

    #[test]
    fn ima_decodes_64_samples_per_36_byte_block() {
        // The first bytes of a real Wwise IMA WEM, and vgmstream's decoding of them: the
        // header's sample first, then one sample per nibble, low nibble first.
        let mut block = vec![0u8; 36];
        block[..8].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00, 0x01, 0x10, 0x10, 0x90]);
        let out = decode_ima(&block, 1, 36, 64).unwrap();
        assert_eq!(out.len(), 64);
        assert_eq!(out[..7], [-1, 1, 1, 1, 3, 3, 5]);
        // Stereo: each channel's half of the block decodes on its own.
        let mut block = vec![0u8; 72];
        block[..2].copy_from_slice(&100i16.to_le_bytes());
        block[36..38].copy_from_slice(&(-100i16).to_le_bytes());
        block[4] = 0x77; // channel 0's first two nibbles: +7 each
        let out = decode_ima(&block, 2, 72, 64).unwrap();
        assert_eq!(out.len(), 128);
        assert!(out[0] == 100 && out[2] > 100, "{:?}", &out[..4]);
        assert_eq!(out[1], -100);
    }
}
