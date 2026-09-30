//! Run the real binary on the fixtures.

use std::fs;
use std::process::Command;

use serde_json::Value;

fn audscan() -> Command {
    Command::new(env!("CARGO_BIN_EXE_audscan"))
}

fn write_fixture(dir: &std::path::Path, f: &audscan_fixtures::Fixture) -> std::path::PathBuf {
    let path = dir.join(format!("{}.bin", f.name));
    fs::write(&path, &f.data).unwrap();
    path
}

#[test]
fn scan_json_lists_audio_and_rejected_headers() {
    let dir = tempfile::tempdir().unwrap();
    let f = audscan_fixtures::audio_archive();
    let input = write_fixture(dir.path(), &f);
    let out = audscan().args(["scan", "--json"]).arg(&input).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let audio = json["audio"].as_array().unwrap();
    let offsets: Vec<_> = audio.iter().map(|a| a["offset"].as_u64().unwrap()).collect();
    let expected: Vec<_> = f.expected.iter().map(|e| e.offset as u64).collect();
    assert_eq!(offsets, expected);
    assert_eq!(json["rejected"].as_array().unwrap().len(), f.rejected.len());
    assert_eq!(audio[1]["codec"], "Wwise Vorbis");
    assert_eq!(audio[1]["big_endian"], true);
    assert_eq!(audio[1]["file"], format!("{:08x}.wem", f.expected[1].offset));
    let bank = audio.iter().find(|a| a["format"] == "fsb5").unwrap();
    assert_eq!(bank["tracks"][1]["name"], "vo_line_01");
}

#[test]
fn scan_text_then_extract_with_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let f = audscan_fixtures::audio_archive();
    let input = write_fixture(dir.path(), &f);
    let manifest = dir.path().join("m.json");
    let out = audscan().arg("scan").arg(&input).arg("-o").arg(&manifest).args(["--show-rejected", "--tracks"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&format!("{} audio file(s) found", f.expected.len())), "{text}");
    assert!(text.contains("unknown codec 99"), "{text}");
    assert!(text.contains("3 tracks: music_intro, vo_line_01, amb_odd"), "{text}");
    assert!(text.contains("wem BE"), "{text}");
    // 441000 samples at 44.1 kHz.
    assert!(text.contains("0:10.000  music_intro"), "{text}");

    let out_dir = dir.path().join("out");
    let out = audscan().arg("extract").arg(&input).arg("-m").arg(&manifest).arg("-d").arg(&out_dir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let extensions = ["wav", "wem", "fsb", "ogg", "bnk", "pck"];
    for e in &f.expected {
        let found: Vec<_> = extensions.iter().map(|x| out_dir.join(format!("{:08x}.{x}", e.offset))).filter(|p| p.exists()).collect();
        assert_eq!(found.len(), 1, "{:#x}", e.offset);
        assert_eq!(fs::read(&found[0]).unwrap(), &f.data[e.offset..e.offset + e.size]);
    }
}

#[test]
fn extract_refuses_a_changed_input_unless_forced() {
    let dir = tempfile::tempdir().unwrap();
    let f = audscan_fixtures::audio_archive();
    let input = write_fixture(dir.path(), &f);
    let manifest = dir.path().join("m.json");
    assert!(audscan().arg("scan").arg(&input).arg("-o").arg(&manifest).output().unwrap().status.success());
    // Change a byte outside every file.
    let mut data = f.data.clone();
    data[10] ^= 0xff;
    fs::write(&input, &data).unwrap();
    let out_dir = dir.path().join("out");
    let run = |force: bool| {
        let mut cmd = audscan();
        cmd.arg("extract").arg(&input).arg("-m").arg(&manifest).arg("-d").arg(&out_dir);
        if force {
            cmd.arg("--force");
        }
        cmd.output().unwrap()
    };
    let out = run(false);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not match the manifest"));
    assert!(run(true).status.success());
}

#[test]
fn formats_filter_takes_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let f = audscan_fixtures::audio_archive();
    let input = write_fixture(dir.path(), &f);
    let out = audscan().args(["scan", "--json", "--formats", "wem,fmod"]).arg(&input).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    let formats: Vec<_> = json["audio"].as_array().unwrap().iter().map(|a| a["format"].as_str().unwrap().to_string()).collect();
    // wem is RIFF; fmod is FSB4 and FSB5.
    assert!(formats.iter().all(|f| f == "riff" || f == "fsb4" || f == "fsb5"), "{formats:?}");
    assert!(formats.contains(&"fsb4".to_string()) && formats.contains(&"fsb5".to_string()));
}

#[test]
fn extract_split_writes_wwise_files_and_lists_them() {
    let dir = tempfile::tempdir().unwrap();
    let f = audscan_fixtures::audio_archive();
    let input = write_fixture(dir.path(), &f);
    let out_dir = dir.path().join("out");
    let out = audscan().arg("extract").arg(&input).arg("-d").arg(&out_dir).arg("--split").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // 3 + 3 + 1 from the FSB4 banks, 3 + 1 + 3 from the FSB5 ones, 3 + 1 from the Wwise
    // banks, 4 + 1 from the packages.
    assert!(String::from_utf8_lossy(&out.stdout).contains("23 file(s) split out"), "{}", String::from_utf8_lossy(&out.stdout));
    let pck = f.expected.iter().find(|e| e.container == "pck").unwrap();
    assert!(out_dir.join(format!("{:08x}", pck.offset)).join("english(us)").join("100.wem").exists());

    let out = audscan().args(["scan", "--tracks"]).arg(&input).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("4 files (1 bnk, 3 wem): 777, 100, 100, ..."), "{text}");
    assert!(text.contains("100 [english(us)]"), "{text}");
    assert!(text.contains("bnk BE"), "{text}");
}
