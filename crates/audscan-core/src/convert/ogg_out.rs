//! Writing an Ogg stream: packets laid into pages (up to about 4 KiB each, 255 segments
//! at most), each page carrying the granule position of the last packet that ends on it.

use crate::formats::ogg::crc;

/// Pages are closed once their data reaches this size, after the packet that crosses it.
const PAGE_TARGET: usize = 4096;

pub(crate) struct OggWriter {
    out: Vec<u8>,
    serial: u32,
    sequence: u32,
    /// The page being filled: its lacing values and data.
    segments: Vec<u8>,
    body: Vec<u8>,
    /// Granule of the last packet completed on this page, if any has been.
    granule: Option<i64>,
    /// The page starts with the rest of a packet from the previous page.
    continued: bool,
    first: bool,
    /// Where the last page written starts.
    last_page: usize,
}

impl OggWriter {
    pub fn new(serial: u32) -> Self {
        Self { out: Vec::new(), serial, sequence: 0, segments: Vec::new(), body: Vec::new(), granule: None, continued: false, first: true, last_page: 0 }
    }

    /// Add a packet ending at `granule`. The page is closed after it if it's full enough.
    /// A packet that doesn't fit in what's left of the page's 255 segments starts a new
    /// page rather than being split: valid either way, but some decoders (vgmstream's Ogg
    /// Opus path, seen on a real file) lose a packet split across pages.
    pub fn packet(&mut self, packet: &[u8], granule: i64) {
        if self.segments.len() + packet.len() / 255 + 1 > 255 {
            self.flush();
        }
        let mut rest = packet;
        loop {
            if self.segments.len() == 255 {
                // Full in the middle of a packet: it continues on the next page.
                self.write_page(false);
                self.continued = true;
            }
            let take = rest.len().min(255);
            self.segments.push(take as u8);
            self.body.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if take < 255 {
                break;
            }
        }
        self.granule = Some(granule);
        if self.body.len() >= PAGE_TARGET {
            self.flush();
        }
    }

    /// Close the current page, if it has anything on it.
    pub fn flush(&mut self) {
        if !self.segments.is_empty() {
            self.write_page(false);
        }
    }

    /// Close the last page, marked end of stream, and return the whole stream. If the
    /// last page was already written, it's marked instead of adding an empty one.
    pub fn finish(mut self) -> Vec<u8> {
        if self.segments.is_empty() && !self.out.is_empty() {
            let start = self.last_page;
            self.out[start + 5] |= 4;
            self.out[start + 22..start + 26].fill(0);
            let checksum = crc(&self.out[start..]);
            self.out[start + 22..start + 26].copy_from_slice(&checksum.to_le_bytes());
        } else {
            self.write_page(true);
        }
        self.out
    }

    fn write_page(&mut self, last: bool) {
        let flags = u8::from(self.continued) | u8::from(self.first) << 1 | u8::from(last) << 2;
        let start = self.out.len();
        self.last_page = start;
        self.out.extend_from_slice(b"OggS\0");
        self.out.push(flags);
        self.out.extend_from_slice(&self.granule.unwrap_or(-1).to_le_bytes());
        self.out.extend_from_slice(&self.serial.to_le_bytes());
        self.out.extend_from_slice(&self.sequence.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.push(self.segments.len() as u8);
        self.out.extend_from_slice(&self.segments);
        self.out.extend_from_slice(&self.body);
        let checksum = crc(&self.out[start..]);
        self.out[start + 22..start + 26].copy_from_slice(&checksum.to_le_bytes());
        self.sequence += 1;
        self.first = false;
        self.continued = false;
        self.granule = None;
        self.segments.clear();
        self.body.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::AudioFormat;
    use crate::formats::ogg::Ogg;

    #[test]
    fn pages_read_back_as_one_stream() {
        let mut w = OggWriter::new(7);
        let mut head = b"\x01vorbis\0\0\0\0\x01".to_vec();
        head.extend_from_slice(&8000u32.to_le_bytes());
        head.resize(30, 0);
        w.packet(&head, 0);
        w.flush();
        // A packet longer than a page, then one of exactly 255 bytes (needs a 0 lacing value).
        w.packet(&vec![1; 70_000], 100);
        w.packet(&[2; 255], 200);
        let stream = w.finish();
        let info = Ogg.parse(&stream).unwrap();
        assert_eq!((info.size, info.codec.as_str(), info.samples, info.note), (stream.len() as u64, "Vorbis", Some(200), None));

        // A stream whose last page was already closed gets that page marked, not an empty one.
        let mut w = OggWriter::new(7);
        w.packet(&head, 0);
        w.flush();
        w.packet(&vec![3; 5000], 300);
        let stream = w.finish();
        let info = Ogg.parse(&stream).unwrap();
        assert_eq!((info.size, info.samples, info.note), (stream.len() as u64, Some(300), None));
    }
}
