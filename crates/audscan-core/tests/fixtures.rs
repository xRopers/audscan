//! Scan and extract every fixture; audio must be reported at exactly the known offsets,
//! with the sizes and details the fixture builder worked out independently.

use std::fs;
use std::path::Path;

use audscan_core::{Container, Error, ExtractOptions, Manifest, ScanOptions, SourceInfo, audio_at, extract_all, scan};
use audscan_fixtures::Fixture;

/// Name (or ID), language, codec, extension, channels, rate, samples, noted, and where it
/// is (checked for Wwise files only).
type TrackRow = (String, Option<String>, Option<String>, Option<String>, u16, u32, Option<u64>, bool, Option<(u64, u64)>);
type Row = (u64, String, String, u64, String, u16, u32, Option<u64>, Vec<TrackRow>, bool, u32);

fn expected_rows(f: &Fixture) -> Vec<Row> {
    f.expected
        .iter()
        .map(|e| {
            let tracks = e
                .tracks
                .iter()
                .map(|t| {
                    let text = |s: Option<&str>| s.map(String::from);
                    let range = t.range.map(|(offset, size)| (offset as u64, size as u64));
                    let (lang, codec, ext) = (text(t.language), text(t.codec), text(t.extension));
                    (t.name.clone(), lang, codec, ext, t.channels, t.sample_rate, t.samples, t.noted, range)
                })
                .collect();
            let (container, label, codec) = (e.container.to_string(), e.label.to_string(), e.codec.to_string());
            (e.offset as u64, container, label, e.size as u64, codec, e.channels, e.sample_rate, e.samples, tracks, e.noted, e.crc32)
        })
        .collect()
}

fn found_rows(data: &[u8]) -> Vec<Row> {
    scan(data, &ScanOptions::default())
        .audio
        .iter()
        .map(|a| {
            let i = &a.info;
            let tracks = i
                .tracks
                .iter()
                .map(|t| {
                    let range = t.id.is_some().then_some((t.offset, t.size));
                    let (lang, codec, ext) = (t.language.clone(), t.codec.clone(), t.extension.clone());
                    (t.display_name(), lang, codec, ext, t.channels, t.sample_rate, t.samples, t.note.is_some(), range)
                })
                .collect();
            let (container, codec) = (a.container.to_string(), i.codec.clone());
            (a.offset, container, a.label(), i.size, codec, i.channels, i.sample_rate, i.samples, tracks, i.note.is_some(), a.crc32)
        })
        .collect()
}

#[test]
fn every_fixture_scans_exactly() {
    for f in audscan_fixtures::all() {
        let (found, expected) = (found_rows(&f.data), expected_rows(&f));
        for (a, b) in found.iter().zip(&expected) {
            assert_eq!(a, b, "fixture {}", f.name);
        }
        assert_eq!(found.len(), expected.len(), "fixture {}", f.name);
    }
}

#[test]
fn unusable_headers_are_reported_with_a_reason() {
    for f in audscan_fixtures::all() {
        let report = scan(&f.data, &ScanOptions::default());
        let found: Vec<_> = report.rejected.iter().map(|r| r.offset).collect();
        let expected: Vec<_> = f.rejected.iter().map(|r| r.offset as u64).collect();
        assert_eq!(found, expected, "fixture {}: {:?}", f.name, report.rejected);
        for (r, e) in report.rejected.iter().zip(&f.rejected) {
            assert!(r.reason.starts_with(e.reason_prefix), "fixture {}: {:?}", f.name, r.reason);
        }
    }
}

#[test]
fn fsb5_tracks_split_the_sample_data() {
    let f = audscan_fixtures::audio_archive();
    let fsb = scan(&f.data, &ScanOptions::default()).audio.into_iter().find(|a| a.info.tracks.len() == 3).unwrap();
    let t = &fsb.info.tracks;
    // Track data starts 32-byte aligned and runs to the next track, the last to the end.
    assert!(t.iter().all(|x| (x.offset - t[0].offset) % 32 == 0));
    assert_eq!(t[0].offset + t[0].size, t[1].offset);
    assert_eq!(t[1].offset + t[1].size, t[2].offset);
    assert_eq!(t[2].offset + t[2].size, fsb.info.size);
    assert_eq!((t[0].size, t[2].size), (704, 250));
    let total = 441_000.0 / 44100.0 + 36_000.0 / 24000.0 + 1.0;
    assert!((fsb.info.duration().unwrap() - total).abs() < 1e-9);
}

#[test]
fn only_the_chosen_formats_are_found() {
    let f = audscan_fixtures::audio_archive();
    let report = scan(&f.data, &ScanOptions { formats: vec![Container::Ogg] });
    assert!(report.audio.iter().all(|a| a.container == Container::Ogg));
    // Without RIFF, the Ogg stream inside a WAV's data is found too.
    let expected_ogg = f.expected.iter().filter(|e| e.container == "ogg").count();
    assert_eq!(report.audio.len(), expected_ogg + 1);
}

#[test]
fn audio_at_an_exact_offset() {
    let f = audscan_fixtures::audio_archive();
    let e = &f.expected[1];
    let a = audio_at(&f.data, e.offset as u64, Container::Riff).unwrap();
    assert_eq!((a.info.size, a.crc32, a.extension()), (e.size as u64, e.crc32, "wem"));
    assert!(audio_at(&f.data, e.offset as u64 + 1, Container::Riff).is_err());
    assert!(audio_at(&f.data, u64::MAX, Container::Riff).is_err());
}

#[test]
fn extract_writes_each_file_verbatim() {
    let f = audscan_fixtures::audio_archive();
    let found = scan(&f.data, &ScanOptions::default()).audio;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let manifest = Manifest::from_json(&manifest.to_json()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let files = extract_all(&f.data, &manifest, dir.path(), &ExtractOptions::default()).unwrap();
    assert_eq!(files.len(), f.expected.len());
    for (file, e) in files.iter().zip(&f.expected) {
        let bytes = fs::read(&file.path).unwrap();
        assert_eq!(bytes, &f.data[e.offset..e.offset + e.size]);
        // An extracted file is valid on its own.
        let alone = scan(&bytes, &ScanOptions::default()).audio;
        assert_eq!((alone.len(), alone[0].offset, alone[0].info.size), (1, 0, e.size as u64));
    }
    let names: Vec<_> = files.iter().map(|f| f.path.extension().unwrap().to_str().unwrap().to_string()).collect();
    assert_eq!(names[..4], ["wav", "wem", "wem", "wem"]);
    assert!(names.contains(&"fsb".to_string()) && names.contains(&"ogg".to_string()));
}

#[test]
fn extract_refuses_a_different_input() {
    let f = audscan_fixtures::wav_file();
    let found = scan(&f.data, &ScanOptions::default()).audio;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let mut changed = f.data.clone();
    changed[500] ^= 1;
    let dir = tempfile::tempdir().unwrap();
    let err = extract_all(&changed, &manifest, dir.path(), &ExtractOptions::default()).unwrap_err();
    assert!(matches!(err, Error::SourceMismatch(_)), "{err}");
    // Forcing past the file check still catches the changed audio.
    let err = extract_all(&changed, &manifest, dir.path(), &ExtractOptions { verify_source: false, ..Default::default() }).unwrap_err();
    assert!(matches!(err, Error::Audio { id: 0, .. }), "{err}");
}

#[test]
fn split_writes_each_wwise_file_by_id_and_language() {
    let f = audscan_fixtures::audio_archive();
    let found = scan(&f.data, &ScanOptions::default()).audio;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let dir = tempfile::tempdir().unwrap();
    let files = extract_all(&f.data, &manifest, dir.path(), &ExtractOptions { split: true, ..Default::default() }).unwrap();

    let i = f.expected.iter().position(|e| e.container == "pck").unwrap();
    let (e, file) = (&f.expected[i], &files[i]);
    let folder = dir.path().join(format!("{:08x}", e.offset));
    let names: Vec<_> = file.split.iter().map(|p| p.strip_prefix(&folder).unwrap().to_string_lossy().replace('\\', "/")).collect();
    // Same ID, different languages: the localized one goes in its language's folder.
    assert_eq!(names, ["777.bnk", "100.wem", "english(us)/100.wem", "4294967297.wem"]);
    for (path, t) in file.split.iter().zip(&e.tracks) {
        let (offset, size) = t.range.unwrap();
        assert_eq!(fs::read(path).unwrap(), &f.data[e.offset + offset..][..size]);
    }
    // A SoundBank split out of a package is a bank of its own.
    let bank = fs::read(&file.split[0]).unwrap();
    let alone = scan(&bank, &ScanOptions::default()).audio;
    assert_eq!((alone.len(), alone[0].container, alone[0].info.size), (1, Container::Bnk, bank.len() as u64));

    // Prefetch media come out as they are in the bank: the start of the WEM.
    let i = f.expected.iter().position(|e| e.container == "bnk" && e.codec == "mixed").unwrap();
    assert_eq!(files[i].split.len(), 3);
    assert_eq!(fs::read(&files[i].split[2]).unwrap().len(), 600);
    // Without split, nothing is.
    let plain = extract_all(&f.data, &manifest, &dir.path().join("plain"), &ExtractOptions::default()).unwrap();
    assert!(plain.iter().all(|f| f.split.is_empty()));
}

fn split_fsb5(n: usize) -> (audscan_fixtures::Expected, Vec<(String, Vec<u8>)>) {
    split_bank("fsb5", n)
}

/// The split files of the fixture's `n`th bank of a format, by name relative to its folder.
fn split_bank(container: &str, n: usize) -> (audscan_fixtures::Expected, Vec<(String, Vec<u8>)>) {
    let f = audscan_fixtures::audio_archive();
    let found = scan(&f.data, &ScanOptions::default()).audio;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let dir = tempfile::tempdir().unwrap();
    let files = extract_all(&f.data, &manifest, dir.path(), &ExtractOptions { split: true, ..Default::default() }).unwrap();
    let i = f.expected.iter().enumerate().filter(|(_, e)| e.container == container).nth(n).unwrap().0;
    let folder = dir.path().join(format!("{:08x}", f.expected[i].offset));
    let split = files[i]
        .split
        .iter()
        .map(|p| (p.strip_prefix(&folder).unwrap().to_string_lossy().into_owned(), fs::read(p).unwrap()))
        .collect();
    (f.expected[i].clone(), split)
}

#[test]
fn fsb5_tracks_split_into_one_track_banks() {
    let (e, split) = split_fsb5(0);
    let names: Vec<_> = split.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["music_intro.fsb", "vo_line_01.fsb", "amb_odd.fsb"]);
    for ((_, bytes), t) in split.iter().zip(&e.tracks) {
        // Each is a bank of one track, with that track's name, channels, rate and length.
        let alone = scan(bytes, &ScanOptions::default()).audio;
        assert_eq!(alone.len(), 1);
        let a = &alone[0];
        assert_eq!((a.container, a.info.size, a.info.codec.as_str()), (Container::Fsb5, bytes.len() as u64, "Vorbis"));
        let only = &a.info.tracks[..];
        assert_eq!(only.len(), 1);
        assert_eq!((only[0].display_name(), only[0].channels, only[0].sample_rate, only[0].samples), (t.name.clone(), t.channels, t.sample_rate, t.samples));
    }
    // The track data is copied as it was: the same bytes at the end of each file.
    let f = audscan_fixtures::audio_archive();
    let bank = scan(&f.data, &ScanOptions::default()).audio.into_iter().find(|a| a.offset == e.offset as u64).unwrap();
    for ((_, bytes), t) in split.iter().zip(&bank.info.tracks) {
        let data = &f.data[(bank.offset + t.offset) as usize..][..t.size as usize];
        assert!(bytes.ends_with(data));
    }
}

#[test]
fn pcm_fsb5_tracks_split_into_wavs() {
    // Version 0, PCM16, unnamed: a WAV named by its index.
    let (_, split) = split_fsb5(1);
    assert_eq!(split.len(), 1);
    let (name, wav) = &split[0];
    assert_eq!(name, "track0.wav");
    let a = &scan(wav, &ScanOptions::default()).audio[0];
    assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.sample_rate, a.info.samples), ("PCM 16-bit", 1, 22050, Some(500)));

    // PCM8: names made safe, duplicates told apart, 8-bit samples made unsigned.
    let (e, split) = split_fsb5(2);
    let names: Vec<_> = split.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["ui_click.wav", "ui_click_1.wav", "track2.wav"]);
    let f = audscan_fixtures::audio_archive();
    let bank = scan(&f.data, &ScanOptions::default()).audio.into_iter().find(|a| a.offset == e.offset as u64).unwrap();
    for ((_, wav), t) in split.iter().zip(&bank.info.tracks) {
        let a = &scan(wav, &ScanOptions::default()).audio[0];
        assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.sample_rate, a.info.samples), ("PCM 8-bit", t.channels, t.sample_rate, t.samples));
        let frames = (t.samples.unwrap() * u64::from(t.channels)) as usize;
        let signed = &f.data[(bank.offset + t.offset) as usize..][..frames];
        let unsigned: Vec<u8> = signed.iter().map(|b| b ^ 0x80).collect();
        let data_at = wav.len() - frames - frames % 2;
        assert_eq!(&wav[data_at..data_at + frames], &unsigned[..]);
    }
}

/// The fixture's `n`th FSB4 bank as found, with its bytes.
fn found_fsb4(n: usize) -> (audscan_core::FoundAudio, Vec<u8>) {
    let f = audscan_fixtures::audio_archive();
    let bank = scan(&f.data, &ScanOptions::default()).audio.into_iter().filter(|a| a.container == Container::Fsb4).nth(n).unwrap();
    let bytes = f.data[bank.offset as usize..bank.end() as usize].to_vec();
    (bank, bytes)
}

#[test]
fn fsb4_data_alignment_is_worked_out() {
    // 32-byte aligned: 1000 bytes take 1024, 77 take 96.
    let (bank, _) = found_fsb4(0);
    let t = &bank.info.tracks;
    let first = t[0].offset;
    assert_eq!((t[1].offset - first, t[2].offset - first), (1024, 1024 + 96));
    assert_eq!(t.iter().map(|t| t.size).collect::<Vec<_>>(), [1000, 77, 417]);
    // 16-byte aligned, the last track unpadded.
    let (bank, _) = found_fsb4(1);
    let t = &bank.info.tracks;
    assert_eq!((t[1].offset - t[0].offset, t[2].offset - t[0].offset), (512, 512 + 336));
    assert_eq!(t[2].offset + t[2].size, bank.info.size);
}

#[test]
fn fsb4_tracks_split_into_wavs_and_one_track_banks() {
    let (_, split) = split_bank("fsb4", 0);
    let names: Vec<_> = split.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["menu_theme.wav", "blip.wav", "voice_01.fsb"]);
    let (bank, bytes) = found_fsb4(0);
    let data = |i: usize| {
        let t = &bank.info.tracks[i];
        &bytes[t.offset as usize..(t.offset + t.size) as usize]
    };
    // 16-bit little-endian PCM is copied; signed 8-bit is made unsigned (and padded to even).
    assert!(split[0].1.ends_with(data(0)));
    let unsigned: Vec<u8> = data(1).iter().map(|b| b ^ 0x80).chain([0]).collect();
    assert!(split[1].1.ends_with(&unsigned));
    for (i, codec) in [(0, "PCM 16-bit"), (1, "PCM 8-bit")] {
        let a = &scan(&split[i].1, &ScanOptions::default()).audio[0];
        let t = &bank.info.tracks[i];
        assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.sample_rate, a.info.samples), (codec, t.channels, t.sample_rate, t.samples));
    }
    // MPEG stays in a bank of one, its data as it was.
    let a = &scan(&split[2].1, &ScanOptions::default()).audio[0];
    assert_eq!((a.container, a.info.codec.as_str(), a.info.tracks[0].name.as_deref(), a.info.samples), (Container::Fsb4, "MPEG", Some("voice_01"), Some(4608)));
    assert!(split[2].1.ends_with(data(2)));

    // Basic headers: each split bank gets a full header of its own.
    let (_, split) = split_bank("fsb4", 1);
    let names: Vec<_> = split.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["sfx_bank.fsb", "track1.fsb", "track2.fsb"]);
    let (bank, bytes) = found_fsb4(1);
    for ((_, file), t) in split.iter().zip(&bank.info.tracks) {
        let a = &scan(file, &ScanOptions::default()).audio[0];
        assert_eq!((a.info.codec.as_str(), a.info.channels, a.info.sample_rate, a.info.samples), ("IMA ADPCM", 1, 22050, t.samples));
        assert_eq!(u32::from_le_bytes(file[0x14..0x18].try_into().unwrap()) & 2, 0, "not basic headers any more");
        assert!(file.ends_with(&bytes[t.offset as usize..(t.offset + t.size) as usize]));
    }

    // Big-endian PCM comes out little-endian.
    let (_, split) = split_bank("fsb4", 2);
    let (bank, bytes) = found_fsb4(2);
    let t = &bank.info.tracks[0];
    let swapped: Vec<u8> = bytes[t.offset as usize..(t.offset + t.size) as usize].chunks(2).flat_map(|p| [p[1], p[0]]).collect();
    assert_eq!(split[0].0, "be_pcm.wav");
    assert!(split[0].1.ends_with(&swapped));
}

#[test]
fn a_manifest_cannot_split_outside_the_folder() {
    let f = audscan_fixtures::audio_archive();
    let found = scan(&f.data, &ScanOptions::default()).audio;
    let manifest = Manifest::new(SourceInfo::describe(Path::new(f.name), &f.data), ScanOptions::default(), &found);
    let pck = manifest.audio.iter().position(|a| a.format == Container::Pck).unwrap();
    let dir = tempfile::tempdir().unwrap();
    for evil in ["wem/../../evil", "..", "a\\b"] {
        let mut m = manifest.clone();
        m.audio[pck].tracks[1].extension = Some(evil.into());
        let err = extract_all(&f.data, &m, dir.path(), &ExtractOptions { split: true, ..Default::default() }).unwrap_err();
        assert!(matches!(err, Error::BadFilename(_)), "{evil}: {err}");
    }
    // A language that isn't a plain name is left out of the path, not followed.
    let mut m = manifest.clone();
    m.audio[pck].tracks[2].language = Some("../..".into());
    let files = extract_all(&f.data, &m, dir.path(), &ExtractOptions { split: true, ..Default::default() }).unwrap();
    assert!(files[pck].split.iter().all(|p| p.starts_with(dir.path().join(m.audio[pck].file.replace(".pck", "")))));
    // A track outside its bank is refused.
    let mut m = manifest.clone();
    m.audio[pck].tracks[1].size = m.audio[pck].size;
    let err = extract_all(&f.data, &m, dir.path(), &ExtractOptions { split: true, ..Default::default() }).unwrap_err();
    assert!(matches!(err, Error::Audio { .. }), "{err}");
}

/// The generated files in `tests/fixtures` must match what the builder makes now.
#[test]
fn checked_in_fixtures_are_current() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for f in audscan_fixtures::all() {
        let on_disk = fs::read(dir.join(format!("{}.bin", f.name))).unwrap();
        assert!(on_disk == f.data, "{}.bin is stale: run `cargo run -p audscan-fixtures --bin gen-fixtures`", f.name);
    }
}
