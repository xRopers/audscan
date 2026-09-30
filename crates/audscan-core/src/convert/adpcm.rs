//! PCM and Wwise IMA ADPCM WEMs to WAV.
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
