//! The window itself, driven with egui_kittest (no GPU): clicks go through the real
//! widgets and are checked through the accessibility tree. Nothing here plays sound.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use audscan_gui::App;
use audscan_gui::app::Action;
use audscan_gui::session::Selection;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size([1400.0, 900.0]).build_ui_state(|ui, app: &mut App| app.show(ui), App::new())
}

/// Finish the running job (if any) and let the window catch up.
fn settle(h: &mut Harness<'static, App>) {
    let app = h.state_mut();
    app.jobs.wait(&mut app.session);
    h.run_steps(3);
}

/// Step until `done` holds (previews decode on another thread).
fn wait_until(h: &mut Harness<'static, App>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
        h.step();
    }
}

struct Input {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: audscan_fixtures::Fixture,
}

fn input() -> Input {
    let fixture = audscan_fixtures::audio_archive();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.bin");
    std::fs::write(&path, &fixture.data).unwrap();
    Input { _dir: dir, path, fixture }
}

fn open(h: &mut Harness<'static, App>, input: &Input) {
    h.state_mut().request(Action::OpenFile(input.path.clone()));
    settle(h);
}

fn offset_label(offset: usize) -> String {
    format!("{offset:#010x}")
}

#[test]
fn welcome_screen() {
    let mut h = harness();
    h.run_steps(2);
    h.get_by_label("Open a file…");
    h.get_by_label("or drop a file on this window");
}

#[test]
fn open_lists_everything_found() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let n = input.fixture.expected.len();
    assert_eq!(h.state().session.audio().len(), n);
    h.get_by_label(&format!("{n} audio files"));
    h.get_by_label(&format!("{} rejected", input.fixture.rejected.len()));
    h.get_by_label("3 tracks: music_intro, vo_line_01, amb_odd");
    h.get_by_label("wem BE");
}

#[test]
fn click_a_wav_to_decode_its_waveform() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let first = &input.fixture.expected[0];
    h.get_by_label(&offset_label(first.offset)).click();
    h.run_steps(2);
    assert_eq!(h.state().selected, Some(Selection { id: 0, track: None }));
    wait_until(&mut h, "the preview", |app| app.shown().is_some());
    let preview = h.state().shown().unwrap().as_ref().unwrap().clone();
    assert_eq!((preview.pcm.channels, preview.pcm.sample_rate, preview.pcm.frames()), (2, 44100, 1000));
    h.run_steps(2);
    h.get_by_label("waveform");
    h.get_by_label("Play");
}

#[test]
fn a_bank_lists_its_tracks_and_a_track_can_be_picked() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let bank = input.fixture.expected.iter().position(|e| e.container == "fsb4" && e.codec == "mixed").unwrap();
    h.get_by_label(&offset_label(input.fixture.expected[bank].offset)).click();
    h.run_steps(2);
    h.get_by_label("3 tracks");
    h.get_by_label("Pick a track to play it.");
    // A PCM track splits out as a WAV and plays; MPEG isn't decoded.
    h.get_by_label("menu_theme").click();
    h.run_steps(2);
    assert_eq!(h.state().selected, Some(Selection { id: bank as u32, track: Some(0) }));
    wait_until(&mut h, "the track's preview", |app| app.shown().is_some());
    let preview = h.state().shown().unwrap().as_ref().unwrap().clone();
    assert_eq!((preview.pcm.channels, preview.pcm.frames()), (2, 250));
    h.get_by_label("voice_01").click();
    h.run_steps(2);
    wait_until(&mut h, "the MPEG track's preview", |app| app.shown().is_some());
    assert!(h.state().shown().unwrap().is_err());
    h.run_steps(2);
    assert!(h.query_by_label_contains("Can't play this").is_some());
}

#[test]
fn arrow_keys_move_through_files() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().selected.map(|s| s.id), Some(0));
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().selected.map(|s| s.id), Some(1));
}

#[test]
fn filter_narrows_the_table() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    h.state_mut().filter = "opus".into();
    h.run_steps(3);
    let opus = input.fixture.expected.iter().find(|e| e.codec == "Opus").unwrap();
    h.get_by_label(&offset_label(opus.offset));
    assert!(h.query_by_label(&offset_label(input.fixture.expected[0].offset)).is_none());
}

#[test]
fn extract_all_split_and_converted() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let dir = tempfile::tempdir().unwrap();
    h.state_mut().extract_to(dir.path().to_path_buf(), None, true, true);
    settle(&mut h);
    let last = h.state().session.log.last().unwrap().text.clone();
    assert!(last.starts_with(&format!("extracted {} file(s)", input.fixture.expected.len())), "{last}");
    let bank = input.fixture.expected.iter().find(|e| e.container == "fsb4" && e.codec == "mixed").unwrap();
    assert!(dir.path().join(format!("{:08x}", bank.offset)).join("menu_theme.wav").exists());
}

#[test]
fn replace_a_track_preview_it_and_pack() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let bank = input.fixture.expected.iter().position(|e| e.label == "fsb5" && e.codec == "PCM 16-bit").unwrap() as u32;
    let track = Selection { id: bank, track: Some(0) };
    h.state_mut().select(track);
    h.run_steps(2);

    // A shorter WAV for the bank's one PCM track: checked, then taken.
    let mut rng = audscan_fixtures::Rng::new(5);
    let wav = input.path.with_file_name("new.wav");
    std::fs::write(&wav, audscan_fixtures::pcm_wav(1, 22050, 16, 250, &mut rng)).unwrap();
    h.state_mut().replace_with(track, wav.clone());
    settle(&mut h);
    assert_eq!(h.state().session.edit_path(track), Some(&wav));
    assert!(h.state().session.log.iter().any(|l| l.text.contains("smaller: padded inside")), "{:?}", h.state().session.log.last());
    h.run_steps(2);
    assert!(!h.get_all_by_label("edited").collect::<Vec<_>>().is_empty());

    // The preview plays the replacement, or the original when asked.
    wait_until(&mut h, "the replacement's preview", |app| app.shown().is_some());
    assert_eq!(h.state().shown().unwrap().as_ref().unwrap().pcm.frames(), 250);
    h.get_by_label("the original").click();
    h.run_steps(2);
    wait_until(&mut h, "the original's preview", |app| app.shown().is_some());
    assert_eq!(h.state().shown().unwrap().as_ref().unwrap().pcm.frames(), 500);

    // Something that can't go in is refused, and the edit stays.
    let wem = input.path.with_file_name("new.wem");
    std::fs::write(&wem, audscan_fixtures::pcm_wem(1, 22050, 10, &mut rng)).unwrap();
    h.state_mut().replace_with(track, wem);
    settle(&mut h);
    assert!(h.state().session.log.last().unwrap().text.starts_with("Replace:"));
    assert_eq!(h.state().session.edit_path(track), Some(&wav));

    // Dry run from the pack window, then write.
    h.get_by_label("Pack 1 edited…").click();
    h.run_steps(2);
    h.get_by_label("Dry run").click();
    h.run_steps(1);
    settle(&mut h);
    assert_eq!(h.state().session.last_pack.as_ref().unwrap().changed(), 1);
    h.get_by_label("Dry run: 1 file(s) would change.");
    let out = input.path.with_file_name("packed.bin");
    h.state_mut().start_pack(Some(out.clone()));
    settle(&mut h);
    let packed = std::fs::read(&out).unwrap();
    assert_eq!(packed.len(), input.fixture.data.len());
    let found = audscan_core::scan(&packed, &audscan_core::ScanOptions::default()).audio;
    let now = found.iter().find(|a| a.offset == input.fixture.expected[bank as usize].offset as u64).unwrap();
    assert_eq!(now.info.tracks[0].samples, Some(250));
    h.get_by_label_contains("Written and verified");
}

#[test]
fn unpacked_edits_are_confirmed_before_they_are_dropped() {
    let input = input();
    let mut h = harness();
    open(&mut h, &input);
    let out_dir = input.path.with_file_name("out");
    h.state_mut().extract_to(out_dir.clone(), None, true, false);
    settle(&mut h);
    // Edit the loose PCM WEM in the extract folder, then import.
    let mut rng = audscan_fixtures::Rng::new(6);
    let wem = &input.fixture.expected[3];
    std::fs::write(out_dir.join(format!("{:08x}.wem", wem.offset)), audscan_fixtures::pcm_wem(1, 48000, 20, &mut rng)).unwrap();
    h.state_mut().import_edits_from(out_dir);
    settle(&mut h);
    assert_eq!(h.state().session.edits.len(), 1);

    h.state_mut().request(Action::Close);
    h.run_steps(2);
    h.get_by_label("Unpacked edits");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(h.state().confirm.is_none());
    assert!(h.state().session.file.is_some());
    h.state_mut().request(Action::Close);
    h.run_steps(2);
    h.get_by_label("Discard edits").click();
    h.run_steps(2);
    assert!(h.state().session.file.is_none());
    assert!(h.state().session.edits.is_empty());
}
