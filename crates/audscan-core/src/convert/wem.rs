//! The parts of a WEM (a Wwise RIFF/RIFX WAVE) the converters need: where its chunks are,
//! and the fmt fields, in the file's byte order.

use std::ops::Range;

use super::ConvertError;

pub(crate) struct Wem<'a> {
    pub data: &'a [u8],
    pub big_endian: bool,
    pub fmt: Range<usize>,
    /// The `data` chunk's body.
    pub body: Range<usize>,
    pub vorb: Option<Range<usize>>,
    pub seek: Option<Range<usize>>,
    pub smpl: Option<Range<usize>>,
    pub tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub avg_bytes: u32,
    pub block_align: u16,
    pub bits: u16,
}

impl<'a> Wem<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, ConvertError> {
        let not = || ConvertError::NotWem("not a RIFF/RIFX WAVE file".into());
        if data.len() < 12 || &data[8..12] != b"WAVE" {
            return Err(not());
        }
        let big_endian = match &data[..4] {
            b"RIFF" => false,
            b"RIFX" => true,
            _ => return Err(not()),
        };
        let r32 = |pos: usize| read_u32(data, pos, big_endian);
        let declared = 8 + r32(4) as usize;
        if declared > data.len() {
            return Err(ConvertError::Partial { have: data.len(), declared });
        }
        let (mut fmt, mut body, mut vorb, mut seek, mut smpl) = (None, None, None, None, None);
        let mut pos = 12;
        while pos + 8 <= declared {
            let id = &data[pos..pos + 4];
            let start = pos + 8;
            let end = start.saturating_add(r32(pos + 4) as usize);
            if end > declared {
                break;
            }
            let range = Some(start..end);
            match id {
                b"fmt " => fmt = fmt.or(range),
                b"data" => body = body.or(range),
                b"vorb" => vorb = vorb.or(range),
                b"seek" => seek = seek.or(range),
                b"smpl" => smpl = smpl.or(range),
                _ => {}
            }
            pos = end + (end - start) % 2;
        }
        let fmt = fmt.filter(|f| f.len() >= 16).ok_or_else(|| ConvertError::NotWem("no fmt chunk".into()))?;
        let body = body.ok_or_else(|| ConvertError::NotWem("no data chunk".into()))?;
        let f = fmt.start;
        let r16 = |pos: usize| read_u16(data, pos, big_endian);
        Ok(Self {
            data,
            big_endian,
            tag: r16(f),
            channels: r16(f + 2),
            sample_rate: r32(f + 4),
            avg_bytes: r32(f + 8),
            block_align: r16(f + 12),
            bits: r16(f + 14),
            fmt,
            body,
            vorb,
            seek,
            smpl,
        })
    }

    pub fn u16(&self, pos: usize) -> Result<u16, ConvertError> {
        self.data.get(pos..pos + 2).ok_or_else(truncated)?;
        Ok(read_u16(self.data, pos, self.big_endian))
    }

    pub fn u32(&self, pos: usize) -> Result<u32, ConvertError> {
        self.data.get(pos..pos + 4).ok_or_else(truncated)?;
        Ok(read_u32(self.data, pos, self.big_endian))
    }

    /// The fmt chunk's size.
    pub fn fmt_len(&self) -> usize {
        self.fmt.len()
    }

    /// The single loop from a `smpl` chunk (start, end inclusive), if there's exactly one.
    pub fn loop_points(&self) -> Option<(u32, u32)> {
        let s = self.smpl.clone()?;
        if s.len() < 0x34 || self.u32(s.start + 0x1C).ok()? != 1 {
            return None;
        }
        Some((self.u32(s.start + 0x2C).ok()?, self.u32(s.start + 0x30).ok()?))
    }
}

fn truncated() -> ConvertError {
    ConvertError::Invalid("a header field lies past the end of the file".into())
}

fn read_u16(data: &[u8], pos: usize, big_endian: bool) -> u16 {
    let b = [data[pos], data[pos + 1]];
    if big_endian { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) }
}

fn read_u32(data: &[u8], pos: usize, big_endian: bool) -> u32 {
    let b = data[pos..pos + 4].try_into().unwrap();
    if big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
}
