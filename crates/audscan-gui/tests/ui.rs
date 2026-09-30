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
