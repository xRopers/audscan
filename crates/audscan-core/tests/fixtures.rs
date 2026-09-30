//! Scan and extract every fixture; audio must be reported at exactly the known offsets,
//! with the sizes and details the fixture builder worked out independently.

use std::fs;
use std::path::Path;

use audscan_core::{Container, Error, ExtractOptions, Manifest, ScanOptions, SourceInfo, audio_at, extract_all, scan};
use audscan_fixtures::Fixture;

type TrackRow = (Option<String>, u16, u32, u64);
type Row = (u64, String, String, u64, String, u16, u32, Option<u64>, Vec<TrackRow>, bool, u32);

fn expected_rows(f: &Fixture) -> Vec<Row> {
    f.expected
        .iter()
        .map(|e| {
            let tracks = e.tracks.iter().map(|&(name, ch, rate, samples)| (name.map(String::from), ch, rate, samples)).collect();
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
            let tracks = i.tracks.iter().map(|t| (t.name.clone(), t.channels, t.sample_rate, t.samples)).collect();
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
    let err = extract_all(&changed, &manifest, dir.path(), &ExtractOptions { verify_source: false }).unwrap_err();
    assert!(matches!(err, Error::Audio { id: 0, .. }), "{err}");
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
