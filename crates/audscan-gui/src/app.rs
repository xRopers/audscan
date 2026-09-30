//! The window: menus, the table of everything found, and the details pane with a bank's
//! tracks and the selected sound's waveform and playback. State lives in [`Session`];
//! slow work runs through [`Jobs`] and [`Previewer`].

use std::path::PathBuf;
use std::sync::Arc;

use audscan_core::AudioEntry;
use egui::{Align, Color32, Key, Layout, Modifiers, RichText, Sense, Ui, ViewportCommand};
use egui_extras::{Column, TableBuilder};

use crate::jobs::{Jobs, finish};
use crate::player::Player;
use crate::preview::Previewer;
use crate::session::{self, Level, Preview, Selection, Session, clock, entry_seconds, human_size, length};
use crate::widgets::{REJECTED_COLOR, file_strip, format_color, muted, waveform};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    OpenFile(PathBuf),
    Rescan,
    Close,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Offset,
    Size,
    Format,
    Codec,
    Channels,
    Rate,
    Length,
    Contents,
}

pub struct App {
    pub session: Session,
    pub jobs: Jobs,
    /// The context of the last frame drawn (for windows, repaints and viewport commands).
    ctx: egui::Context,
    pub preview: Previewer,
    player: Player,
    /// What's playing, and from which frame the last play started.
    playing: Option<Selection>,
    pub selected: Option<Selection>,
    scroll_to_selected: bool,
    sort: (SortKey, bool),
    pub filter: String,
    show_log: bool,
    pub show_rejected: bool,
    title: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            session: Session::default(),
            jobs: Jobs::default(),
            ctx: egui::Context::default(),
            preview: Previewer::default(),
            player: Player::default(),
            playing: None,
            selected: None,
            scroll_to_selected: false,
            sort: (SortKey::Offset, true),
            filter: String::new(),
            show_log: false,
            show_rejected: false,
            title: String::new(),
        }
    }

    // ----- actions -------------------------------------------------------------------

    pub fn request(&mut self, action: Action) {
        match action {
            Action::OpenFile(path) => self.open_file(path),
            Action::Rescan => self.rescan(),
            Action::Close => {
                self.deselect();
                self.session.close();
            }
            Action::Exit => self.ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }

    fn open_file(&mut self, path: PathBuf) {
        self.deselect();
        let opts = self.session.scan_options.clone();
        self.jobs.start("Opening and scanning", move || {
            finish("Open", session::open_and_scan(&path, &opts), Session::set_opened)
        });
    }

    fn rescan(&mut self) {
        let Some(file) = &self.session.file else { return };
        let file = session::OpenFile { path: file.path.clone(), data: file.data.clone() };
        let opts = self.session.scan_options.clone();
        self.deselect();
        self.jobs.start("Scanning", move || {
            let scanned = session::run_scan(&file, &opts);
            Box::new(move |s: &mut Session| s.set_scanned(scanned))
        });
    }

    /// Select a file or a track, and start decoding it for the preview.
    pub fn select(&mut self, selection: Selection) {
        if self.selected != Some(selection) {
            self.stop();
        }
        self.selected = Some(selection);
        self.scroll_to_selected = true;
        let (Some(file), Some(entry)) = (&self.session.file, self.session.entry(selection.id)) else { return };
        // A bank as a whole isn't one sound: only its tracks are previewed.
        if selection.track.is_some() || entry.tracks.is_empty() {
            self.preview.request(selection, file.data.clone(), entry.clone());
        }
    }

    fn deselect(&mut self) {
        self.stop();
        self.selected = None;
        self.preview.clear();
    }

    /// The selection's preview, once decoded.
    pub fn shown(&self) -> Option<&Result<Arc<Preview>, String>> {
        self.preview.get(self.selected?)
    }

    pub fn is_playing(&self) -> bool {
        self.playing.is_some() && self.player.playing()
    }

    /// Play the selection from `fraction` (0 to 1) of the way in.
    pub fn play(&mut self, fraction: f32) {
        let Some(selection) = self.selected else { return };
        let Some(Ok(preview)) = self.preview.get(selection) else { return };
        let pcm = preview.pcm.clone();
        let from = (f64::from(fraction.clamp(0.0, 1.0)) * pcm.frames() as f64) as u64;
        match self.player.play(pcm, from) {
            Ok(()) => self.playing = Some(selection),
            Err(e) => self.session.error(format!("Play: {e}")),
        }
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.playing = None;
    }

    fn toggle_play(&mut self) {
        if self.is_playing() { self.stop() } else { self.play(0.0) }
    }

    fn pick_and_open(&mut self) {
        if let Some(path) = rfd::FileDialog::new().set_title("Open a file to scan").pick_file() {
            self.request(Action::OpenFile(path));
        }
    }

    fn data(&self) -> Option<Arc<audscan_core::input::Input>> {
        self.session.file.as_ref().map(|f| f.data.clone())
    }

    fn save_item(&mut self, selection: Selection, convert: bool) {
        let (Some(data), Some(entry)) = (self.data(), self.session.entry(selection.id).cloned()) else { return };
        let name = session::item_file_name(&entry, selection.track);
        let title = if convert { "Save as Ogg or WAV" } else { "Save the file" };
        let Some(path) = rfd::FileDialog::new().set_title(title).set_file_name(&name).save_file() else { return };
        if self.session.file.as_ref().is_some_and(|f| same_file(&f.path, &path)) {
            self.session.error("Save: that's the file being scanned; choose another name");
            return;
        }
        self.jobs.start("Saving", move || {
            let result = if convert {
                session::save_converted(&data, &entry, selection.track, &path)
            } else {
                session::save_item(&data, &entry, selection.track, &path).map(|()| path)
            };
            finish("Save", result, |s, path| s.info(format!("saved {}", path.display())))
        });
    }

    /// Extract everything (or only `only`) to a folder the user picks.
    fn extract_with_dialog(&mut self, only: Option<u32>, split: bool, convert: bool) {
        if let Some(dir) = rfd::FileDialog::new().set_title("Extract into folder").pick_folder() {
            self.extract_to(dir, only, split, convert);
        }
    }

    /// Extract into `dir` in the background (what the Extract menu items do after asking).
    pub fn extract_to(&mut self, dir: PathBuf, only: Option<u32>, split: bool, convert: bool) {
        let (Some(data), Some(scanned)) = (self.data(), &self.session.scanned) else { return };
        let manifest = scanned.manifest.clone();
        self.jobs.start("Extracting", move || {
            let result = session::extract(&data, &manifest, only, &dir, split, convert);
            finish("Extract", result, move |s, r| {
                let mut text = format!("extracted {} file(s) to {}", r.files, dir.display());
                if split {
                    text += &format!(", {} split out", r.split);
                }
                if convert {
                    text += &format!(", {} converted to Ogg/WAV", r.converted);
                    if r.not_converted > 0 {
                        text += &format!(" ({} couldn't be: see extract --convert in the CLI for why)", r.not_converted);
                    }
                }
                s.info(text);
            })
        });
    }

    // ----- drawing -------------------------------------------------------------------

    pub fn show(&mut self, ui: &mut Ui) {
        if self.ctx != *ui.ctx() {
            self.ctx = ui.ctx().clone();
            let waker = self.ctx.clone();
            self.jobs.set_waker(move || waker.request_repaint());
        }
        self.preview.poll();
        self.jobs.poll(&mut self.session);
        if self.selected.is_some_and(|s| self.session.entry(s.id).is_none()) {
            self.deselect();
        }
        if self.playing.is_some() && !self.player.playing() {
            self.playing = None;
        }
        self.handle_input(ui);
        self.update_title();

        egui::Panel::top("menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| self.menu_bar(ui));
            ui.add_space(2.0);
            self.toolbar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if self.session.scanned.is_some() {
            egui::Panel::top("strip").show(ui, |ui| {
                ui.add_space(4.0);
                self.strip(ui);
                ui.add_space(4.0);
            });
        }
        if self.selected.is_some() {
            egui::Panel::right("details").resizable(true).default_size(560.0).min_size(360.0).show(ui, |ui| self.details(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));

        self.log_window();
        self.rejected_window();
        if self.jobs.busy() || self.preview.loading() || self.is_playing() {
            self.ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn handle_input(&mut self, ui: &Ui) {
        let ctx = ui.ctx().clone();
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()));
        if let Some(path) = dropped
            && !self.jobs.busy()
        {
            self.request(Action::OpenFile(path));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) && !self.jobs.busy() {
            self.pick_and_open();
        }
        if ctx.memory(|m| m.focused().is_none()) {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space)) {
                self.toggle_play();
            }
            // Up and down move through the files (or a bank's tracks, once one is picked).
            let step = ctx.input(|i| i.key_pressed(Key::ArrowDown) as i64 - i.key_pressed(Key::ArrowUp) as i64);
            if step != 0 {
                self.step_selection(step);
            }
        }
    }

    fn step_selection(&mut self, step: i64) {
        if let Some(Selection { id, track: Some(t) }) = self.selected {
            let count = self.session.entry(id).map_or(0, |e| e.tracks.len());
            let next = (t as i64 + step).clamp(0, count as i64 - 1) as usize;
            self.select(Selection { id, track: Some(next) });
            return;
        }
        let view = self.view_order();
        let audio = self.session.audio();
        let pos = self.selected.and_then(|s| view.iter().position(|&i| audio[i].id == s.id));
        let next = match pos {
            Some(p) => (p as i64 + step).clamp(0, view.len() as i64 - 1) as usize,
            None => 0,
        };
        if let Some(&i) = view.get(next) {
            let id = audio[i].id;
            self.select(Selection { id, track: None });
        }
    }

    fn update_title(&mut self) {
        let title = match &self.session.file {
            Some(f) => format!("{} - audscan", f.path.file_name().map_or_else(|| f.path.display().to_string(), |n| n.to_string_lossy().into_owned())),
            None => "audscan".to_string(),
        };
        if title != self.title {
            self.ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    fn menu_bar(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        let has_file = self.session.file.is_some();
        let has_audio = !self.session.audio().is_empty();
        ui.menu_button("File", |ui| {
            if ui.add_enabled(!busy, egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                ui.close();
                self.pick_and_open();
            }
            if ui.add_enabled(has_file && !busy, egui::Button::new("Scan again")).clicked() {
                ui.close();
                self.request(Action::Rescan);
            }
            if ui.add_enabled(has_file && !busy, egui::Button::new("Close")).clicked() {
                ui.close();
                self.request(Action::Close);
            }
            ui.separator();
            if ui.button("Exit").clicked() {
                ui.close();
                self.request(Action::Exit);
            }
        });
        ui.menu_button("Audio", |ui| {
            if ui.add_enabled(has_audio && !busy, egui::Button::new("Extract all…")).clicked() {
                ui.close();
                self.extract_with_dialog(None, false, false);
            }
            if ui
                .add_enabled(has_audio && !busy, egui::Button::new("Extract all, split and converted…"))
                .on_hover_text("Also every sound inside banks and packages, and every WEM as Ogg or WAV")
                .clicked()
            {
                ui.close();
                self.extract_with_dialog(None, true, true);
            }
            ui.separator();
            match self.selected {
                Some(sel) => {
                    if ui.add_enabled(!busy, egui::Button::new("Save selected…")).clicked() {
                        ui.close();
                        self.save_item(sel, false);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Save selected as Ogg/WAV…")).clicked() {
                        ui.close();
                        self.save_item(sel, true);
                    }
                    if ui.add_enabled(!self.is_playing(), egui::Button::new("Play").shortcut_text("Space")).clicked() {
                        ui.close();
                        self.play(0.0);
                    }
                    if ui.add_enabled(self.is_playing(), egui::Button::new("Stop").shortcut_text("Space")).clicked() {
                        ui.close();
                        self.stop();
                    }
                }
                None => {
                    ui.add_enabled(false, egui::Button::new("Select a sound for more"));
                }
            }
        });
        ui.menu_button("View", |ui| {
            if ui.button("Rejected headers").clicked() {
                ui.close();
                self.show_rejected = true;
            }
            if ui.button("Log").clicked() {
                ui.close();
                self.show_log = true;
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let busy = self.jobs.busy();
        let has_audio = !self.session.audio().is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Open…")).clicked() {
                self.pick_and_open();
            }
            if ui.add_enabled(has_audio && !busy, egui::Button::new("Extract all…")).clicked() {
                self.extract_with_dialog(None, true, true);
            }
            ui.separator();
            ui.label("Filter:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("format, codec, name…").desired_width(200.0));
            if !self.filter.is_empty() && ui.small_button("×").on_hover_text("Clear the filter").clicked() {
                self.filter.clear();
            }
        });
    }

    fn strip(&mut self, ui: &mut Ui) {
        let Some(file) = &self.session.file else { return };
        let clicked = file_strip(ui, file.len(), self.session.audio(), self.session.rejected(), self.selected.map(|s| s.id));
        if let Some(id) = clicked {
            self.select(Selection { id, track: None });
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(job) = self.jobs.current() {
                ui.spinner();
                ui.label(format!("{}… {:.1} s", job.label, job.started.elapsed().as_secs_f64()));
            } else if let Some(line) = self.session.log.last() {
                let text = RichText::new(&line.text).color(level_color(ui, line.level));
                if ui.add(egui::Label::new(text).sense(Sense::click()).truncate()).on_hover_text("Show the log").clicked() {
                    self.show_log = true;
                }
            } else {
                ui.label("Open a file, or drop one on the window.");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(file) = &self.session.file {
                    ui.label(human_size(file.len()));
                    if self.session.scanned.is_some() {
                        ui.separator();
                        let rejected = self.session.rejected().len();
                        if rejected > 0 && ui.link(RichText::new(format!("{rejected} rejected")).color(REJECTED_COLOR)).clicked() {
                            self.show_rejected = true;
                        }
                        ui.label(format!("{} audio files", self.session.audio().len()));
                    }
                }
            });
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.session.file.is_none() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                ui.heading("audscan");
                ui.label("Find, play, extract and convert audio inside binary files.");
                ui.add_space(12.0);
                if ui.add_enabled(!self.jobs.busy(), egui::Button::new("Open a file…")).clicked() {
                    self.pick_and_open();
                }
                ui.add_space(8.0);
                ui.weak("or drop a file on this window");
            });
            return;
        }
        if self.session.audio().is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 3.0);
                if self.jobs.busy() {
                    ui.spinner();
                    return;
                }
                ui.label("No audio found.");
                let rejected = self.session.rejected().len();
                if rejected > 0 && ui.link(format!("{rejected} header(s) look like audio but can't be used")).clicked() {
                    self.show_rejected = true;
                }
            });
            return;
        }
        self.table(ui);
    }

    /// Indices into the manifest's audio, filtered and sorted.
    fn view_order(&self) -> Vec<usize> {
        let audio = self.session.audio();
        let needle = self.filter.trim().to_lowercase();
        let mut view: Vec<usize> = (0..audio.len())
            .filter(|&i| {
                let a = &audio[i];
                needle.is_empty()
                    || a.label().to_lowercase().contains(&needle)
                    || a.codec.to_lowercase().contains(&needle)
                    || contents(a).to_lowercase().contains(&needle)
                    || format!("{:#x}", a.offset).contains(&needle)
            })
            .collect();
        let (key, ascending) = self.sort;
        view.sort_by(|&x, &y| {
            let (a, b) = (&audio[x], &audio[y]);
            let order = match key {
                SortKey::Offset => a.offset.cmp(&b.offset),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Format => a.label().cmp(&b.label()),
                SortKey::Codec => a.codec.cmp(&b.codec),
                SortKey::Channels => a.channels.cmp(&b.channels),
                SortKey::Rate => a.sample_rate.cmp(&b.sample_rate),
                SortKey::Length => entry_seconds(a).unwrap_or(-1.0).total_cmp(&entry_seconds(b).unwrap_or(-1.0)),
                SortKey::Contents => contents(a).cmp(&contents(b)),
            };
            if ascending { order.then(a.offset.cmp(&b.offset)) } else { order.reverse() }
        });
        view
    }

    fn table(&mut self, ui: &mut Ui) {
        ui.style_mut().interaction.selectable_labels = false;
        let view = self.view_order();
        let audio = self.session.audio();
        let selected = self.selected.map(|s| s.id);
        let mut clicked = None;
        let mut sort = self.sort;
        let mut table = TableBuilder::new(ui)
            .id_salt("files")
            .striped(true)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::auto().at_least(90.0))
            .column(Column::auto().at_least(70.0))
            .column(Column::auto().at_least(60.0))
            .column(Column::auto().at_least(130.0))
            .column(Column::auto().at_least(30.0))
            .column(Column::auto().at_least(50.0))
            .column(Column::auto().at_least(60.0))
            .column(Column::remainder().at_least(100.0));
        if self.scroll_to_selected {
            if let Some(row) = selected.and_then(|id| view.iter().position(|&i| audio[i].id == id)) {
                table = table.scroll_to_row(row, Some(Align::Center));
            }
            self.scroll_to_selected = false;
        }
        let headers = [
            ("Offset", SortKey::Offset),
            ("Size", SortKey::Size),
            ("Format", SortKey::Format),
            ("Codec", SortKey::Codec),
            ("Ch", SortKey::Channels),
            ("Rate", SortKey::Rate),
            ("Length", SortKey::Length),
            ("Contents", SortKey::Contents),
        ];
        table
            .header(22.0, |mut header| {
                for (label, key) in headers {
                    header.col(|ui| {
                        let arrow = match sort {
                            (k, true) if k == key => " ⏶",
                            (k, false) if k == key => " ⏷",
                            _ => "",
                        };
                        if ui.add(egui::Button::new(RichText::new(format!("{label}{arrow}")).strong()).frame(false)).clicked() {
                            sort = if sort.0 == key { (key, !sort.1) } else { (key, true) };
                        }
                    });
                }
            })
            .body(|mut body| {
                let row_height = body.ui_mut().text_style_height(&egui::TextStyle::Body) + 4.0;
                body.rows(row_height, view.len(), |mut row| {
                    let a = &audio[view[row.index()]];
                    row.set_selected(selected == Some(a.id));
                    row.col(|ui| {
                        ui.monospace(format!("{:#010x}", a.offset));
                    });
                    row.col(|ui| {
                        ui.label(human_size(a.size));
                    });
                    row.col(|ui| {
                        ui.colored_label(format_color(a), a.label());
                    });
                    row.col(|ui| {
                        ui.label(&a.codec);
                    });
                    row.col(|ui| {
                        ui.label(if a.channels > 0 { a.channels.to_string() } else { String::new() });
                    });
                    row.col(|ui| {
                        ui.label(if a.sample_rate > 0 { a.sample_rate.to_string() } else { String::new() });
                    });
                    row.col(|ui| {
                        ui.label(length(entry_seconds(a)));
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(contents(a)).truncate());
                    });
                    if row.response().clicked() {
                        clicked = Some(a.id);
                    }
                });
            });
        self.sort = sort;
        if let Some(id) = clicked {
            self.select(Selection { id, track: None });
            self.scroll_to_selected = false;
        }
    }

    fn details(&mut self, ui: &mut Ui) {
        let Some(selection) = self.selected else { return };
        let Some(entry) = self.session.entry(selection.id).cloned() else { return };
        ui.add_space(4.0);
        ui.heading(format!("{} at {:#x}", entry.label(), entry.offset));
        egui::Grid::new("facts").num_columns(2).spacing([16.0, 3.0]).show(ui, |ui| {
            let fact = |ui: &mut Ui, name: &str, value: String| {
                ui.weak(name);
                ui.label(value);
                ui.end_row();
            };
            fact(ui, "Codec", entry.codec.clone());
            if entry.channels > 0 {
                fact(ui, "Channels", entry.channels.to_string());
                fact(ui, "Sample rate", format!("{} Hz", entry.sample_rate));
            }
            if let Some(s) = entry_seconds(&entry) {
                fact(ui, "Length", length(Some(s)));
            }
            fact(ui, "Size", format!("{} ({} bytes)", human_size(entry.size), entry.size));
            fact(ui, "Extracted as", entry.file.clone());
        });
        if let Some(note) = &entry.note {
            ui.colored_label(ui.visuals().warn_fg_color, note);
        }
        ui.horizontal(|ui| {
            let busy = self.jobs.busy();
            let whole = Selection { id: entry.id, track: None };
            if ui.add_enabled(!busy, egui::Button::new("Save file…")).clicked() {
                self.save_item(whole, false);
            }
            if entry.wwise && entry.tracks.is_empty() && ui.add_enabled(!busy, egui::Button::new("Save as Ogg/WAV…")).clicked() {
                self.save_item(whole, true);
            }
            if !entry.tracks.is_empty()
                && ui.add_enabled(!busy, egui::Button::new("Extract tracks…")).on_hover_text("Every track as a file of its own, WEMs also converted").clicked()
            {
                self.extract_with_dialog(Some(entry.id), true, true);
            }
        });
        ui.separator();

        if !entry.tracks.is_empty() {
            ui.label(RichText::new(format!("{} tracks", entry.tracks.len())).strong());
            self.tracks_table(ui, &entry);
            ui.separator();
        }
        self.player_pane(ui, &entry, selection);
    }

    fn tracks_table(&mut self, ui: &mut Ui, entry: &AudioEntry) {
        ui.push_id("tracks", |ui| {
            ui.style_mut().interaction.selectable_labels = false;
            let current = self.selected.and_then(|s| s.track);
            let mut clicked = None;
            TableBuilder::new(ui)
                .id_salt("tracks")
                .striped(true)
                .sense(Sense::click())
                .max_scroll_height(260.0)
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(Column::auto().at_least(30.0))
                .column(Column::auto().at_least(140.0))
                .column(Column::auto().at_least(40.0))
                .column(Column::auto().at_least(110.0))
                .column(Column::auto().at_least(30.0))
                .column(Column::auto().at_least(50.0))
                .column(Column::remainder().at_least(50.0))
                .header(20.0, |mut header| {
                    for label in ["#", "Name", "Type", "Codec", "Ch", "Rate", "Length"] {
                        header.col(|ui| {
                            ui.strong(label);
                        });
                    }
                })
                .body(|body| {
                    let row_height = 20.0;
                    body.rows(row_height, entry.tracks.len(), |mut row| {
                        let i = row.index();
                        let t = &entry.tracks[i];
                        row.set_selected(current == Some(i));
                        row.col(|ui| {
                            ui.label(i.to_string());
                        });
                        row.col(|ui| {
                            let name = match &t.language {
                                Some(lang) if lang != "sfx" => format!("{} [{lang}]", t.display_name()),
                                _ => t.display_name(),
                            };
                            ui.add(egui::Label::new(name).truncate());
                        });
                        row.col(|ui| {
                            ui.label(t.extension.as_deref().unwrap_or(""));
                        });
                        row.col(|ui| {
                            ui.label(t.codec.as_deref().unwrap_or(&entry.codec));
                        });
                        row.col(|ui| {
                            ui.label(if t.channels > 0 { t.channels.to_string() } else { String::new() });
                        });
                        row.col(|ui| {
                            ui.label(if t.sample_rate > 0 { t.sample_rate.to_string() } else { String::new() });
                        });
                        row.col(|ui| {
                            let seconds = t.samples.filter(|_| t.sample_rate > 0).map(|s| s as f64 / f64::from(t.sample_rate));
                            ui.label(length(seconds));
                        });
                        let response = row.response();
                        let response = match &t.note {
                            Some(note) => response.on_hover_text(note),
                            None => response,
                        };
                        if response.clicked() {
                            clicked = Some(i);
                        }
                    });
                });
            if let Some(i) = clicked {
                self.select(Selection { id: entry.id, track: Some(i) });
            }
        });
    }

    fn player_pane(&mut self, ui: &mut Ui, entry: &AudioEntry, selection: Selection) {
        if selection.track.is_none() && !entry.tracks.is_empty() {
            ui.weak("Pick a track to play it.");
            return;
        }
        if let Some(i) = selection.track {
            let t = &entry.tracks[i];
            ui.label(RichText::new(format!("Track {i}: {}", t.display_name())).strong());
            ui.horizontal(|ui| {
                let busy = self.jobs.busy();
                if ui.add_enabled(!busy, egui::Button::new("Save track…")).clicked() {
                    self.save_item(selection, false);
                }
                if t.extension.as_deref() == Some("wem") && ui.add_enabled(!busy, egui::Button::new("Save as Ogg/WAV…")).clicked() {
                    self.save_item(selection, true);
                }
            });
            if let Some(note) = &t.note {
                ui.colored_label(ui.visuals().warn_fg_color, note);
            }
        }
        match self.shown().cloned() {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Decoding…");
                });
            }
            Some(Err(e)) => {
                ui.colored_label(muted(ui), format!("Can't play this: {e}"));
            }
            Some(Ok(preview)) => {
                let total = preview.pcm.frames().max(1);
                let position = if self.playing == Some(selection) { self.player.position() } else { None };
                let fraction = position.map(|p| p as f32 / total as f32);
                if let Some(at) = waveform(ui, &preview.peaks, fraction, 120.0) {
                    self.play(at);
                }
                ui.horizontal(|ui| {
                    if self.is_playing() && self.playing == Some(selection) {
                        if ui.button("Stop").clicked() {
                            self.stop();
                        }
                    } else if ui.button("Play").clicked() {
                        self.play(0.0);
                    }
                    let rate = f64::from(preview.pcm.sample_rate.max(1));
                    let now = position.map_or(0.0, |p| p as f64 / rate);
                    ui.monospace(format!("{} / {}", clock(now), clock(preview.pcm.seconds())));
                    ui.weak("click the waveform to play from there; Space plays and stops");
                });
            }
        }
    }

    fn log_window(&mut self) {
        let mut open = self.show_log;
        egui::Window::new("Log").open(&mut open).default_size([640.0, 320.0]).show(&self.ctx.clone(), |ui| {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                for line in &self.session.log {
                    ui.colored_label(level_color(ui, line.level), &line.text);
                }
            });
        });
        self.show_log = open;
    }

    fn rejected_window(&mut self) {
        let mut open = self.show_rejected;
        egui::Window::new("Rejected headers").open(&mut open).default_size([560.0, 300.0]).show(&self.ctx.clone(), |ui| {
            ui.label("These look like audio but can't be used.");
            ui.add_space(4.0);
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new("rejected").striped(true).num_columns(3).spacing([12.0, 3.0]).show(ui, |ui| {
                    for r in self.session.rejected() {
                        ui.monospace(format!("{:#010x}", r.offset));
                        ui.label(r.container.name());
                        ui.label(&r.reason);
                        ui.end_row();
                    }
                });
            });
        });
        self.show_rejected = open;
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// What the Contents column shows: a single track's name, or how many tracks and the first
/// few names.
pub fn contents(a: &AudioEntry) -> String {
    match a.tracks.as_slice() {
        [] => String::new(),
        [one] => one.display_name(),
        many => {
            let names: Vec<_> = many.iter().map(|t| t.display_name()).filter(|n| !n.is_empty()).take(3).collect();
            let more = if many.len() > names.len() && !names.is_empty() { ", …" } else { "" };
            let sep = if names.is_empty() { "" } else { ": " };
            format!("{} tracks{sep}{}{more}", many.len(), names.join(", "))
        }
    }
}

fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn level_color(ui: &Ui, level: Level) -> Color32 {
    match level {
        Level::Info => ui.visuals().text_color(),
        Level::Warn => ui.visuals().warn_fg_color,
        Level::Error => ui.visuals().error_fg_color,
    }
}
