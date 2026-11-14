use egui::{Color32, CornerRadius, Stroke};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x13, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x18, 0x1A, 0x20);
pub const CARD: Color32 = Color32::from_rgb(0x1E, 0x20, 0x27);
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x29, 0x32);
pub const LINE: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x36);
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE9, 0xEC);
pub const MUTED: Color32 = Color32::from_rgb(0x9F, 0xA4, 0xB0);
pub const ACCENT: Color32 = Color32::from_rgb(0xF0, 0xA5, 0x3A);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x1A, 0x12, 0x04);
pub const GOOD: Color32 = Color32::from_rgb(0x5B, 0xD0, 0x8A);
pub const BAD: Color32 = Color32::from_rgb(0xE5, 0x6B, 0x6B);

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    let r = CornerRadius::same(8);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.inactive.bg_fill = RAISED;
    v.widgets.inactive.weak_bg_fill = RAISED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x30, 0x34, 0x3F);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x38, 0x3C, 0x48);
    ctx.set_visuals(v);
}

/// Amber filled button used for the single primary action in a view.
pub fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(text).color(ON_ACCENT).strong()).fill(ACCENT).corner_radius(8))
}

pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(10, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(color));
        });
}

pub fn badge(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().fill(ACCENT).corner_radius(10).inner_margin(egui::Margin::symmetric(8, 1)).show(ui, |ui| {
        ui.label(egui::RichText::new(text).size(12.0).color(ON_ACCENT).strong());
    });
}

pub fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(11.5).color(MUTED).strong());
}
