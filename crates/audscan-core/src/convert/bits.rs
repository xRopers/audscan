//! Bit-level reading and writing, least significant bit first, as Vorbis packs its
//! headers and packets.

/// Reads bits from a byte slice, lowest bit of each byte first.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Bits read so far.
    pos: usize,
}

/// A read past the end of the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutOfBits;

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Up to 32 bits, as an unsigned number (the first bit read is the lowest).
    pub fn read(&mut self, bits: u32) -> Result<u32, OutOfBits> {
        debug_assert!(bits <= 32);
        let mut value = 0u32;
        for i in 0..bits {
            let byte = *self.data.get(self.pos / 8).ok_or(OutOfBits)?;
            if byte >> (self.pos % 8) & 1 != 0 {
                value |= 1 << i;
            }
            self.pos += 1;
        }
        Ok(value)
    }

    pub fn bits_read(&self) -> usize {
        self.pos
    }
}

/// Writes bits into bytes, lowest bit of each byte first.
#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used in the last byte (0 when it's full or there is none).
    used: u32,
}

impl BitWriter {
    /// The low `bits` bits of `value`, lowest first.
    pub fn write(&mut self, value: u32, bits: u32) {
        debug_assert!(bits <= 32 && (bits == 32 || value >> bits == 0), "{value} doesn't fit in {bits} bits");
        for i in 0..bits {
            if self.used == 0 {
                self.bytes.push(0);
            }
            if value >> i & 1 != 0 {
                *self.bytes.last_mut().unwrap() |= 1 << self.used;
            }
            self.used = (self.used + 1) % 8;
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if self.used == 0 {
            self.bytes.extend_from_slice(bytes);
        } else {
            bytes.iter().for_each(|&b| self.write(b.into(), 8));
        }
    }

    /// The bytes written, the last one padded with zero bits.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Bits needed to hold `v` (0 for 0), as Vorbis's `ilog`.
pub(crate) fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_lsb_first() {
        let mut w = BitWriter::default();
        w.write(0b101, 3);
        w.write(0x1234, 16);
        w.write(1, 1);
        w.write_bytes(&[0xAB]);
        let bytes = w.into_bytes();
        // 3 bits 101, then 0x1234 from its low bit: the first byte is 0b1010_0101.
        assert_eq!(bytes[0], 0b1010_0101);
        let mut r = BitReader::new(&bytes);
        assert_eq!((r.read(3), r.read(16), r.read(1), r.read(8)), (Ok(0b101), Ok(0x1234), Ok(1), Ok(0xAB)));
        assert_eq!(r.bits_read(), 28);
        assert_eq!(r.read(8), Err(OutOfBits));
        assert_eq!((ilog(0), ilog(1), ilog(7), ilog(8)), (0, 1, 3, 4));
    }
}
