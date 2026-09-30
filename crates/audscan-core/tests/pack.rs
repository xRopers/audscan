//! Putting edited sounds back: whole files and tracks of banks and packages, fitted into
//! their place or growing at the end of the input, then scanned again.

use std::collections::BTreeMap;
use std::path::Path;

use audscan_core::{
    AudioEdit, AudioEntry, Container, Edits, Error, ExtractOptions, Manifest, Outcome, PackOptions, PackResult, Placement,
    ScanOptions, SourceInfo, audio_bytes, extract_all, load_edits, pack, scan, track_bytes,
};
use audscan_fixtures::{self as fx, FsbTrack, PckFile, Rng};

fn manifest_of(data: &[u8]) -> Manifest {
    let opts = ScanOptions::default();
    let found = scan(data, &opts).audio;
    Manifest::new(SourceInfo::describe(Path::new("input.bin"), data), opts, &found)
}

fn find(m: &Manifest, format: Container) -> &AudioEntry {
    m.audio.iter().find(|a| a.format == format).unwrap_or_else(|| panic!("no {format} found"))
}

/// Pack, build the output in memory and verify it.
fn packed(data: &[u8], m: &Manifest, edits: Edits) -> (Vec<u8>, PackResult) {
    let result = pack(data, m, &edits, &PackOptions::default()).unwrap();
    let out = result.output(data);
    result.verify(&out).unwrap();
    (out, result)
}

fn one(id: u32, edit: AudioEdit) -> Edits {
    BTreeMap::from([(id, edit)])
}

fn tracks(edits: &[(usize, Vec<u8>)]) -> AudioEdit {
    AudioEdit::Tracks(edits.iter().cloned().collect())
}

/// Every track of an entry, as extract --split writes them.
fn split(data: &[u8], entry: &AudioEntry) -> Vec<Vec<u8>> {
    let bytes = audio_bytes(data, entry).unwrap();
    (0..entry.tracks.len()).map(|i| track_bytes(entry, bytes, i).unwrap().into_owned()).collect()
}

fn u32_le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// A little-endian bank of three WEMs whose HIRC records media 102's size after its ID,
/// as a sound object does.
fn bank(rng: &mut Rng) -> Vec<u8> {
    let media = vec![(101, fx::pcm_wem(1, 48000, 400, rng)), (102, fx::pcm_wem(1, 48000, 300, rng)), (103, fx::vorbis_wem(false, 2, 48000, 9000, 777, rng))];
    let (mut bank, _) = fx::bnk(false, 0x88, &media, rng);
    let hirc = bank.windows(4).position(|w| w == b"HIRC").unwrap() + 8;
    bank[hirc + 4..hirc + 8].copy_from_slice(&102u32.to_le_bytes());
    bank[hirc + 8..hirc + 12].copy_from_slice(&(media[1].1.len() as u32).to_le_bytes());
    bank
}

#[test]
fn bank_media_in_the_middle_of_a_file_are_fitted_into_its_place() {
    let mut rng = Rng::new(1);
    let bank = bank(&mut rng);
    let mut data = rng.bytes(100);
    data.extend_from_slice(&bank);
    data.extend(rng.bytes(64));
    let m = manifest_of(&data);
    let entry = find(&m, Container::Bnk).clone();
    let before = split(&data, &entry);

    // Smaller media: the bank shrinks, then DATA is padded back to the bank's size.
    let new = fx::pcm_wem(1, 48000, 100, &mut rng);
    let (out, result) = packed(&data, &m, one(entry.id, tracks(&[(1, new.clone())])));
    assert_eq!(out.len(), data.len());
    let plan = &result.audio[0];
    assert_eq!(plan.outcome, Outcome::TracksReplaced(vec![1]));
    assert!(matches!(plan.placement, Placement::Padded { inside: true, .. }), "{:?}", plan.placement);
    assert!(plan.notes.iter().any(|n| n.contains("1 sound object(s) in HIRC")), "{:?}", plan.notes);
    let after = manifest_of(&out);
    let now = find(&after, Container::Bnk);
    assert_eq!((now.offset, now.size), (entry.offset, entry.size));
    let tracks_now = split(&out, now);
    assert_eq!(tracks_now, [before[0].clone(), new.clone(), before[2].clone()]);
    // HIRC now holds the new size after the ID.
    let mut pattern = 102u32.to_le_bytes().to_vec();
    pattern.extend_from_slice(&(new.len() as u32).to_le_bytes());
    assert!(out.windows(8).any(|w| w == pattern));
    // Wwise keeps media 16-byte aligned in DATA.
    let data_start = out.windows(4).position(|w| w == b"DATA").unwrap() + 8;
    assert!(now.tracks.iter().all(|t| (now.offset + t.offset - data_start as u64).is_multiple_of(16)));
    // Everything outside the bank is untouched.
    assert_eq!(out[..100], data[..100]);
    assert_eq!(out[out.len() - 64..], data[data.len() - 64..]);

    // Bigger media don't fit in the middle of a file.
    let big = fx::pcm_wem(1, 48000, 5000, &mut rng);
    let err = pack(&data, &m, &one(entry.id, tracks(&[(1, big)])), &PackOptions::default()).unwrap_err();
    assert!(matches!(&err, Error::Edit { reason, .. } if reason.contains("bytes too big")), "{err}");
}

#[test]
fn a_bank_that_ends_the_input_grows_and_media_must_be_wems() {
    let mut rng = Rng::new(2);
    let data = bank(&mut rng);
    let m = manifest_of(&data);
    let entry = find(&m, Container::Bnk).clone();
    let before = split(&data, &entry);
    let new = fx::pcm_wem(2, 44100, 2000, &mut rng);
    let (out, result) = packed(&data, &m, one(entry.id, tracks(&[(0, new.clone())])));
    assert_eq!(result.audio[0].placement, Placement::Resized);
    assert!(result.fields.is_empty(), "a file of its own has no size fields outside it");
    assert_eq!(out.len() as u64, data.len() as u64 + new.len() as u64 - before[0].len() as u64);
    let now = manifest_of(&out);
    assert_eq!(split(&out, find(&now, Container::Bnk)), [new, before[1].clone(), before[2].clone()]);

    let fail = |edit: AudioEdit| pack(&data, &m, &one(entry.id, edit), &PackOptions::default()).unwrap_err().to_string();
    assert!(fail(tracks(&[(0, fx::pcm_wav(1, 48000, 16, 10, &mut rng))])).contains("plain WAV"));
    assert!(fail(tracks(&[(0, b"not audio".to_vec())])).contains("isn't a WEM"));
    assert!(fail(tracks(&[(7, before[0].clone())])).contains("no track 7"));
    let mut cut = before[0].clone();
    cut.push(0);
    assert!(fail(tracks(&[(0, cut)])).contains("its RIFF header says"));
    // The same bytes change nothing.
    let (out, result) = packed(&data, &m, one(entry.id, tracks(&[(2, before[2].clone())])));
    assert_eq!((out, result.changed()), (data.clone(), 0));
}

#[test]
fn prefetch_media_are_refused() {
    let mut rng = Rng::new(3);
    let wem = fx::pcm_wem(1, 48000, 1000, &mut rng);
    let (bank, _) = fx::bnk(false, 0x88, &[(5, wem[..300].to_vec())], &mut rng);
    let m = manifest_of(&bank);
    let entry = find(&m, Container::Bnk);
    let err = pack(&bank, &m, &one(entry.id, tracks(&[(0, wem)])), &PackOptions::default()).unwrap_err();
    assert!(err.to_string().contains("prefetch media"), "{err}");
}

#[test]
fn package_files_move_and_keep_their_block_alignment() {
    let mut rng = Rng::new(4);
    let (bnk, _) = fx::bnk(false, 0x88, &[(9, fx::pcm_wem(1, 48000, 50, &mut rng))], &mut rng);
    let banks = [PckFile { id: 1, language: 0, bytes: bnk }];
    let streams = [
        PckFile { id: 20, language: 0, bytes: fx::pcm_wem(1, 48000, 300, &mut rng) },
        PckFile { id: 21, language: 1, bytes: fx::vorbis_wem(false, 1, 48000, 100, 333, &mut rng) },
        PckFile { id: 22, language: 1, bytes: fx::pcm_wem(2, 48000, 100, &mut rng) },
    ];
    let (data, _) = fx::pck(&[(0, "sfx"), (1, "english(us)")], &banks, &streams, None, 16);
    let m = manifest_of(&data);
    let entry = find(&m, Container::Pck).clone();
    let before = split(&data, &entry);

    let wem = fx::pcm_wem(1, 48000, 999, &mut rng);
    let (new_bank, _) = fx::bnk(false, 0x88, &[(9, fx::pcm_wem(1, 48000, 70, &mut rng))], &mut rng);
    let (out, result) = packed(&data, &m, one(entry.id, tracks(&[(0, new_bank.clone()), (1, wem.clone())])));
    assert_eq!(result.audio[0].placement, Placement::Resized);
    let now = manifest_of(&out);
    let pck = find(&now, Container::Pck);
    assert_eq!(split(&out, pck), [new_bank, wem, before[2].clone(), before[3].clone()]);
    assert!(pck.tracks.iter().all(|t| t.offset % 16 == 0));
    let languages: Vec<_> = pck.tracks.iter().map(|t| t.language.as_deref()).collect();
    assert_eq!(languages, [Some("sfx"), Some("sfx"), Some("english(us)"), Some("english(us)")]);

    let err = pack(&data, &m, &one(entry.id, tracks(&[(0, before[1].clone())])), &PackOptions::default()).unwrap_err();
    assert!(err.to_string().contains("isn't a SoundBank"), "{err}");
}

/// A bank of three 16-bit PCM tracks, 32-byte aligned (an even length in all).
fn pcm_fsb(rng: &mut Rng) -> Vec<u8> {
    let tracks = [
        FsbTrack { name: Some("a"), channels: 2, rate: 44100, samples: 100, data_len: 400 },
        FsbTrack { name: Some("b"), channels: 1, rate: 22050, samples: 50, data_len: 100 },
        FsbTrack { name: Some("c"), channels: 1, rate: 48000, samples: 30, data_len: 60 },
    ];
    fx::fsb5(1, 2, &tracks, rng)
}

/// The samples of a WAV whose data chunk comes last.
fn wav_samples(wav: &[u8], frames: usize, frame: usize) -> &[u8] {
    &wav[wav.len() - frames * frame..]
}

#[test]
fn fsb5_pcm_tracks_take_wavs() {
    let mut rng = Rng::new(5);
    let fsb = pcm_fsb(&mut rng);
    let mut data = rng.bytes(200);
    data.extend_from_slice(&fsb);
    let m = manifest_of(&data);
    let entry = find(&m, Container::Fsb5).clone();
    let before = split(&data, &entry);

    // A plain WAV at a rate the header holds, and one needing extra chunks (12345 Hz,
    // 3 channels).
    let wav = fx::pcm_wav(1, 32000, 16, 700, &mut rng);
    let odd = fx::riff(false, b"WAVE", &[(b"fmt ", fx::fmt(false, 1, 3, 12345, 6, 16, &[])), (b"data", rng.bytes(6 * 11))]);
    let (out, result) = packed(&data, &m, one(entry.id, tracks(&[(1, wav.clone()), (2, odd.clone())])));
    assert_eq!(result.audio[0].placement, Placement::Resized);
    let now = manifest_of(&out);
    let bank = find(&now, Container::Fsb5);
    let t = &bank.tracks;
    assert_eq!((t[1].channels, t[1].sample_rate, t[1].samples, t[1].name.as_deref()), (1, 32000, Some(700), Some("b")));
    assert_eq!((t[2].channels, t[2].sample_rate, t[2].samples, t[2].name.as_deref()), (3, 12345, Some(11), Some("c")));
    let after = split(&out, bank);
    assert_eq!(after[0], before[0]);
    assert!(after[1].ends_with(wav_samples(&wav, 700, 2)));
    assert!(after[2].ends_with(wav_samples(&odd, 11, 6)));
    assert!(t.iter().all(|x| (x.offset - t[0].offset).is_multiple_of(32)), "tracks stay 32-byte aligned");

    // Split out again, the new track is the same WAV audscan would write.
    let (again, _) = packed(&out, &now, one(bank.id, tracks(&[(1, after[1].clone())])));
    assert_eq!(again, out, "putting back an unedited split track changes nothing");

    let fail = |file: Vec<u8>| pack(&data, &m, &one(entry.id, tracks(&[(0, file)])), &PackOptions::default()).unwrap_err().to_string();
    assert!(fail(fx::pcm_wav(1, 32000, 8, 10, &mut rng)).contains("holds PCM 16-bit"));
    assert!(fail(fx::pcm_wem(1, 48000, 10, &mut rng)).contains("(a WEM)"));
}

#[test]
fn fsb5_tracks_of_other_codecs_take_one_track_banks() {
    let mut rng = Rng::new(6);
    let tracks_in = [
        FsbTrack { name: Some("music"), channels: 2, rate: 44100, samples: 441_000, data_len: 700 },
        FsbTrack { name: Some("voice"), channels: 1, rate: 24000, samples: 36_000, data_len: 300 },
        FsbTrack { name: Some("amb"), channels: 3, rate: 12345, samples: 12_345, data_len: 250 },
    ];
    let data = fx::fsb5(1, 15, &tracks_in, &mut rng);
    let m = manifest_of(&data);
    let entry = find(&m, Container::Fsb5).clone();
    let before = split(&data, &entry);

    // Track 0, split out as a one-track bank, goes in as track 2.
    let (out, _) = packed(&data, &m, one(entry.id, tracks(&[(2, before[0].clone())])));
    let now = manifest_of(&out);
    let bank = find(&now, Container::Fsb5);
    let (t0, t2) = (&bank.tracks[0], &bank.tracks[2]);
    assert_eq!((t2.channels, t2.sample_rate, t2.samples), (t0.channels, t0.sample_rate, t0.samples));
    assert_eq!(t2.name.as_deref(), Some("amb"), "the track keeps its name in the bank");
    let after = split(&out, bank);
    assert_eq!(after[1], before[1]);
    // The same sound as track 0, under track 2's name.
    let data_of = |file: &[u8]| file[file.len() - 700..].to_vec();
    assert_eq!(data_of(&after[2]), data_of(&before[0]));

    let fail = |file: Vec<u8>| pack(&data, &m, &one(entry.id, tracks(&[(1, file)])), &PackOptions::default()).unwrap_err().to_string();
    assert!(fail(fx::pcm_wav(1, 32000, 16, 10, &mut rng)).contains("a WAV can't go in"));
    let pcm_bank = fx::fsb5(1, 2, &[FsbTrack { name: None, channels: 1, rate: 8000, samples: 4, data_len: 8 }], &mut rng);
    assert!(fail(pcm_bank).contains("the replacement is PCM 16-bit, but the bank is Vorbis"));
    assert!(fail(data.clone()).contains("has 3 tracks"));
}

#[test]
fn an_fmod_bank_s_size_fields_follow_its_fsb5() {
    let mut rng = Rng::new(7);
    let fsb = pcm_fsb(&mut rng);
    assert_eq!(fsb.len() % 2, 0);
    // As FMOD Studio writes it: an SNDH chunk giving the FSB5's offset and size, far
    // enough ahead that only the index entry gives it away, then the SND chunk holding it.
    let at = 12 + 20 + 8 + 100 + 8;
    let sndh: Vec<u8> = [0x0008_0003u32, at, fsb.len() as u32].iter().flat_map(|v| v.to_le_bytes()).chain([0; 88]).collect();
    let data = fx::riff(false, b"FEV ", &[(b"FMT ", vec![0; 12]), (b"SNDH", sndh), (b"SND ", fsb.clone())]);
    let m = manifest_of(&data);
    let entry = find(&m, Container::Fsb5).clone();
    assert_eq!((entry.offset, entry.end()), (u64::from(at), data.len() as u64));
    let wav = fx::pcm_wav(2, 44100, 16, 2000, &mut rng);
    let (out, result) = packed(&data, &m, one(entry.id, tracks(&[(0, wav)])));
    let measures: Vec<_> = result.fields.iter().map(|f| (f.offset, f.measures.as_str())).collect();
    let index = format!("audio {} (after its offset, as in an index)", entry.id);
    assert_eq!(measures, [(4, "the rest of the input"), (48, index.as_str()), (entry.offset - 4, "the rest of the input")]);
    assert_eq!(u32_le(&out, 4) as usize, out.len() - 8);
    let now = manifest_of(&out);
    let bank = find(&now, Container::Fsb5);
    assert_eq!(u32_le(&out, entry.offset as usize - 4) as u64, bank.size);
    assert_eq!(u32_le(&out, 48) as u64, bank.size);
    assert_eq!(bank.end(), out.len() as u64);
}

#[test]
fn whole_files_are_replaced_and_padded_to_fit() {
    let mut rng = Rng::new(8);
    let wav = fx::pcm_wav(2, 44100, 16, 500, &mut rng);
    let wem = fx::pcm_wem(1, 48000, 200, &mut rng);
    let ogg = fx::ogg_stream(0x77, &fx::vorbis_head(2, 44100), &[(600, 44100), (300, 60000)], true, &mut rng);
    let mut data = Vec::new();
    for file in [&wav, &wem, &ogg] {
        data.extend(rng.bytes(50));
        data.extend_from_slice(file);
    }
    data.extend(rng.bytes(50));
    let m = manifest_of(&data);
    let [w, e, o] = [0, 1, 2].map(|i| m.audio[i].clone());
    assert_eq!((w.format, e.wwise, o.format), (Container::Riff, true, Container::Ogg));

    // A shorter WAV (by an even amount) gets a JUNK chunk, so it keeps its size.
    let short = fx::pcm_wav(2, 44100, 16, 300, &mut rng);
    let smaller_ogg = fx::ogg_stream(0x77, &fx::vorbis_head(2, 44100), &[(100, 44100)], true, &mut rng);
    let edits = BTreeMap::from([(w.id, AudioEdit::File(short.clone())), (o.id, AudioEdit::File(smaller_ogg.clone()))]);
    let (out, result) = packed(&data, &m, edits);
    assert_eq!(out.len(), data.len());
    assert_eq!(result.audio[0].placement, Placement::Padded { by: w.size - short.len() as u64, inside: true });
    assert_eq!(result.audio[1].placement, Placement::Padded { by: o.size - smaller_ogg.len() as u64, inside: false });
    assert!(result.audio[1].notes.iter().any(|n| n.contains("zero bytes after it")));
    let now = manifest_of(&out);
    let n = &now.audio[0];
    assert_eq!((n.offset, n.size, n.samples), (w.offset, w.size, Some(300)));
    assert_eq!(out[w.offset as usize..][..short.len() - 8][8..], short[8..short.len() - 8], "the new WAV, then JUNK");
    assert_eq!(&out[o.offset as usize..][..smaller_ogg.len()], smaller_ogg.as_slice());
    assert_eq!(now.audio[2].size, smaller_ogg.len() as u64);

    let fail = |id: u32, file: Vec<u8>| pack(&data, &m, &one(id, AudioEdit::File(file)), &PackOptions::default()).unwrap_err().to_string();
    assert!(fail(e.id, short.clone()).contains("plain WAV"));
    assert!(fail(w.id, wem.clone()).contains("is a Wwise WEM"));
    assert!(fail(w.id, ogg.clone()).contains("isn't a wav file"));
    assert!(fail(o.id, fx::ogg_stream(0x77, &fx::vorbis_head(2, 44100), &[(900, 1), (900, 2)], true, &mut rng)).contains("bytes too big"));

    // A changed input is refused unless told otherwise.
    let mut changed = data.clone();
    changed[0] ^= 1;
    let err = pack(&changed, &m, &one(w.id, AudioEdit::File(short.clone())), &PackOptions::default()).unwrap_err();
    assert!(matches!(err, Error::SourceMismatch(_)));
    assert!(pack(&changed, &m, &one(w.id, AudioEdit::File(short)), &PackOptions { verify_source: false }).is_ok());
}

#[test]
fn edits_are_found_in_an_extract_folder_and_written_verified() {
    let mut rng = Rng::new(9);
    let bank_bytes = bank(&mut rng);
    let mut data = rng.bytes(64);
    let wav = fx::pcm_wav(1, 22050, 16, 100, &mut rng);
    data.extend_from_slice(&wav);
    data.extend(rng.bytes(64));
    data.extend_from_slice(&bank_bytes);
    let m = manifest_of(&data);
    let (w, b) = (find(&m, Container::Riff).clone(), find(&m, Container::Bnk).clone());
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("out");
    extract_all(&data, &m, &out_dir, &ExtractOptions { split: true, ..ExtractOptions::default() }).unwrap();

    let found = load_edits(&data, &m, &out_dir).unwrap();
    assert!(found.edits.is_empty());
    assert_eq!(found.unchanged, 1 + 1 + 3, "the WAV, the bank and its three WEMs");

    let new_wav = fx::pcm_wav(1, 22050, 16, 60, &mut rng);
    let new_wem = fx::pcm_wem(1, 48000, 900, &mut rng);
    std::fs::write(out_dir.join(&w.file), &new_wav).unwrap();
    let stem = b.file.trim_end_matches(".bnk");
    std::fs::write(out_dir.join(stem).join("103.wem"), &new_wem).unwrap();
    let found = load_edits(&data, &m, &out_dir).unwrap();
    assert_eq!(found.edits[&w.id], AudioEdit::File(new_wav.clone()));
    assert_eq!(found.edits[&b.id], tracks(&[(2, new_wem.clone())]));
    assert_eq!(found.unchanged, 1 + 2);

    let result = pack(&data, &m, &found.edits, &PackOptions::default()).unwrap();
    let output = dir.path().join("packed.bin");
    result.write_file(&data, &output).unwrap();
    let out = std::fs::read(&output).unwrap();
    assert_eq!(out, result.output(&data));
    let now = manifest_of(&out);
    assert_eq!(split(&out, find(&now, Container::Bnk))[2], new_wem);
    assert_eq!(find(&now, Container::Riff).samples, Some(60));

    // A bank and one of its tracks both edited is ambiguous.
    std::fs::write(out_dir.join(&b.file), &bank_bytes[..bank_bytes.len() - 1]).unwrap();
    let err = load_edits(&data, &m, &out_dir).unwrap_err();
    assert!(err.to_string().contains("keep one kind of edit"), "{err}");
}
