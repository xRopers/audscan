//! Deterministic test fixtures: binary blobs with audio at known offsets, plus traps.
//!
//! Everything here is written independently of audscan-core, with its own RIFF, FSB5 and
//! Ogg writers and its own (bitwise) Ogg CRC, so the tests check the core against a second
//! opinion. `gen-fixtures` writes them to `tests/fixtures`.

/// A small xorshift generator, so fixtures are the same on every run.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next_u64() as u8).collect()
    }
}

/// A sound the scanner should report, as the fixture wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Expected {
    pub offset: usize,
    pub size: usize,
    /// `riff`, `fsb5`, `ogg`, `bnk` or `pck`.
    pub container: &'static str,
    /// `wav`, `wem`, `fsb5`, `ogg`, `bnk` or `pck`, with ` BE` for big-endian files.
    pub label: &'static str,
    pub codec: &'static str,
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Option<u64>,
    /// A bank's or package's contents.
    pub tracks: Vec<ExpectedTrack>,
    /// Whether the scanner should attach a note (an Ogg stream with no EOS page).
    pub noted: bool,
    pub crc32: u32,
}

/// One sound (or SoundBank) inside a bank or package.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedTrack {
    /// FSB5's name, or the Wwise ID in decimal.
    pub name: String,
    pub language: Option<&'static str>,
    /// Per track for Wwise; `None` for FSB5.
    pub codec: Option<&'static str>,
    /// `wem` or `bnk` for Wwise; `None` for FSB5.
    pub extension: Option<&'static str>,
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Option<u64>,
    /// Prefetch media (the start of a streamed WEM): noted.
    pub noted: bool,
    /// Where it is from the start of the bank or package, and its size (Wwise only: the
    /// FSB5 tests check those separately).
    pub range: Option<(usize, usize)>,
}

impl ExpectedTrack {
    pub fn fsb(name: Option<&str>, channels: u16, sample_rate: u32, samples: u64) -> Self {
        let name = name.unwrap_or("").to_string();
        Self { name, language: None, codec: None, extension: None, channels, sample_rate, samples: Some(samples), noted: false, range: None }
    }
}

/// A header the scanner should report as unusable.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedAt {
    pub offset: usize,
    pub reason_prefix: &'static str,
}

pub struct Fixture {
    pub name: &'static str,
    pub description: &'static str,
    pub data: Vec<u8>,
    pub expected: Vec<Expected>,
    pub rejected: Vec<RejectedAt>,
}

/// What an expected sound is, apart from where it lands.
pub struct Meta {
    pub container: &'static str,
    pub label: &'static str,
    pub codec: &'static str,
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Option<u64>,
    pub tracks: Vec<ExpectedTrack>,
    pub noted: bool,
}

impl Meta {
    pub fn new(container: &'static str, label: &'static str, codec: &'static str, channels: u16, sample_rate: u32, samples: Option<u64>) -> Self {
        Self { container, label, codec, channels, sample_rate, samples, tracks: Vec::new(), noted: false }
    }
}

struct Builder {
    fixture: Fixture,
    rng: Rng,
}

impl Builder {
    fn new(name: &'static str, description: &'static str, seed: u64) -> Self {
        let fixture = Fixture { name, description, data: Vec::new(), expected: Vec::new(), rejected: Vec::new() };
        Self { fixture, rng: Rng::new(seed) }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.fixture.data.extend_from_slice(bytes);
    }

    /// Random bytes between items.
    fn gap(&mut self) {
        let n = 16 + self.rng.below(48);
        let bytes = self.rng.bytes(n);
        self.raw(&bytes);
    }

    fn audio(&mut self, bytes: &[u8], m: Meta) {
        let e = Expected {
            offset: self.fixture.data.len(),
            size: bytes.len(),
            container: m.container,
            label: m.label,
            codec: m.codec,
            channels: m.channels,
            sample_rate: m.sample_rate,
            samples: m.samples,
            tracks: m.tracks,
            noted: m.noted,
            crc32: crc32fast::hash(bytes),
        };
        self.fixture.expected.push(e);
        self.raw(bytes);
    }

    fn reject(&mut self, bytes: &[u8], reason_prefix: &'static str) {
        self.fixture.rejected.push(RejectedAt { offset: self.fixture.data.len(), reason_prefix });
        self.raw(bytes);
    }
}

// ---- RIFF -----------------------------------------------------------------------------

fn u16_bytes(v: u16, big: bool) -> [u8; 2] {
    if big { v.to_be_bytes() } else { v.to_le_bytes() }
}

fn u32_bytes(v: u32, big: bool) -> [u8; 4] {
    if big { v.to_be_bytes() } else { v.to_le_bytes() }
}

/// A RIFF (or big-endian RIFX) file of the given form and chunks, odd chunks padded.
pub fn riff(big: bool, form: &[u8; 4], chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut body = form.to_vec();
    for (id, c) in chunks {
        body.extend_from_slice(*id);
        body.extend_from_slice(&u32_bytes(c.len() as u32, big));
        body.extend_from_slice(c);
        if c.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = if big { b"RIFX".to_vec() } else { b"RIFF".to_vec() };
    out.extend_from_slice(&u32_bytes(body.len() as u32, big));
    out.extend(body);
    out
}

/// A WAVEFORMATEX `fmt ` body: 16 bytes, plus cbSize and `extra` if there is any.
pub fn fmt(big: bool, tag: u16, channels: u16, rate: u32, block_align: u16, bits: u16, extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u16_bytes(tag, big));
    out.extend_from_slice(&u16_bytes(channels, big));
    out.extend_from_slice(&u32_bytes(rate, big));
    out.extend_from_slice(&u32_bytes(rate * u32::from(block_align), big));
    out.extend_from_slice(&u16_bytes(block_align, big));
    out.extend_from_slice(&u16_bytes(bits, big));
    if !extra.is_empty() {
        out.extend_from_slice(&u16_bytes(extra.len() as u16, big));
        out.extend_from_slice(extra);
    }
    out
}

/// A plain PCM WAV with a LIST chunk in front of the data.
pub fn pcm_wav(channels: u16, rate: u32, bits: u16, frames: usize, rng: &mut Rng) -> Vec<u8> {
    let align = channels * bits / 8;
    riff(false, b"WAVE", &[
        (b"fmt ", fmt(false, 1, channels, rate, align, bits, &[])),
        (b"LIST", b"INFOISFT\x05\0\0\0test\0".to_vec()),
        (b"data", rng.bytes(frames * align as usize)),
    ])
}

// ---- FSB5 -----------------------------------------------------------------------------

pub struct FsbTrack {
    pub name: Option<&'static str>,
    pub channels: u16,
    pub rate: u32,
    pub samples: u64,
    pub data_len: usize,
}

const FSB_RATES: [u32; 11] = [4000, 8000, 11000, 11025, 16000, 22050, 24000, 32000, 44100, 48000, 96000];

/// An FSB5 bank. Rates and channel counts the packed header can't hold go in extra
/// chunks (type 2 and 1), and a Vorbis bank gets a type-11 setup chunk to skip over.
pub fn fsb5(version: u32, codec: u32, tracks: &[FsbTrack], rng: &mut Rng) -> Vec<u8> {
    let (mut headers, mut data) = (Vec::new(), Vec::new());
    for t in tracks {
        while data.len() % 32 != 0 {
            data.push(0);
        }
        let mut extra: Vec<(u32, Vec<u8>)> = Vec::new();
        let rate_index = FSB_RATES.iter().position(|&r| r == t.rate).unwrap_or_else(|| {
            extra.push((2, t.rate.to_le_bytes().to_vec()));
            8
        });
        let channel_code = [1, 2, 6, 8].iter().position(|&c| c == t.channels).unwrap_or_else(|| {
            extra.push((1, vec![t.channels as u8]));
            0
        });
        if codec == 15 {
            extra.push((11, rng.bytes(8)));
        }
        let packed = u64::from(!extra.is_empty())
            | (rate_index as u64) << 1
            | (channel_code as u64) << 5
            | (data.len() as u64 / 32) << 7
            | t.samples << 34;
        headers.extend_from_slice(&packed.to_le_bytes());
        for (i, (kind, body)) in extra.iter().enumerate() {
            let more = u32::from(i + 1 < extra.len());
            headers.extend_from_slice(&(more | (body.len() as u32) << 1 | kind << 25).to_le_bytes());
            headers.extend_from_slice(body);
        }
        data.extend(rng.bytes(t.data_len));
    }
    let mut names = Vec::new();
    if tracks.iter().any(|t| t.name.is_some()) {
        let mut strings = Vec::new();
        for t in tracks {
            names.extend_from_slice(&((4 * tracks.len() + strings.len()) as u32).to_le_bytes());
            strings.extend_from_slice(t.name.unwrap_or("").as_bytes());
            strings.push(0);
        }
        names.extend(strings);
    }
    let mut out = b"FSB5".to_vec();
    for v in [version, tracks.len() as u32, headers.len() as u32, names.len() as u32, data.len() as u32, codec] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.resize(if version == 0 { 0x40 } else { 0x3C }, 0);
    out.extend(headers);
    out.extend(names);
    out.extend(data);
    out
}

// ---- Ogg ------------------------------------------------------------------------------

/// Ogg's CRC, bit by bit.
fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in page {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

pub const BOS: u8 = 2;
pub const EOS: u8 = 4;

/// One page holding `body` (as a single packet).
pub fn ogg_page(flags: u8, granule: i64, serial: u32, sequence: u32, body: &[u8]) -> Vec<u8> {
    let mut segments = vec![255u8; body.len() / 255];
    segments.push((body.len() % 255) as u8);
    let mut page = b"OggS\0".to_vec();
    page.push(flags);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&[0; 4]);
    page.push(segments.len() as u8);
    page.extend(segments);
    page.extend_from_slice(body);
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// A whole logical stream: a BOS page with `head`, then a page of random data per
/// `(length, granule)`, the last one marked EOS unless `end` is false.
pub fn ogg_stream(serial: u32, head: &[u8], pages: &[(usize, i64)], end: bool, rng: &mut Rng) -> Vec<u8> {
    let mut out = ogg_page(BOS, 0, serial, 0, head);
    for (i, &(len, granule)) in pages.iter().enumerate() {
        let flags = if end && i + 1 == pages.len() { EOS } else { 0 };
        out.extend(ogg_page(flags, granule, serial, i as u32 + 1, &rng.bytes(len)));
    }
    out
}

pub fn vorbis_head(channels: u8, rate: u32) -> Vec<u8> {
    let mut p = b"\x01vorbis".to_vec();
    p.extend_from_slice(&0u32.to_le_bytes());
    p.push(channels);
    p.extend_from_slice(&rate.to_le_bytes());
    p.extend_from_slice(&[0; 12]); // bitrates
    p.push(0xB8); // block sizes 256 / 2048
    p.push(1); // framing
    p
}

pub fn opus_head(channels: u8, preskip: u16) -> Vec<u8> {
    let mut p = b"OpusHead\x01".to_vec();
    p.push(channels);
    p.extend_from_slice(&preskip.to_le_bytes());
    p.extend_from_slice(&44100u32.to_le_bytes()); // input rate: informational only
    p.extend_from_slice(&[0, 0, 0]); // gain, mapping family
    p
}

pub fn flac_head(channels: u8, rate: u32, total_samples: u64) -> Vec<u8> {
    let mut p = b"\x7fFLAC\x01\x00\x00\x01fLaC".to_vec();
    p.extend_from_slice(&[0, 0, 0, 34]); // STREAMINFO block header
    p.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]); // block and frame sizes
    let packed = u64::from(rate) << 44 | u64::from(channels - 1) << 41 | 15 << 36 | total_samples;
    p.extend_from_slice(&packed.to_be_bytes());
    p.extend_from_slice(&[0; 16]); // MD5
    p
}

// ---- Wwise ----------------------------------------------------------------------------

/// A Wwise Vorbis WEM (sample count inside a 0x42-byte fmt chunk).
pub fn vorbis_wem(big: bool, channels: u16, rate: u32, samples: u32, data_len: usize, rng: &mut Rng) -> Vec<u8> {
    let mut extra = vec![0u8; 48];
    extra[6..10].copy_from_slice(&u32_bytes(samples, big));
    riff(big, b"WAVE", &[(b"fmt ", fmt(big, 0xFFFF, channels, rate, 0, 0, &extra)), (b"data", rng.bytes(data_len))])
}

/// A 16-bit PCM WEM, marked as Wwise's by an `akd ` chunk.
pub fn pcm_wem(channels: u16, rate: u32, frames: usize, rng: &mut Rng) -> Vec<u8> {
    let align = channels * 2;
    riff(false, b"WAVE", &[
        (b"fmt ", fmt(false, 1, channels, rate, align, 16, &[])),
        (b"akd ", rng.bytes(16)),
        (b"data", rng.bytes(frames * align as usize)),
    ])
}

fn section(tag: &[u8; 4], body: &[u8], big: bool) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend_from_slice(&u32_bytes(body.len() as u32, big));
    out.extend_from_slice(body);
    out
}

/// A SoundBank: BKHD (version, bank ID, padding: 16 bytes), then DIDX and DATA for
/// `media` (ID and bytes, each 16-byte aligned in DATA) if there are any, then HIRC.
/// Returns the bank and where each medium landed in it.
pub fn bnk(big: bool, version: u32, media: &[(u32, Vec<u8>)], rng: &mut Rng) -> (Vec<u8>, Vec<usize>) {
    let mut header = u32_bytes(version, big).to_vec();
    header.extend_from_slice(&u32_bytes(0xB00C_0000 | version, big));
    header.extend_from_slice(&[0; 8]);
    let mut out = section(b"BKHD", &header, big);
    let mut places = Vec::new();
    if !media.is_empty() {
        let (mut index, mut data) = (Vec::new(), Vec::new());
        for (id, bytes) in media {
            while !data.len().is_multiple_of(16) {
                data.push(0);
            }
            for v in [*id, data.len() as u32, bytes.len() as u32] {
                index.extend_from_slice(&u32_bytes(v, big));
            }
            places.push(data.len());
            data.extend_from_slice(bytes);
        }
        out.extend(section(b"DIDX", &index, big));
        let data_start = out.len() + 8;
        out.extend(section(b"DATA", &data, big));
        places.iter_mut().for_each(|p| *p += data_start);
    }
    out.extend(section(b"HIRC", &rng.bytes(30), big));
    (out, places)
}

/// One file in a package's lookup table.
pub struct PckFile {
    pub id: u64,
    pub language: u32,
    pub bytes: Vec<u8>,
}

/// A little-endian AKPK package: a language map (UTF-16 names), lookup tables for banks,
/// streams and (if `externals` is `Some`) external files with 64-bit IDs, then the files
/// at multiples of `block`. Returns it and each file's offset, in table order.
pub fn pck(languages: &[(u32, &str)], banks: &[PckFile], streams: &[PckFile], externals: Option<&[PckFile]>, block: u32) -> (Vec<u8>, Vec<usize>) {
    let mut map = (languages.len() as u32).to_le_bytes().to_vec();
    let mut strings = Vec::new();
    for (id, name) in languages {
        map.extend_from_slice(&((4 + 8 * languages.len() + strings.len()) as u32).to_le_bytes());
        map.extend_from_slice(&id.to_le_bytes());
        strings.extend(name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    map.extend(strings);
    while !map.len().is_multiple_of(4) {
        map.push(0);
    }
    let mut tables: Vec<(&[PckFile], bool)> = vec![(banks, false), (streams, false)];
    tables.extend(externals.map(|e| (e, true)));
    let table_size = |files: &[PckFile], wide: bool| 4 + files.len() * if wide { 24 } else { 20 };
    let fields = 16 + if externals.is_some() { 4 } else { 0 };
    let header_size = fields + map.len() + tables.iter().map(|&(f, w)| table_size(f, w)).sum::<usize>();

    let all: Vec<&PckFile> = tables.iter().flat_map(|(files, _)| files.iter()).collect();
    let mut offsets = Vec::new();
    let mut at = 8 + header_size;
    for f in &all {
        at = at.div_ceil(block as usize) * block as usize;
        offsets.push(at);
        at += f.bytes.len();
    }

    let mut out = b"AKPK".to_vec();
    out.extend_from_slice(&(header_size as u32).to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(map.len() as u32).to_le_bytes());
    for &(files, wide) in &tables {
        out.extend_from_slice(&(table_size(files, wide) as u32).to_le_bytes());
    }
    out.extend(map);
    let mut next = offsets.iter();
    for &(files, wide) in &tables {
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for f in files {
            if wide {
                out.extend_from_slice(&f.id.to_le_bytes());
            } else {
                out.extend_from_slice(&(f.id as u32).to_le_bytes());
            }
            let offset = *next.next().unwrap();
            for v in [block, f.bytes.len() as u32, offset as u32 / block, f.language] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    assert_eq!(out.len(), 8 + header_size);
    for (f, &offset) in all.iter().zip(&offsets) {
        out.resize(offset, 0);
        out.extend_from_slice(&f.bytes);
    }
    (out, offsets)
}

fn wem_track(id: u64, codec: &'static str, channels: u16, rate: u32, samples: Option<u64>, range: (usize, usize)) -> ExpectedTrack {
    ExpectedTrack {
        name: id.to_string(),
        language: None,
        codec: Some(codec),
        extension: Some("wem"),
        channels,
        sample_rate: rate,
        samples,
        noted: false,
        range: Some(range),
    }
}

// ---- Fixtures -------------------------------------------------------------------------

pub fn all() -> Vec<Fixture> {
    vec![audio_archive(), wav_file()]
}

/// A plain PCM WAV file on its own.
pub fn wav_file() -> Fixture {
    let mut b = Builder::new("wav_file", "a single 16-bit mono PCM WAV", 7);
    let wav = pcm_wav(1, 16000, 16, 800, &mut b.rng);
    b.audio(&wav, Meta::new("riff", "wav", "PCM 16-bit", 1, 16000, Some(800)));
    b.fixture
}

/// Every format, byte order and codec path, back to back with random gaps, plus traps.
pub fn audio_archive() -> Fixture {
    let mut b = Builder::new("audio_archive", "WAV, WEM (RIFF and RIFX), FSB5 (also inside a .bank), Ogg Vorbis/Opus/FLAC, chained and multiplexed Ogg, and traps", 1);

    // Magic bytes in text: none of these is followed by a real header.
    b.raw(b"notes: RIFF files, OggS pages and FSB5 banks live here, BKHD sections and AKPK packages too\n");
    b.gap();

    // Plain 16-bit stereo WAV with an odd-sized chunk after the data.
    let mut chunks = vec![
        (b"fmt ", fmt(false, 1, 2, 44100, 4, 16, &[])),
        (b"LIST", b"INFOodd".to_vec()),
        (b"data", b.rng.bytes(4000)),
    ];
    chunks.push((b"id3 ", b"ID3v2".to_vec()));
    b.audio(&riff(false, b"WAVE", &chunks), Meta::new("riff", "wav", "PCM 16-bit", 2, 44100, Some(1000)));
    b.gap();

    // RIFX Wwise Vorbis with its sample count inside a 0x42-byte fmt chunk.
    let mut extra = vec![0u8; 48];
    extra[6..10].copy_from_slice(&480_000u32.to_be_bytes());
    let wem = riff(true, b"WAVE", &[(b"fmt ", fmt(true, 0xFFFF, 2, 48000, 0, 0, &extra)), (b"data", b.rng.bytes(3000))]);
    b.audio(&wem, Meta::new("riff", "wem BE", "Wwise Vorbis", 2, 48000, Some(480_000)));
    b.gap();

    // Little-endian Wwise Vorbis with a separate vorb chunk.
    let mut vorb = b.rng.bytes(42);
    vorb[..4].copy_from_slice(&96_000u32.to_le_bytes());
    let wem = riff(false, b"WAVE", &[
        (b"fmt ", fmt(false, 0xFFFF, 1, 32000, 0, 0, &[0; 6])),
        (b"vorb", vorb),
        (b"data", b.rng.bytes(1500)),
    ]);
    b.audio(&wem, Meta::new("riff", "wem", "Wwise Vorbis", 1, 32000, Some(96_000)));
    b.gap();

    // A PCM WEM, known as Wwise's only by its akd chunk.
    let wem = riff(false, b"WAVE", &[
        (b"fmt ", fmt(false, 1, 1, 48000, 2, 16, &[])),
        (b"akd ", b.rng.bytes(16)),
        (b"data", b.rng.bytes(600)),
    ]);
    b.audio(&wem, Meta::new("riff", "wem", "PCM 16-bit", 1, 48000, Some(300)));
    b.gap();

    // MS ADPCM: the length comes from the fact chunk.
    let adpcm = riff(false, b"WAVE", &[
        (b"fmt ", fmt(false, 2, 1, 22050, 256, 4, &b.rng.bytes(32))),
        (b"fact", 1234u32.to_le_bytes().to_vec()),
        (b"data", b.rng.bytes(512)),
    ]);
    b.audio(&adpcm, Meta::new("riff", "wav", "MS ADPCM", 1, 22050, Some(1234)));
    b.gap();

    // WAVE_FORMAT_EXTENSIBLE, 5.1 float.
    let mut ext = vec![32, 0, 0x3F, 0, 0, 0]; // valid bits, channel mask
    ext.extend_from_slice(&3u16.to_le_bytes()); // subformat: IEEE float
    ext.extend_from_slice(b"\0\0\0\0\x10\0\x80\0\0\xAA\0\x38\x9B\x71");
    let float = riff(false, b"WAVE", &[(b"fmt ", fmt(false, 0xFFFE, 6, 48000, 24, 32, &ext)), (b"data", b.rng.bytes(2400))]);
    b.audio(&float, Meta::new("riff", "wav", "IEEE float 32-bit", 6, 48000, Some(100)));
    b.gap();

    // An FMOD Studio .bank: a RIFF "FEV " form (not audio itself) with an FSB5 inside.
    let tracks = [
        FsbTrack { name: Some("music_intro"), channels: 2, rate: 44100, samples: 441_000, data_len: 700 },
        FsbTrack { name: Some("vo_line_01"), channels: 1, rate: 24000, samples: 36_000, data_len: 300 },
        FsbTrack { name: Some("amb_odd"), channels: 3, rate: 12345, samples: 12_345, data_len: 250 },
    ];
    let fsb = fsb5(1, 15, &tracks, &mut b.rng);
    let bank = riff(false, b"FEV ", &[(b"FMT ", vec![0; 12]), (b"SND ", fsb.clone())]);
    let at = 12 + 8 + 12 + 8;
    assert_eq!(&bank[at..at + 4], b"FSB5");
    b.raw(&bank[..at]);
    let mut m = Meta::new("fsb5", "fsb5", "Vorbis", 2, 44100, None);
    m.tracks = tracks.iter().map(|t| ExpectedTrack::fsb(t.name, t.channels, t.rate, t.samples)).collect();
    b.audio(&fsb, m);
    b.raw(&bank[at + fsb.len()..]);
    b.gap();

    // Version 0 FSB5 (0x40-byte header), one unnamed PCM track.
    let fsb = fsb5(0, 2, &[FsbTrack { name: None, channels: 1, rate: 22050, samples: 500, data_len: 1000 }], &mut b.rng);
    let mut m = Meta::new("fsb5", "fsb5", "PCM 16-bit", 1, 22050, Some(500));
    m.tracks = vec![ExpectedTrack::fsb(None, 1, 22050, 500)];
    b.audio(&fsb, m);
    b.gap();

    // Ogg Vorbis over several pages, including a 510-byte packet (segments 255, 255, 0).
    let ogg = ogg_stream(0x1111, &vorbis_head(2, 44100), &[(600, 44100), (510, 88200), (100, 100_000)], true, &mut b.rng);
    b.audio(&ogg, Meta::new("ogg", "ogg", "Vorbis", 2, 44100, Some(100_000)));
    b.gap();

    // Ogg Opus: the pre-skip comes off the length.
    let ogg = ogg_stream(0x2222, &opus_head(2, 312), &[(300, 48000), (200, 96_312)], true, &mut b.rng);
    b.audio(&ogg, Meta::new("ogg", "ogg", "Opus", 2, 48000, Some(96_000)));
    b.gap();

    // A chained file: FLAC then Vorbis, back to back, found as two.
    let flac = ogg_stream(0x3333, &flac_head(1, 44100, 5000), &[(400, 4096), (10, 5000)], true, &mut b.rng);
    b.audio(&flac, Meta::new("ogg", "ogg", "FLAC", 1, 44100, Some(5000)));
    let ogg = ogg_stream(0x4444, &vorbis_head(1, 22050), &[(50, 2000)], true, &mut b.rng);
    b.audio(&ogg, Meta::new("ogg", "ogg", "Vorbis", 1, 22050, Some(2000)));
    b.gap();

    // Theora video multiplexed with Vorbis audio: one file, the audio's details.
    let mut theora = b"\x80theora".to_vec();
    theora.resize(42, 0);
    let mut muxed = ogg_page(BOS, 0, 7, 0, &theora);
    muxed.extend(ogg_page(BOS, 0, 8, 0, &vorbis_head(2, 48000)));
    muxed.extend(ogg_page(0, 10, 7, 1, &b.rng.bytes(100)));
    muxed.extend(ogg_page(0, 1000, 8, 1, &b.rng.bytes(200)));
    muxed.extend(ogg_page(EOS, 20, 7, 2, &b.rng.bytes(50)));
    muxed.extend(ogg_page(EOS, 3000, 8, 2, &b.rng.bytes(80)));
    b.audio(&muxed, Meta::new("ogg", "ogg", "Theora + Vorbis", 2, 48000, Some(3000)));
    b.gap();

    // An Ogg stream with no EOS page: found, with a note.
    let ogg = ogg_stream(0x5555, &vorbis_head(1, 8000), &[(90, 800), (90, 1600)], false, &mut b.rng);
    let mut m = Meta::new("ogg", "ogg", "Vorbis", 1, 8000, Some(1600));
    m.noted = true;
    b.audio(&ogg, m);
    b.gap();

    // A WAV whose data holds a whole Ogg stream: only the WAV is found.
    let mut data = b.rng.bytes(100);
    data.extend(ogg_stream(0x6666, &vorbis_head(1, 8000), &[(64, 64)], true, &mut b.rng));
    data.extend(b.rng.bytes(99));
    let len = data.len() as u64;
    let wav = riff(false, b"WAVE", &[(b"fmt ", fmt(false, 1, 1, 8000, 1, 8, &[])), (b"data", data)]);
    b.audio(&wav, Meta::new("riff", "wav", "PCM 8-bit", 1, 8000, Some(len)));
    b.gap();

    // A RIFF size 4 bytes short, as written by tools that assume a 16-byte fmt chunk when
    // it's 20: the data chunk shows where it really ends.
    let mut wav = riff(false, b"WAVE", &[(b"fmt ", fmt(false, 3, 1, 48000, 4, 32, &[0, 0])), (b"data", b.rng.bytes(400))]);
    let short = u32::from_le_bytes(wav[4..8].try_into().unwrap()) - 4;
    wav[4..8].copy_from_slice(&short.to_le_bytes());
    let mut m = Meta::new("riff", "wav", "IEEE float 32-bit", 1, 48000, Some(100));
    m.noted = true;
    b.audio(&wav, m);
    b.gap();

    // A SoundBank with a PCM WEM, a Wwise Vorbis WEM and prefetch media: the first 600
    // bytes of a streamed WEM whose header says it's much longer.
    let pcm = pcm_wem(1, 48000, 200, &mut b.rng);
    let vorbis = vorbis_wem(false, 2, 44100, 88200, 700, &mut b.rng);
    let mut prefetch = vorbis_wem(false, 2, 48000, 960_000, 20_000, &mut b.rng);
    prefetch.truncate(600);
    let media = [(111, pcm.clone()), (222, vorbis.clone()), (333, prefetch)];
    let (bank, at) = bnk(false, 0x88, &media, &mut b.rng);
    let mut m = Meta::new("bnk", "bnk", "mixed", 1, 48000, None);
    m.tracks = vec![
        wem_track(111, "PCM 16-bit", 1, 48000, Some(200), (at[0], pcm.len())),
        wem_track(222, "Wwise Vorbis", 2, 44100, Some(88200), (at[1], vorbis.len())),
        ExpectedTrack { noted: true, ..wem_track(333, "Wwise Vorbis", 2, 48000, Some(960_000), (at[2], 600)) },
    ];
    b.audio(&bank, m);
    b.gap();

    // A big-endian bank (an older console) holding a RIFX WEM.
    let wem = vorbis_wem(true, 1, 32000, 64000, 300, &mut b.rng);
    let (bank, at) = bnk(true, 0x30, &[(0xABCD, wem.clone())], &mut b.rng);
    let mut m = Meta::new("bnk", "bnk BE", "Wwise Vorbis", 1, 32000, Some(64000));
    m.tracks = vec![wem_track(0xABCD, "Wwise Vorbis", 1, 32000, Some(64000), (at[0], wem.len()))];
    b.audio(&bank, m);
    b.gap();

    // A bank of events only, no media.
    let (bank, _) = bnk(false, 0x86, &[], &mut b.rng);
    b.audio(&bank, Meta::new("bnk", "bnk", "no media", 0, 0, None));
    b.gap();

    // A package with a SoundBank, two streams with the same ID in different languages,
    // and an external file with a 64-bit ID.
    let (inner, _) = bnk(false, 0x88, &[(5, pcm_wem(1, 22050, 50, &mut b.rng))], &mut b.rng);
    let streams = [
        PckFile { id: 100, language: 0, bytes: vorbis_wem(false, 2, 48000, 48000, 400, &mut b.rng) },
        PckFile { id: 100, language: 1, bytes: vorbis_wem(false, 1, 48000, 24000, 200, &mut b.rng) },
    ];
    let externals = [PckFile { id: 0x1_0000_0001, language: 0, bytes: vorbis_wem(false, 2, 44100, 44100, 250, &mut b.rng) }];
    let banks = [PckFile { id: 777, language: 0, bytes: inner.clone() }];
    let (package, at) = pck(&[(0, "sfx"), (1, "english(us)")], &banks, &streams, Some(&externals), 16);
    let mut m = Meta::new("pck", "pck", "Wwise Vorbis", 2, 48000, None);
    m.tracks = vec![
        ExpectedTrack {
            name: "777".into(),
            language: Some("sfx"),
            codec: Some("SoundBank"),
            extension: Some("bnk"),
            channels: 0,
            sample_rate: 0,
            samples: None,
            noted: false,
            range: Some((at[0], inner.len())),
        },
        ExpectedTrack { language: Some("sfx"), ..wem_track(100, "Wwise Vorbis", 2, 48000, Some(48000), (at[1], streams[0].bytes.len())) },
        ExpectedTrack {
            language: Some("english(us)"),
            ..wem_track(100, "Wwise Vorbis", 1, 48000, Some(24000), (at[2], streams[1].bytes.len()))
        },
        ExpectedTrack {
            language: Some("sfx"),
            ..wem_track(0x1_0000_0001, "Wwise Vorbis", 2, 44100, Some(44100), (at[3], externals[0].bytes.len()))
        },
    ];
    b.audio(&package, m);
    b.gap();

    // An older package: no external table, 2 KiB blocks.
    let stream = [PckFile { id: 9, language: 0, bytes: pcm_wem(2, 44100, 100, &mut b.rng) }];
    let (package, at) = pck(&[(0, "sfx")], &[], &stream, None, 2048);
    let mut m = Meta::new("pck", "pck", "PCM 16-bit", 2, 44100, Some(100));
    m.tracks = vec![ExpectedTrack { language: Some("sfx"), ..wem_track(9, "PCM 16-bit", 2, 44100, Some(100), (at[0], stream[0].bytes.len())) }];
    b.audio(&package, m);
    b.gap();

    // Traps. Not audio: an AVI, and an Ogg page from the middle of a stream.
    b.raw(&riff(false, b"AVI ", &[(b"avih", vec![0; 56])]));
    b.gap();
    let page = ogg_page(0, 5000, 0x7777, 3, &b.rng.bytes(40));
    b.raw(&page);
    b.gap();
    // Audio, but unusable.
    let no_fmt = riff(false, b"WAVE", &[(b"data", b.rng.bytes(100))]);
    b.reject(&no_fmt, "no fmt chunk");
    b.gap();
    let bad = fsb5(1, 99, &[FsbTrack { name: None, channels: 1, rate: 8000, samples: 10, data_len: 32 }], &mut b.rng);
    b.reject(&bad, "unknown codec 99");
    b.gap();
    // A bank whose index says its medium is bigger than DATA: the first DIDX entry's size
    // is at 16 (BKHD) + 8 (its section header) + 8 (DIDX's) + 8 (ID, offset).
    let (mut bank, _) = bnk(false, 0x88, &[(1, b.rng.bytes(64))], &mut b.rng);
    assert_eq!(bank[40..44], 64u32.to_le_bytes());
    bank[40..44].copy_from_slice(&4096u32.to_le_bytes());
    b.reject(&bank, "media 1 runs past the end of the DATA section");
    b.gap();
    // A package whose only file starts far past the end of the input: its start block is at
    // 24 (fixed fields) + 20 (map) + 4 (empty bank table) + 4 (count) + 12.
    let stream = [PckFile { id: 9, language: 0, bytes: b.rng.bytes(40) }];
    let (mut package, at) = pck(&[(0, "sfx")], &[], &stream, None, 16);
    assert_eq!(package[64..68], (at[0] as u32 / 16).to_le_bytes());
    package[64..68].copy_from_slice(&0x0100_0000u32.to_le_bytes());
    b.reject(&package, "file 9 runs past the end of the input");
    b.gap();
    let mut page = ogg_page(BOS, 0, 0x8888, 0, &vorbis_head(1, 8000));
    page[30] ^= 0xFF;
    b.reject(&page, "the first page's CRC doesn't match");
    b.gap();
    // Last: a WAV cut off by the end of the file.
    let wav = pcm_wav(2, 44100, 16, 500, &mut b.rng);
    b.reject(&wav[..wav.len() / 2], "runs past the end of the file");
    b.fixture
}
