//! The audio-format trait: each container format (RIFF/RIFX WAVE, FSB5, Ogg) is one file
//! under `formats/` that recognises its header and works out the whole stream's size.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// An audio container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    /// RIFF or RIFX WAVE: `.wav` (PCM, ADPCM, XMA, xWMA...) and Wwise `.wem`.
    Riff,
    /// FMOD sound bank, version 5: `.fsb`, also inside FMOD Studio `.bank` files.
    Fsb5,
    /// Ogg: Vorbis, Opus, FLAC or Speex.
    Ogg,
}

impl Container {
    pub const ALL: [Container; 3] = [Container::Riff, Container::Fsb5, Container::Ogg];

    pub fn name(self) -> &'static str {
        match self {
            Container::Riff => "riff",
            Container::Fsb5 => "fsb5",
            Container::Ogg => "ogg",
        }
    }

    /// The container a file starts with, by its magic bytes.
    pub fn sniff(data: &[u8]) -> Option<Container> {
        Container::ALL.into_iter().find(|&c| format_for(c).magics().iter().any(|m| data.starts_with(m)))
    }
}

impl fmt::Display for Container {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

impl FromStr for Container {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "riff" | "rifx" | "wav" | "wem" => Ok(Container::Riff),
            "fsb5" | "fsb" | "fmod" => Ok(Container::Fsb5),
            "ogg" => Ok(Container::Ogg),
            _ => Err(format!("unknown audio format {s:?} (known: riff, fsb5, ogg)")),
        }
    }
}

/// One sound in a bank (FSB5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub channels: u16,
    pub sample_rate: u32,
    /// Per channel.
    pub samples: u64,
    /// Where its data starts, from the start of the bank.
    pub offset: u64,
    /// Its data, up to the next track's (padding included).
    pub size: u64,
}

/// What a header says about the audio, and so how big it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInfo {
    /// The whole file, header included.
    pub size: u64,
    /// Like `PCM 16-bit`, `Wwise Vorbis`, `Opus`. Several Ogg streams multiplexed together
    /// are joined with ` + `.
    pub codec: String,
    /// Of the first track for a bank, of the first audio stream for a multiplexed Ogg.
    pub channels: u16,
    pub sample_rate: u32,
    /// Per channel, when the header says. `None` for a bank of several tracks: see `tracks`.
    pub samples: Option<u64>,
    /// RIFX: a big-endian RIFF.
    pub big_endian: bool,
    /// Made by Audiokinetic Wwise, so extracted as `.wem`.
    pub wwise: bool,
    /// A bank's sounds (FSB5); empty for other formats.
    pub tracks: Vec<Track>,
    /// Something worth knowing that didn't stop it being found, like an Ogg stream with
    /// no end-of-stream page.
    pub note: Option<String>,
}

impl AudioInfo {
    /// In seconds, when the length is known: a bank's is the total of its tracks.
    pub fn duration(&self) -> Option<f64> {
        let seconds = |samples: u64, rate: u32| (rate > 0).then(|| samples as f64 / f64::from(rate));
        if self.tracks.is_empty() {
            seconds(self.samples?, self.sample_rate)
        } else {
            self.tracks.iter().map(|t| seconds(t.samples, t.sample_rate)).sum()
        }
    }
}

/// File extension for extracted audio, without the dot: `wem` for Wwise's.
pub fn extension(container: Container, wwise: bool) -> &'static str {
    match container {
        Container::Riff if wwise => "wem",
        Container::Riff => "wav",
        Container::Fsb5 => "fsb",
        Container::Ogg => "ogg",
    }
}

/// A short label for listings: `wav`, `wem`, `fsb5` or `ogg`, with ` BE` for RIFX.
pub fn label(container: Container, wwise: bool, big_endian: bool) -> String {
    let name = match container {
        Container::Fsb5 => "fsb5",
        c => extension(c, wwise),
    };
    if big_endian { format!("{name} BE") } else { name.to_string() }
}

/// Why [`AudioFormat::parse`] didn't accept a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// Not this format at all, such as the magic bytes inside some text. Not reported.
    NoMatch,
    /// The header is clearly this format but can't be used (unknown codec, truncated
    /// data...). Reported, since the user may want to know.
    Bad(String),
}

pub trait AudioFormat: Sync {
    fn container(&self) -> Container;

    /// Byte strings a file in this format can start with.
    fn magics(&self) -> &'static [&'static [u8]];

    /// Read the header of the file at `data[0]`; `data` runs to the end of the input.
    /// Accepts it only if the whole file fits.
    fn parse(&self, data: &[u8]) -> Result<AudioInfo, Reject>;
}

pub fn format_for(container: Container) -> &'static dyn AudioFormat {
    match container {
        Container::Riff => &crate::formats::riff::Riff,
        Container::Fsb5 => &crate::formats::fsb5::Fsb5,
        Container::Ogg => &crate::formats::ogg::Ogg,
    }
}

/// Little-endian u64 at `pos`; the caller has checked the length.
pub(crate) fn u64_le(data: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap())
}

/// Little-endian u32 at `pos`; the caller has checked the length.
pub(crate) fn u32_le(data: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap())
}

/// Little-endian u16 at `pos`; the caller has checked the length.
pub(crate) fn u16_le(data: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases() {
        for (text, c) in [("WEM", Container::Riff), ("rifx", Container::Riff), ("fmod", Container::Fsb5), ("ogg", Container::Ogg)] {
            assert_eq!(text.parse::<Container>().unwrap(), c);
        }
        assert!("mp3".parse::<Container>().is_err());
        assert_eq!(label(Container::Riff, true, true), "wem BE");
        assert_eq!(label(Container::Fsb5, false, false), "fsb5");
        assert_eq!(extension(Container::Fsb5, false), "fsb");
    }
}
