//! The manifest: what a scan found, and the contract that extract (and later pack) work
//! from.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_err};
use crate::format::{Container, Track, extension, label};
use crate::scan::{FoundAudio, ScanOptions};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub source: SourceInfo,
    pub scan_options: ScanOptions,
    pub audio: Vec<AudioEntry>,
}

/// Identifies the input file, so a manifest isn't applied to the wrong file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub path: String,
    pub size: u64,
    #[serde(with = "hex_u32")]
    pub crc32: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEntry {
    pub id: u32,
    pub offset: u64,
    /// The whole file, header included.
    pub size: u64,
    pub format: Container,
    pub codec: String,
    pub channels: u16,
    pub sample_rate: u32,
    /// Per channel, when the header says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u64>,
    /// RIFX.
    #[serde(default, skip_serializing_if = "is_false")]
    pub big_endian: bool,
    /// Made by Wwise (extracted as `.wem`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub wwise: bool,
    /// A bank's sounds (FSB5).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// CRC-32 of the whole file, header included.
    #[serde(with = "hex_u32")]
    pub crc32: u32,
    /// Extracted file name, relative to the extract directory.
    pub file: String,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl SourceInfo {
    pub fn describe(path: &Path, data: &[u8]) -> Self {
        Self { path: path.display().to_string(), size: data.len() as u64, crc32: crc32fast::hash(data) }
    }

    /// Error if `data` is not the file this manifest was made from.
    pub fn check(&self, data: &[u8]) -> Result<()> {
        if data.len() as u64 != self.size {
            return Err(Error::SourceMismatch(format!("input is {} bytes, manifest expects {}", data.len(), self.size)));
        }
        let actual = crc32fast::hash(data);
        if actual != self.crc32 {
            return Err(Error::SourceMismatch(format!(
                "input CRC-32 is {actual:08x}, manifest expects {:08x}",
                self.crc32
            )));
        }
        Ok(())
    }
}

impl AudioEntry {
    pub fn from_found(id: u32, a: &FoundAudio) -> Self {
        let i = &a.info;
        Self {
            id,
            offset: a.offset,
            size: i.size,
            format: a.container,
            codec: i.codec.clone(),
            channels: i.channels,
            sample_rate: i.sample_rate,
            samples: i.samples,
            big_endian: i.big_endian,
            wwise: i.wwise,
            tracks: i.tracks.clone(),
            note: i.note.clone(),
            crc32: a.crc32,
            file: audio_filename(a.offset, a.extension()),
        }
    }

    pub fn end(&self) -> u64 {
        self.offset + self.size
    }

    /// `wav`, `wem`, `fsb5` or `ogg`, with ` BE` for RIFX.
    pub fn label(&self) -> String {
        label(self.format, self.wwise, self.big_endian)
    }

    pub fn extension(&self) -> &'static str {
        extension(self.format, self.wwise)
    }
}

impl Manifest {
    pub fn new(source: SourceInfo, scan_options: ScanOptions, found: &[FoundAudio]) -> Self {
        let audio = found.iter().enumerate().map(|(i, a)| AudioEntry::from_found(i as u32, a)).collect();
        Self { version: MANIFEST_VERSION, source, scan_options, audio }
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        match value["version"].as_u64() {
            Some(v) if v == u64::from(MANIFEST_VERSION) => Ok(serde_json::from_value(value)?),
            other => {
                let found = other.and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
                Err(Error::ManifestVersion { found, expected: MANIFEST_VERSION })
            }
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("manifest serialization cannot fail")
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::from_json(&fs::read_to_string(path).map_err(io_err(path))?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(path, self.to_json() + "\n").map_err(io_err(path))
    }
}

/// `0001f400.wem`: the offset in hex, so files sort in file order.
pub fn audio_filename(offset: u64, extension: &str) -> String {
    format!("{offset:08x}.{extension}")
}

pub(crate) mod hex_u32 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(value: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{value:08x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        let text = String::deserialize(d)?;
        let digits = text.strip_prefix("0x").unwrap_or(&text);
        u32::from_str_radix(digits, 16).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames() {
        assert_eq!(audio_filename(0x1234, "wem"), "00001234.wem");
        assert_eq!(audio_filename(0x1_0000_0000, "ogg"), "100000000.ogg");
    }

    #[test]
    fn rejects_other_versions() {
        let err = Manifest::from_json(r#"{"version": 7}"#).unwrap_err();
        assert!(matches!(err, Error::ManifestVersion { found: 7, expected: 1 }));
    }
}
