//! Finding audio: look for each format's magic bytes, then let the format check the header
//! and work out the size. A file that is found is skipped past, so magic bytes inside its
//! data (an Ogg stream stored inside a WAV, say) are ignored.

use memchr::memmem;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::format::{AudioInfo, Container, Reject, extension, format_for, label};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Formats to look for.
    pub formats: Vec<Container>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self { formats: Container::ALL.to_vec() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundAudio {
    pub offset: u64,
    pub container: Container,
    pub info: AudioInfo,
    /// CRC-32 of the whole file, header included.
    pub crc32: u32,
}

impl FoundAudio {
    pub fn end(&self) -> u64 {
        self.offset + self.info.size
    }

    /// `wav`, `wem`, `fsb5`, `ogg`, `bnk` or `pck`, with ` BE` for big-endian files.
    pub fn label(&self) -> String {
        label(self.container, self.info.wwise, self.info.big_endian)
    }

    /// Extension for the extracted file, without the dot.
    pub fn extension(&self) -> &'static str {
        extension(self.container, self.info.wwise)
    }
}

/// A header that is clearly audio but couldn't be used, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rejected {
    pub offset: u64,
    pub container: Container,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// In file order, never overlapping.
    pub audio: Vec<FoundAudio>,
    /// In file order. Candidates inside found audio aren't listed.
    pub rejected: Vec<Rejected>,
}

pub fn scan(data: &[u8], opts: &ScanOptions) -> ScanReport {
    let mut candidates: Vec<(usize, Container)> = opts
        .formats
        .iter()
        .flat_map(|&c| format_for(c).magics().iter().flat_map(move |m| memmem::find_iter(data, m).map(move |pos| (pos, c))))
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    let parsed: Vec<_> =
        candidates.into_par_iter().map(|(pos, c)| (pos, c, format_for(c).parse(&data[pos..]))).collect();

    let mut report = ScanReport::default();
    let mut next = 0;
    for (pos, container, result) in parsed {
        if pos < next {
            continue;
        }
        let offset = pos as u64;
        match result {
            Ok(info) => {
                next = pos + info.size as usize;
                report.audio.push(FoundAudio { offset, container, info, crc32: 0 });
            }
            Err(Reject::Bad(reason)) => report.rejected.push(Rejected { offset, container, reason }),
            Err(Reject::NoMatch) => {}
        }
    }
    report.audio.par_iter_mut().for_each(|a| {
        a.crc32 = crc32fast::hash(&data[a.offset as usize..a.end() as usize]);
    });
    report
}

/// Check for audio of a given format at an exact offset.
pub fn audio_at(data: &[u8], offset: u64, container: Container) -> Result<FoundAudio, Reject> {
    let pos = usize::try_from(offset).ok().filter(|&p| p <= data.len()).ok_or(Reject::NoMatch)?;
    let info = format_for(container).parse(&data[pos..])?;
    let crc32 = crc32fast::hash(&data[pos..pos + info.size as usize]);
    Ok(FoundAudio { offset, container, info, crc32 })
}
