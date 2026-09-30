//! The audio-format trait: each container format (RIFF/RIFX WAVE, FSB5, Ogg, Wwise
//! SoundBanks and file packages) is one file under `formats/` that recognises its header
//! and works out the whole file's size.

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
    /// Wwise SoundBank (`.bnk`): WEMs in its `DATA` section, listed by `DIDX`.
    Bnk,
    /// Wwise file package (`.pck`, `AKPK`): SoundBanks and streamed WEMs.
    Pck,
}

impl Container {
    pub const ALL: [Container; 5] = [Container::Riff, Container::Fsb5, Container::Ogg, Container::Bnk, Container::Pck];

    pub fn name(self) -> &'static str {
        match self {
            Container::Riff => "riff",
            Container::Fsb5 => "fsb5",
            Container::Ogg => "ogg",
            Container::Bnk => "bnk",
            Container::Pck => "pck",
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
            "bnk" => Ok(Container::Bnk),
            "pck" | "akpk" => Ok(Container::Pck),
            _ => Err(format!("unknown audio format {s:?} (known: riff, fsb5, ogg, bnk, pck)")),
        }
    }
}

/// One sound (or, in a Wwise package, one SoundBank) inside a bank or package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    /// FSB5 names its sounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Wwise identifies files by number instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// A Wwise package's language for it (`sfx` for none), from the package's own map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// When tracks can differ (Wwise); an FSB5 bank has one codec for all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// The type of file it's split out as: a Wwise `wem` or `bnk` is one already; an FSB5
    /// track becomes a `wav` (PCM) or a one-track `fsb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    /// 0 when unknown or not audio (a SoundBank in a package).
    pub channels: u16,
    pub sample_rate: u32,
    /// Per channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u64>,
    /// Where its data starts, from the start of the bank or package.
    pub offset: u64,
    /// For FSB5, its data up to the next track's (padding included); for Wwise, the file.
    pub size: u64,
    /// Like the one on [`AudioInfo`]: a Wwise bank's prefetch media say so here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Track {
    /// Its name, or else its Wwise ID.
    pub fn display_name(&self) -> String {
        match (&self.name, self.id) {
            (Some(name), _) => name.clone(),
            (None, Some(id)) => id.to_string(),
            (None, None) => String::new(),
        }
    }

    /// Where `extract --split` writes it (track `index` of its bank), relative to the bank's
    /// folder: `{id}.{ext}` for Wwise, in a folder named after its language unless that's
    /// `sfx`; `{name}.{ext}` for FSB5, with characters a file name can't hold replaced by
    /// `_`, or `track{index}.{ext}` if it has no name. `None` if it can't be split out.
    /// Two tracks can get the same name; extract tells them apart.
    pub fn split_filename(&self, index: usize) -> Option<String> {
        let ext = self.extension.as_deref()?;
        let stem = match (self.id, self.name.as_deref().map(file_stem)) {
            (Some(id), _) => id.to_string(),
            (None, Some(name)) if !name.is_empty() => name,
            _ => format!("track{index}"),
        };
        let file = format!("{stem}.{ext}");
        match self.language.as_deref() {
            Some(lang) if lang != "sfx" && crate::extract::is_safe_filename(lang) => Some(format!("{lang}/{file}")),
            _ => Some(file),
        }
    }
}

/// A name made safe as a file name on any system: letters, digits, spaces and `_-.()[]`
/// kept, anything else `_`, no leading or trailing dots or spaces (Windows drops those).
fn file_stem(name: &str) -> String {
    let kept: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || " _-.()[]".contains(c) { c } else { '_' })
        .collect();
    kept.trim_matches(['.', ' ']).to_string()
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
    /// A bank's or package's contents (FSB5, BNK, PCK); empty for other formats.
    pub tracks: Vec<Track>,
    /// Something worth knowing that didn't stop it being found, like an Ogg stream with
    /// no end-of-stream page.
    pub note: Option<String>,
}

impl AudioInfo {
    /// In seconds, when the length is known: a bank's is the total of its audio tracks
    /// (those with a sample rate), known only if every one's is.
    pub fn duration(&self) -> Option<f64> {
        let seconds = |samples: Option<u64>, rate: u32| (rate > 0).then_some(samples? as f64 / f64::from(rate));
        if self.tracks.is_empty() {
            return seconds(self.samples, self.sample_rate);
        }
        let audio: Vec<_> = self.tracks.iter().filter(|t| t.sample_rate > 0).collect();
        if audio.is_empty() {
            return None;
        }
        audio.iter().map(|t| seconds(t.samples, t.sample_rate)).sum()
    }
}

/// File extension for extracted audio, without the dot: `wem` for Wwise's.
pub fn extension(container: Container, wwise: bool) -> &'static str {
    match container {
        Container::Riff if wwise => "wem",
        Container::Riff => "wav",
        Container::Fsb5 => "fsb",
        Container::Ogg => "ogg",
        Container::Bnk => "bnk",
        Container::Pck => "pck",
    }
}

/// A short label for listings: `wav`, `wem`, `fsb5`, `ogg`, `bnk` or `pck`, with ` BE` for
/// big-endian files (RIFX, and Wwise banks and packages from big-endian consoles).
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
        Container::Bnk => &crate::formats::bnk::Bnk,
        Container::Pck => &crate::formats::pck::Pck,
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
        assert_eq!("AKPK".parse::<Container>().unwrap(), Container::Pck);
        assert!("mp3".parse::<Container>().is_err());
        assert_eq!(label(Container::Riff, true, true), "wem BE");
        assert_eq!(label(Container::Fsb5, false, false), "fsb5");
        assert_eq!(extension(Container::Fsb5, false), "fsb");
    }

    #[test]
    fn split_filenames() {
        let fsb = |name: Option<&str>| Track { name: name.map(String::from), extension: Some("fsb".into()), ..Track::default() };
        assert_eq!(fsb(Some("music/intro:loop")).split_filename(3).unwrap(), "music_intro_loop.fsb");
        assert_eq!(fsb(Some("..")).split_filename(3).unwrap(), "track3.fsb");
        assert_eq!(fsb(None).split_filename(0).unwrap(), "track0.fsb");
        let wem = Track { id: Some(42), language: Some("french".into()), extension: Some("wem".into()), ..Track::default() };
        assert_eq!(wem.split_filename(9).unwrap(), "french/42.wem");
        assert_eq!(Track::default().split_filename(0), None);
    }
}
