//! WEM conversion on synthetic WEMs from the fixture builders (real game files can't be in
//! the repo; conversion was checked on those against vgmstream, see CLAUDE.md).

use std::path::Path;

use audscan_core::{Container, ConvertError, ExtractOptions, Manifest, ScanOptions, SourceInfo, convert_wem, extract_all, scan};
use audscan_fixtures::{Rng, wwise_ima_wem, wwise_opus_wem, wwise_pcm_wem, wwise_vorbis_wem};

/// The packets of a single Ogg stream, in order, and each page's granule position.
fn ogg_packets(ogg: &[u8]) -> (Vec<Vec<u8>>, Vec<i64>) {
    let (mut packets, mut granules, mut current) = (Vec::new(), Vec::new(), Vec::new());
    let mut pos = 0;
    while pos < ogg.len() {
        assert_eq!(&ogg[pos..pos + 4], b"OggS");
        granules.push(i64::from_le_bytes(ogg[pos + 6..pos + 14].try_into().unwrap()));
        let segments = usize::from(ogg[pos + 26]);
        let lacing = &ogg[pos + 27..pos + 27 + segments];
        let mut at = pos + 27 + segments;
        for &l in lacing {
            current.extend_from_slice(&ogg[at..at + usize::from(l)]);
            at += usize::from(l);
            if l < 255 {
                packets.push(std::mem::take(&mut current));
            }
        }
        pos = at;
    }
    (packets, granules)
}

/// Granule after each packet: the first adds nothing, the rest a quarter of the previous
/// block and a quarter of their own.
fn expected_granules(modes: &[u32]) -> Vec<u64> {
    let block = |m: u32| if m == 1 { 2048 } else { 256 };
    let mut g = 0;
    modes.iter().enumerate().map(|(i, &m)| {
        if i > 0 {
            g += block(modes[i - 1]) / 4 + block(m) / 4;
        }
        g
    }).collect()
}

#[test]
fn wwise_vorbis_becomes_standard_ogg_vorbis() {
    let modes = [0, 1, 1, 0, 0, 1, 0, 1, 1, 1, 0];
    let full = *expected_granules(&modes).last().unwrap();
    let samples = full as u32 - 100; // the end trimmed, as Wwise stores it
    let wem = wwise_vorbis_wem(2, samples, 0, &modes, &mut Rng::new(1));
    let c = convert_wem(&wem).unwrap();
    assert_eq!((c.extension, c.codec.as_str(), c.note.as_deref()), ("ogg", "Wwise Vorbis", None));

    // An independent decoder accepts all three rebuilt headers, codebook included.
    let r = lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(&c.bytes)).expect("lewton reads the headers");
    assert_eq!((r.ident_hdr.audio_channels, r.ident_hdr.audio_sample_rate), (2, 48000));
    assert_eq!((r.ident_hdr.blocksize_0, r.ident_hdr.blocksize_1), (8, 11));
    assert!(r.comment_hdr.vendor.contains("audscan"));

    // The stream reads back with the exact length.
    let a = &scan(&c.bytes, &ScanOptions::default()).audio[0];
    assert_eq!((a.container, a.info.size, a.info.samples, a.info.note.as_deref()), (Container::Ogg, c.bytes.len() as u64, Some(u64::from(samples)), None));

    // Audio packets get their type bit and window bits back: type 0, the mode, and for a
    // long block whether the previous and next ones are long.
    let (packets, _) = ogg_packets(&c.bytes);
    let audio = &packets[3..];
    assert_eq!(audio.len(), modes.len());
    for (i, p) in audio.iter().enumerate() {
        assert_eq!(p[0] & 1, 0, "packet {i} is audio");
        assert_eq!(u32::from(p[0] >> 1 & 1), modes[i], "packet {i}'s mode");
        if modes[i] == 1 {
            let prev = i > 0 && modes[i - 1] == 1;
            let next = i + 1 < modes.len() && modes[i + 1] == 1;
            assert_eq!((p[0] >> 2 & 1 == 1, p[0] >> 3 & 1 == 1), (prev, next), "packet {i}'s windows");
        }
    }
}

#[test]
fn every_page_granule_is_right() {
    // Enough packets for several pages.
    let modes: Vec<u32> = (0..600).map(|i| u32::from(i % 5 == 1 || i % 5 == 2)).collect();
    let granules = expected_granules(&modes);
    let samples = *granules.last().unwrap() as u32;
    let c = convert_wem(&wwise_vorbis_wem(1, samples, 0, &modes, &mut Rng::new(2))).unwrap();
    let mut pos = 0;
    let mut packets_so_far = 0usize;
    while pos < c.bytes.len() {
        let segments = usize::from(c.bytes[pos + 26]);
        let lacing = &c.bytes[pos + 27..pos + 27 + segments];
        packets_so_far += lacing.iter().filter(|&&l| l < 255).count();
        let granule = i64::from_le_bytes(c.bytes[pos + 6..pos + 14].try_into().unwrap());
        // Headers: 3 packets with granule 0. Audio: the granule of the last packet on the page.
        let audio_done = packets_so_far.saturating_sub(3);
        let want = if audio_done == 0 { 0 } else { granules[audio_done - 1] as i64 };
        assert_eq!(granule, want, "page at {pos}");
        pos += 27 + segments + lacing.iter().map(|&l| usize::from(l)).sum::<usize>();
    }
}

#[test]
fn multichannel_vorbis_is_converted_with_a_note() {
    let wem = wwise_vorbis_wem(6, 1000, 0, &[0, 0, 1, 0], &mut Rng::new(3));
    let c = convert_wem(&wem).unwrap();
    assert!(c.note.unwrap().contains("6 channels in Wwise's order"));
}

#[test]
fn wwise_opus_becomes_ogg_opus() {
    let (packets, preskip) = (50, 312);
    let samples = 50 * 960 - 312 - 500;
    let c = convert_wem(&wwise_opus_wem(1, packets, samples, preskip, &mut Rng::new(4))).unwrap();
    assert_eq!((c.extension, c.codec.as_str()), ("ogg", "Wwise Opus (WEM)"));
    let a = &scan(&c.bytes, &ScanOptions::default()).audio[0];
    // Length as RFC 7845 counts it: last granule minus pre-skip.
    assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.samples), ("Opus", 1, Some(u64::from(samples))));
    let (packets_out, _) = ogg_packets(&c.bytes);
    assert_eq!(&packets_out[0][..8], b"OpusHead");
    assert_eq!(u16::from_le_bytes([packets_out[0][10], packets_out[0][11]]), preskip);
    assert_eq!(packets_out.len(), 2 + packets);
    // Packets are copied as they are, including those over 255 bytes, never split.
    let wem = wwise_opus_wem(1, packets, samples, preskip, &mut Rng::new(4));
    let data_at = wem.windows(4).position(|w| w == b"data").unwrap() + 8;
    let data_len = u32::from_le_bytes(wem[data_at - 4..data_at].try_into().unwrap()) as usize;
    let concatenated: Vec<u8> = packets_out[2..].concat();
    assert!(wem[data_at..data_at + data_len] == concatenated[..], "the packets are the data chunk, unchanged");
}

#[test]
fn ima_ptadpcm_and_pcm_become_wav() {
    for channels in [1, 2] {
        let c = convert_wem(&wwise_ima_wem(channels, 3, &mut Rng::new(5))).unwrap();
        assert_eq!((c.extension, c.codec.as_str()), ("wav", "Wwise IMA ADPCM"));
        let a = &scan(&c.bytes, &ScanOptions::default()).audio[0];
        assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.samples), ("PCM 16-bit", channels, Some(3 * 64)));
    }
    for channels in [1, 2] {
        // 3 frames hold 192 samples; Wwise says 170, and the WAV stops there.
        let c = convert_wem(&audscan_fixtures::wwise_ptadpcm_wem(channels, 3, 170, &mut Rng::new(10))).unwrap();
        assert_eq!((c.extension, c.codec.as_str()), ("wav", "Wwise PTADPCM"));
        let a = &scan(&c.bytes, &ScanOptions::default()).audio[0];
        assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.samples), ("PCM 16-bit", channels, Some(170)));
    }
    let wem = wwise_pcm_wem(2, 100, &mut Rng::new(6));
    let c = convert_wem(&wem).unwrap();
    let a = &scan(&c.bytes, &ScanOptions::default()).audio[0];
    assert_eq!((c.extension, a.info.codec.as_str(), a.info.samples, a.info.wwise), ("wav", "PCM 16-bit", Some(100), false));
    // The samples are the same bytes.
    assert!(c.bytes.ends_with(&wem[wem.len() - 400..]));
}

#[test]
fn what_cant_be_converted_says_why() {
    let wem = wwise_vorbis_wem(2, 1000, 0, &[0, 1, 0], &mut Rng::new(7));
    assert!(matches!(convert_wem(&wem[..wem.len() / 2]), Err(ConvertError::Partial { .. })));
    // A codebook number past the table (an older Wwise's).
    let wem = wwise_vorbis_wem(2, 1000, 1000, &[0, 1, 0], &mut Rng::new(7));
    assert!(matches!(convert_wem(&wem), Err(ConvertError::Unsupported(r)) if r.contains("codebook 1000")));
    let wav = audscan_fixtures::pcm_wav(1, 8000, 16, 10, &mut Rng::new(8));
    assert!(convert_wem(&wav).is_ok(), "a plain PCM WAV converts to itself, near enough");
    assert!(matches!(convert_wem(b"not audio at all"), Err(ConvertError::NotWem(_))));
}

#[test]
fn extract_converts_wems_and_split_bank_media() {
    let mut rng = Rng::new(9);
    let vorbis = wwise_vorbis_wem(2, 500, 0, &[0, 1, 1, 0], &mut rng);
    let opus = wwise_opus_wem(2, 10, 9000, 312, &mut rng);
    let ima = wwise_ima_wem(1, 2, &mut rng);
    let (bank, _) = audscan_fixtures::bnk(false, 0x88, &[(10, opus), (20, ima)], &mut rng);
    let mut data = rng.bytes(100);
    data.extend(&vorbis);
    data.extend(rng.bytes(37));
    data.extend(&bank);
    data.extend(rng.bytes(50));

    let found = scan(&data, &ScanOptions::default()).audio;
    assert_eq!(found.iter().map(|a| a.container).collect::<Vec<_>>(), [Container::Riff, Container::Bnk]);
    let manifest = Manifest::new(SourceInfo::describe(Path::new("x.bin"), &data), ScanOptions::default(), &found);
    let dir = tempfile::tempdir().unwrap();
    let opts = ExtractOptions { split: true, convert: true, ..Default::default() };
    let files = extract_all(&data, &manifest, dir.path(), &opts).unwrap();
    let names = |f: &audscan_core::ExtractedFile| -> Vec<String> {
        f.converted.iter().map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/")).collect()
    };
    assert_eq!(names(&files[0]), ["00000064.ogg"]);
    let bank_dir = format!("{:08x}", found[1].offset);
    assert_eq!(names(&files[1]), [format!("{bank_dir}/10.ogg"), format!("{bank_dir}/20.wav")]);
    assert!(files.iter().all(|f| f.not_converted.is_empty()));
    // Without --convert, nothing is.
    let plain = extract_all(&data, &manifest, &dir.path().join("plain"), &ExtractOptions { split: true, ..Default::default() }).unwrap();
    assert!(plain.iter().all(|f| f.converted.is_empty()));
}
