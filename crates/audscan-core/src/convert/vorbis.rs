//! Wwise Vorbis to Ogg Vorbis, without re-encoding (a port of ww2ogg's rebuild, plus
//! revorb's granule positions).
//!
//! Wwise keeps Vorbis packets but strips what a decoder needs to read them:
//! - no identification or comment header: rebuilt from the fmt and `vorb` fields;
//! - a setup header with fields removed and codebooks replaced by 10-bit numbers into a
//!   fixed table (`data/packed_codebooks_aoTuV_603.bin`, from ww2ogg): rebuilt field by
//!   field, each codebook unpacked into the standard form;
//! - its own packet framing (a 2-byte size, or 6 with a granule, before each packet)
//!   instead of Ogg pages;
//! - in "modified" packets (most files since 2012), the packet-type bit and the
//!   window-shape bits left out of each audio packet: put back, the window bits worked out
//!   from the previous and next packets' modes.
//!
//! ww2ogg leaves the granule positions of modern files at 0 (hence tools like revorb);
//! here each packet's is worked out from the block sizes (a packet adds a quarter of the
//! previous block and a quarter of its own), and the last one is set to the sample count
//! Wwise stores, so a decoder trims the end exactly.
//!
//! Layouts: the modern one (`vorb` data inside a 0x42-byte fmt chunk, or a 0x2A-byte
//! `vorb` chunk) and the older 0x32/0x34-byte `vorb` chunks with 6-byte packet headers.
//! The oldest (0x28/0x2C, with the full header triad inline) and inline codebooks aren't
//! handled.

use std::sync::OnceLock;

use super::bits::{BitReader, BitWriter, OutOfBits, ilog};
use super::ogg_out::OggWriter;
use super::wem::Wem;
use super::ConvertError;

static PACKED_CODEBOOKS: &[u8] = include_bytes!("../../data/packed_codebooks_aoTuV_603.bin");

impl From<OutOfBits> for ConvertError {
    fn from(_: OutOfBits) -> Self {
        ConvertError::Invalid("a Vorbis header ends early".into())
    }
}

fn invalid(what: &str) -> ConvertError {
    ConvertError::Invalid(what.into())
}

/// The packed codebooks, by number: slices of the table.
fn codebook(id: u32) -> Option<&'static [u8]> {
    static OFFSETS: OnceLock<Vec<usize>> = OnceLock::new();
    let offsets = OFFSETS.get_or_init(|| {
        let data = PACKED_CODEBOOKS;
        let table = u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap()) as usize;
        data[table..].as_chunks::<4>().0.iter().map(|&c| u32::from_le_bytes(c) as usize).collect()
    });
    // Codebook n runs to where n + 1 starts; the last offset is the end of the last one.
    let id = id as usize;
    let (start, end) = (*offsets.get(id)?, *offsets.get(id + 1)?);
    PACKED_CODEBOOKS.get(start..end)
}

/// Vorbis's `_book_maptype1_quantvals`: the largest n with n^dimensions <= entries.
fn quantvals(entries: u32, dimensions: u32) -> Result<u32, ConvertError> {
    if dimensions == 0 || entries == 0 {
        return Err(invalid("a codebook with no dimensions or entries"));
    }
    let bits = ilog(entries);
    let mut vals = entries >> ((bits - 1) * (dimensions - 1) / dimensions);
    loop {
        let power = |v: u32| (0..dimensions).try_fold(1u64, |acc, _| acc.checked_mul(v.into())).unwrap_or(u64::MAX);
        let (acc, acc1) = (power(vals), power(vals + 1));
        if acc <= entries.into() && acc1 > entries.into() {
            return Ok(vals);
        } else if acc > entries.into() {
            vals -= 1;
        } else {
            vals += 1;
        }
    }
}

/// Unpack one Wwise-packed codebook into the standard Vorbis form.
fn rebuild_codebook(packed: &[u8], w: &mut BitWriter) -> Result<(), ConvertError> {
    let mut r = BitReader::new(packed);
    let (dimensions, entries) = (r.read(4)?, r.read(14)?);
    w.write(0x564342, 24);
    w.write(dimensions, 16);
    w.write(entries, 24);

    let ordered = r.read(1)?;
    w.write(ordered, 1);
    if ordered == 1 {
        w.write(r.read(5)?, 5);
        let mut current = 0;
        while current < entries {
            let bits = ilog(entries - current);
            let number = r.read(bits)?;
            w.write(number, bits);
            current += number;
        }
        if current > entries {
            return Err(invalid("a codebook's ordered lengths overrun its entries"));
        }
    } else {
        let (length_bits, sparse) = (r.read(3)?, r.read(1)?);
        if length_bits == 0 || length_bits > 5 {
            return Err(invalid("a codebook with nonsense codeword lengths"));
        }
        w.write(sparse, 1);
        for _ in 0..entries {
            let present = if sparse == 1 {
                let p = r.read(1)?;
                w.write(p, 1);
                p == 1
            } else {
                true
            };
            if present {
                w.write(r.read(length_bits)?, 5);
            }
        }
    }

    let lookup = r.read(1)?;
    w.write(lookup, 4);
    if lookup == 1 {
        let (min, max, value_bits, sequence) = (r.read(32)?, r.read(32)?, r.read(4)?, r.read(1)?);
        w.write(min, 32);
        w.write(max, 32);
        w.write(value_bits, 4);
        w.write(sequence, 1);
        for _ in 0..quantvals(entries, dimensions)? {
            w.write(r.read(value_bits + 1)?, value_bits + 1);
        }
    }
    // The packed form fills its bytes exactly, plus one (ww2ogg's check).
    if r.bits_read() / 8 + 1 != packed.len() {
        return Err(invalid("a codebook didn't unpack to its packed size"));
    }
    Ok(())
}

/// What the rest of the conversion needs from the setup header.
struct Modes {
    /// Long-window flag of each mode.
    blockflags: Vec<bool>,
    /// Bits of the mode number at the start of each audio packet.
    bits: u32,
}

/// Rebuild the setup header (packet type 5) from Wwise's stripped one.
fn rebuild_setup(stripped: &[u8], channels: u32) -> Result<(Vec<u8>, Modes), ConvertError> {
    let mut s = BitReader::new(stripped);
    let mut w = BitWriter::default();
    w.write_bytes(b"\x05vorbis");

    let copy = |s: &mut BitReader, w: &mut BitWriter, bits: u32| -> Result<u32, ConvertError> {
        let v = s.read(bits)?;
        w.write(v, bits);
        Ok(v)
    };

    let codebooks = copy(&mut s, &mut w, 8)? + 1;
    for _ in 0..codebooks {
        let id = s.read(10)?;
        let packed = codebook(id).ok_or_else(|| {
            ConvertError::Unsupported(format!("codebook {id} isn't in the aoTuV 6.03 table (a Wwise from before 2011.2?)"))
        })?;
        rebuild_codebook(packed, &mut w)?;
    }
    let book = |v: u32| if v < codebooks { Ok(v) } else { Err(invalid("a setup field names a codebook that doesn't exist")) };

    // Time domain transforms: one placeholder.
    w.write(0, 6);
    w.write(0, 16);

    let floors = copy(&mut s, &mut w, 6)? + 1;
    for _ in 0..floors {
        w.write(1, 16); // floor type 1, the only one Wwise uses
        let partitions = copy(&mut s, &mut w, 5)?;
        let classes: Vec<u32> = (0..partitions).map(|_| copy(&mut s, &mut w, 4)).collect::<Result<_, _>>()?;
        // As libvorbis: one entry per class up to the highest used (none with no partitions).
        let max_class = classes.iter().copied().max().map_or(0, |m| m + 1);
        let mut class_dimensions = Vec::new();
        for _ in 0..max_class {
            class_dimensions.push(copy(&mut s, &mut w, 3)? + 1);
            let subclasses = copy(&mut s, &mut w, 2)?;
            if subclasses != 0 {
                book(copy(&mut s, &mut w, 8)?)?;
            }
            for _ in 0..1 << subclasses {
                let plus1 = copy(&mut s, &mut w, 8)?;
                if plus1 > 0 {
                    book(plus1 - 1)?;
                }
            }
        }
        copy(&mut s, &mut w, 2)?; // multiplier
        let range_bits = copy(&mut s, &mut w, 4)?;
        for class in classes {
            for _ in 0..class_dimensions[class as usize] {
                copy(&mut s, &mut w, range_bits)?;
            }
        }
    }

    let residues = copy(&mut s, &mut w, 6)? + 1;
    for _ in 0..residues {
        let kind = s.read(2)?;
        if kind > 2 {
            return Err(invalid("an unknown residue type"));
        }
        w.write(kind, 16);
        for bits in [24, 24, 24] {
            copy(&mut s, &mut w, bits)?; // begin, end, partition size
        }
        let classifications = copy(&mut s, &mut w, 6)? + 1;
        book(copy(&mut s, &mut w, 8)?)?;
        let mut cascade = Vec::new();
        for _ in 0..classifications {
            let low = copy(&mut s, &mut w, 3)?;
            let high = if copy(&mut s, &mut w, 1)? == 1 { copy(&mut s, &mut w, 5)? } else { 0 };
            cascade.push(high * 8 + low);
        }
        for c in cascade {
            for k in 0..8 {
                if c & 1 << k != 0 {
                    book(copy(&mut s, &mut w, 8)?)?;
                }
            }
        }
    }

    let mappings = copy(&mut s, &mut w, 6)? + 1;
    let channel_bits = ilog(channels.saturating_sub(1));
    for _ in 0..mappings {
        w.write(0, 16); // mapping type 0
        let submaps = if copy(&mut s, &mut w, 1)? == 1 { copy(&mut s, &mut w, 4)? + 1 } else { 1 };
        if copy(&mut s, &mut w, 1)? == 1 {
            let steps = copy(&mut s, &mut w, 8)? + 1;
            for _ in 0..steps {
                let (magnitude, angle) = (copy(&mut s, &mut w, channel_bits)?, copy(&mut s, &mut w, channel_bits)?);
                if magnitude == angle || magnitude >= channels || angle >= channels {
                    return Err(invalid("invalid channel coupling"));
                }
            }
        }
        if copy(&mut s, &mut w, 2)? != 0 {
            return Err(invalid("a mapping's reserved field isn't 0"));
        }
        if submaps > 1 {
            for _ in 0..channels {
                if copy(&mut s, &mut w, 4)? >= submaps {
                    return Err(invalid("a channel mapped to a submap that doesn't exist"));
                }
            }
        }
        for _ in 0..submaps {
            copy(&mut s, &mut w, 8)?; // time config
            if copy(&mut s, &mut w, 8)? >= floors || copy(&mut s, &mut w, 8)? >= residues {
                return Err(invalid("a submap names a floor or residue that doesn't exist"));
            }
        }
    }

    let modes = copy(&mut s, &mut w, 6)? + 1;
    let mut blockflags = Vec::new();
    for _ in 0..modes {
        blockflags.push(copy(&mut s, &mut w, 1)? == 1);
        w.write(0, 16); // window type
        w.write(0, 16); // transform type
        if copy(&mut s, &mut w, 8)? >= mappings {
            return Err(invalid("a mode names a mapping that doesn't exist"));
        }
    }
    w.write(1, 1); // framing
    if s.bits_read().div_ceil(8) != stripped.len() {
        return Err(invalid("the setup header didn't rebuild to its size"));
    }
    Ok((w.into_bytes(), Modes { blockflags, bits: ilog(modes - 1) }))
}

/// Where the parts of a Wwise Vorbis stream are, from the `vorb` data.
struct Layout {
    samples: u32,
    setup: usize,
    first_audio: usize,
    blocksize_0: u32,
    blocksize_1: u32,
    /// Packet headers: a 2-byte size, or 6 bytes with a granule too.
    header: usize,
    modified_packets: bool,
}

fn layout(w: &Wem) -> Result<Layout, ConvertError> {
    let (vorb, size) = match (&w.vorb, w.fmt_len()) {
        (Some(v), _) => (v.start, v.len()),
        (None, 0x42) => (w.fmt.start + 0x18, 0x2A),
        (None, _) => return Err(ConvertError::Unsupported("Wwise Vorbis without vorb data".into())),
    };
    let byte = |at: usize| w.data.get(vorb + at).copied().ok_or_else(|| invalid("the vorb data is cut off"));
    match size {
        0x2A => {
            // Seen as 0xD9, 0xCB, 0xBC, 0xB2 when packets are modified (ww2ogg's notes).
            let signal = w.u32(vorb + 4)?;
            Ok(Layout {
                samples: w.u32(vorb)?,
                setup: w.u32(vorb + 0x10)? as usize,
                first_audio: w.u32(vorb + 0x14)? as usize,
                blocksize_0: byte(0x28)?.into(),
                blocksize_1: byte(0x29)?.into(),
                header: 2,
                modified_packets: !matches!(signal, 0x4A | 0x4B | 0x69 | 0x70),
            })
        }
        0x32 | 0x34 => Ok(Layout {
            samples: w.u32(vorb)?,
            setup: w.u32(vorb + 0x18)? as usize,
            first_audio: w.u32(vorb + 0x1C)? as usize,
            blocksize_0: byte(0x30)?.into(),
            blocksize_1: byte(0x31)?.into(),
            header: 6,
            modified_packets: false,
        }),
        _ => Err(ConvertError::Unsupported(format!("the Wwise Vorbis layout with a {size:#x}-byte vorb chunk"))),
    }
}

/// One Wwise packet at `pos` in the data chunk: its payload's range.
fn packet(w: &Wem, pos: usize, header: usize) -> Result<std::ops::Range<usize>, ConvertError> {
    let size = w.u16(pos)? as usize;
    let start = pos + header;
    if start + size > w.body.end {
        return Err(invalid("a packet runs past the end of the data"));
    }
    Ok(start..start + size)
}

pub(crate) fn to_ogg(w: &Wem) -> Result<Vec<u8>, ConvertError> {
    let l = layout(w)?;
    if !(6..=13).contains(&l.blocksize_0) || !(6..=13).contains(&l.blocksize_1) || l.blocksize_0 > l.blocksize_1 {
        return Err(invalid("block sizes out of range"));
    }
    let channels = u32::from(w.channels);
    let mut ogg = OggWriter::new(crc32fast::hash(w.data));

    // Identification header.
    let mut id = b"\x01vorbis".to_vec();
    id.extend_from_slice(&0u32.to_le_bytes());
    id.push(w.channels as u8);
    id.extend_from_slice(&w.sample_rate.to_le_bytes());
    id.extend_from_slice(&0u32.to_le_bytes());
    id.extend_from_slice(&w.avg_bytes.saturating_mul(8).to_le_bytes());
    id.extend_from_slice(&0u32.to_le_bytes());
    id.push((l.blocksize_0 | l.blocksize_1 << 4) as u8);
    id.push(1);
    ogg.packet(&id, 0);
    ogg.flush();

    // Comment header, with the loop points if there are any (as ww2ogg writes them).
    let mut comment = b"\x03vorbis".to_vec();
    let vendor = b"audscan: converted from Audiokinetic Wwise";
    comment.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    comment.extend_from_slice(vendor);
    let loops = w.loop_points().and_then(|(start, end)| {
        let end = if end == 0 { l.samples } else { end + 1 };
        (start < end && end <= l.samples).then(|| [format!("LoopStart={start}"), format!("LoopEnd={end}")])
    });
    let comments = loops.as_ref().map_or(&[][..], |c| &c[..]);
    comment.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        comment.extend_from_slice(&(c.len() as u32).to_le_bytes());
        comment.extend_from_slice(c.as_bytes());
    }
    comment.push(1);
    ogg.packet(&comment, 0);

    // Setup header.
    let setup_at = w.body.start + l.setup;
    let setup = packet(w, setup_at, l.header)?;
    let (setup_header, modes) = rebuild_setup(&w.data[setup.clone()], channels)?;
    ogg.packet(&setup_header, 0);
    ogg.flush();
    if setup.end != w.body.start + l.first_audio {
        return Err(invalid("the first audio packet doesn't follow the setup header"));
    }

    // Audio packets.
    let blocksize = |mode: usize| 1u64 << if modes.blockflags[mode] { l.blocksize_1 } else { l.blocksize_0 };
    let mode_of = |bytes: &[u8], has_type_bit: bool| -> Result<usize, ConvertError> {
        let mut r = BitReader::new(bytes);
        if has_type_bit && r.read(1)? != 0 {
            return Err(invalid("an audio packet isn't marked as audio"));
        }
        let mode = r.read(modes.bits)? as usize;
        if mode >= modes.blockflags.len() {
            return Err(invalid("an audio packet names a mode that doesn't exist"));
        }
        Ok(mode)
    };
    let mut packets = Vec::new();
    let (mut granule, mut previous, mut previous_long) = (0u64, None, false);
    let mut pos = w.body.start + l.first_audio;
    while pos < w.body.end {
        if pos + l.header > w.body.end {
            return Err(invalid("a packet header is cut off"));
        }
        let payload = packet(w, pos, l.header)?;
        pos = payload.end;
        let bytes = &w.data[payload];
        if bytes.is_empty() {
            continue;
        }
        let (out, mode) = if l.modified_packets {
            let mode = mode_of(bytes, false)?;
            let mut r = BitReader::new(bytes);
            r.read(modes.bits)?;
            let remainder = r.read(8 - modes.bits)?;
            let mut out = BitWriter::default();
            out.write(0, 1); // packet type: audio
            out.write(mode as u32, modes.bits);
            if modes.blockflags[mode] {
                // A long window says whether its neighbours are long too.
                let mut next_long = false;
                if pos + l.header <= w.body.end {
                    let next = packet(w, pos, l.header)?;
                    if !next.is_empty() {
                        next_long = modes.blockflags[mode_of(&w.data[next], false)?];
                    }
                }
                out.write(previous_long.into(), 1);
                out.write(next_long.into(), 1);
            }
            out.write(remainder, 8 - modes.bits);
            out.write_bytes(&bytes[1..]);
            previous_long = modes.blockflags[mode];
            (out.into_bytes(), mode)
        } else {
            (bytes.to_vec(), mode_of(bytes, true)?)
        };
        let size = blocksize(mode);
        if let Some(prev) = previous {
            granule += prev / 4 + size / 4;
        }
        previous = Some(size);
        packets.push((out, granule));
    }
    // The last packet ends at the sample count, so the decoder trims what's past it.
    if let Some(last) = packets.last_mut() {
        last.1 = last.1.min(l.samples.into());
    }
    for (out, granule) in &packets {
        ogg.packet(out, *granule as i64);
    }
    Ok(ogg.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_packed_codebook_unpacks() {
        let mut id = 0;
        while let Some(packed) = codebook(id) {
            let mut w = BitWriter::default();
            rebuild_codebook(packed, &mut w).unwrap_or_else(|e| panic!("codebook {id}: {e}"));
            let bytes = w.into_bytes();
            assert_eq!(&bytes[..3], &[0x42, 0x43, 0x56], "codebook {id} starts with BCV");
            id += 1;
        }
        assert!(id > 500, "only {id} codebooks");
    }

    #[test]
    fn quantvals_is_the_integer_root() {
        assert_eq!(quantvals(81, 4).unwrap(), 3);
        assert_eq!(quantvals(80, 4).unwrap(), 2);
        assert_eq!(quantvals(1, 1).unwrap(), 1);
        assert_eq!(quantvals(1000, 3).unwrap(), 10);
    }
}
