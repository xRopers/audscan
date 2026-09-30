//! Custom widgets: the file strip showing where each sound is, and the waveform.

use audscan_core::{AudioEntry, Container, Rejected};
use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Colour for a found file by its format.
pub fn format_color(entry: &AudioEntry) -> Color32 {
    match entry.format {
        Container::Riff if entry.wwise => Color32::from_rgb(90, 160, 255),
        Container::Riff => Color32::from_rgb(110, 200, 120),
        Container::Bnk | Container::Pck => Color32::from_rgb(150, 120, 240),
        Container::Fsb4 | Container::Fsb5 => Color32::from_rgb(230, 150, 70),
        Container::Ogg => Color32::from_rgb(220, 200, 80),
    }
}

pub const REJECTED_COLOR: Color32 = Color32::from_rgb(220, 70, 60);

/// The file at a glance: one lane with every sound (and rejected headers in red).
/// Returns the file clicked, if any.
pub fn file_strip(ui: &mut Ui, file_len: u64, audio: &[AudioEntry], rejected: &[Rejected], selected: Option<u32>) -> Option<u32> {
    const H: f32 = 18.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), H), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    if file_len == 0 {
        return None;
    }
    let x_of = |offset: u64| rect.left() + (offset as f64 / file_len as f64 * f64::from(rect.width())) as f32;
    for a in audio {
        let (x0, x1) = (x_of(a.offset), x_of(a.end()));
        let r = Rect::from_min_max(Pos2::new(x0, rect.top() + 2.0), Pos2::new(x1.max(x0 + 1.0), rect.bottom() - 2.0));
        painter.rect_filled(r, 0.0, format_color(a));
        if selected == Some(a.id) {
            painter.rect_stroke(r.expand(1.0), 0.0, Stroke::new(2.0, ui.visuals().strong_text_color()), StrokeKind::Outside);
        }
    }
    for r in rejected {
        let x = x_of(r.offset);
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, REJECTED_COLOR));
    }
    // The file under the pointer, or the nearest one within a few pixels.
    let hit = |pos: Pos2| {
        audio
            .iter()
            .map(|a| {
                let (x0, x1) = (x_of(a.offset), x_of(a.end()).max(x_of(a.offset) + 1.0));
                (a, if pos.x < x0 { x0 - pos.x } else if pos.x > x1 { pos.x - x1 } else { 0.0 })
            })
            .filter(|(_, d)| *d <= 4.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(a, _)| a)
    };
    let hovered = response.hover_pos().and_then(hit);
    let response = match hovered {
        Some(a) => response.on_hover_text_at_pointer(format!("{:#x}: {} {}", a.offset, a.label(), a.codec)),
        None => response,
    };
    if response.clicked() { response.interact_pointer_pos().and_then(hit).map(|a| a.id) } else { None }
}

/// A waveform from (min, max) peaks, with the play position as a line. Returns where it
/// was clicked, from 0 to 1.
pub fn waveform(ui: &mut Ui, peaks: &[(f32, f32)], position: Option<f32>, height: f32) -> Option<f32> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "waveform"));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    let mid = rect.center().y;
    let half = rect.height() / 2.0 - 2.0;
    let color = ui.visuals().selection.bg_fill;
    let columns = rect.width().max(1.0) as usize;
    if !peaks.is_empty() {
        for c in 0..columns {
            let from = c * peaks.len() / columns;
            let to = ((c + 1) * peaks.len() / columns).max(from + 1).min(peaks.len());
            let (lo, hi) = peaks[from..to].iter().fold((0f32, 0f32), |(lo, hi), &(a, b)| (lo.min(a), hi.max(b)));
            let x = rect.left() + c as f32 + 0.5;
            painter.line_segment([Pos2::new(x, mid - hi * half), Pos2::new(x, mid - lo * half - 0.5)], Stroke::new(1.0, color));
        }
    }
    painter.line_segment([Pos2::new(rect.left(), mid), Pos2::new(rect.right(), mid)], Stroke::new(0.5, ui.visuals().weak_text_color()));
    if let Some(p) = position {
        let x = rect.left() + p.clamp(0.0, 1.0) * rect.width();
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, ui.visuals().strong_text_color()));
    }
    let clicked = response.clicked().then(|| response.interact_pointer_pos()).flatten();
    clicked.map(|pos| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0))
}

/// Grey for things that can't be played.
pub fn muted(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}
