//! Converting WEMs to files any player opens: Wwise Vorbis and Opus to Ogg (the packets
//! rewrapped, not re-encoded), PCM, IMA ADPCM and PTADPCM to WAV. The older Opus variants
//! aren't converted yet.

mod adpcm;
mod bits;
pub(crate) mod ogg_out;
mod opus;
mod vorbis;
mod wem;

use crate::formats::riff::parse_prefix;
use wem::Wem;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    #[error("not a WEM: {0}")]
    NotWem(String),
    #[error("only {have} of its {declared} bytes are here (prefetch media from a bank, or a cut-off file)")]
    Partial { have: usize, declared: usize },
    #[error("converting {0} isn't supported yet")]
    Unsupported(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

/// A converted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converted {
    /// `ogg` or `wav`.
    pub extension: &'static str,
    pub bytes: Vec<u8>,
    /// The WEM's codec, as the scan names it.
    pub codec: String,
    /// Something to know about the result, such as a channel order a player may get wrong.
    pub note: Option<String>,
}

/// Convert one WEM (the whole file) to Ogg or WAV.
pub fn convert_wem(data: &[u8]) -> Result<Converted, ConvertError> {
    let w = Wem::parse(data)?;
    let info = parse_prefix(data).map_err(|_| ConvertError::NotWem("its header doesn't read".into()))?;
    let short_fmt = w.fmt_len() == 0x18;
    // WAVE_FORMAT_EXTENSIBLE's subformat, when the fmt chunk has one.
    let subformat = if w.tag == 0xFFFE && w.fmt_len() >= 40 { Some(w.u16(w.fmt.start + 24)?) } else { None };
    let mut note = None;
    let (extension, bytes) = match (w.tag, subformat) {
        (0xFFFF, _) => {
            if w.channels > 2 {
                // Channel order is part of how each packet is coded, so it can't be changed
                // without re-encoding (ww2ogg has the same limit).
                note = Some(format!(
                    "{} channels in Wwise's order (as in WAV: L R C LFE ...); players expect Vorbis's (L C R ...), so some channels will play from the wrong speakers",
                    w.channels
                ));
            }
            ("ogg", vorbis::to_ogg(&w)?)
        }
        (0x3041, _) => ("ogg", opus::to_ogg(&w)?),
        (0x0001, _) | (0xFFFE, None | Some(1)) => ("wav", adpcm::pcm_to_wav(&w, false)?),
        (0x0003, _) | (0xFFFE, Some(3)) => ("wav", adpcm::pcm_to_wav(&w, true)?),
        (0x0002, _) if short_fmt => ("wav", adpcm::ima_to_wav(&w, info.samples)?),
        (0x8311, _) => ("wav", adpcm::ptadpcm_to_wav(&w, info.samples)?),
        _ => return Err(ConvertError::Unsupported(info.codec)),
    };
    Ok(Converted { extension, bytes, codec: info.codec, note })
}
