//! Grounded-Earth Warm Espresso living-room palette for Xbox 360 TV display.

use egui::{Color32, FontId, Stroke, TextStyle, Visuals};
use xbox360_draw::draw::Rgba8;

pub const CLEAR_BG: Rgba8 = Rgba8::new(0x1F, 0x19, 0x14, 0xFF);

pub const CANVAS: Color32 = Color32::from_rgb(0x1F, 0x19, 0x14);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x2A, 0x22, 0x1B);
pub const HEADER_DETAIL: Color32 = Color32::from_rgb(0x25, 0x1E, 0x18);
pub const CARD: Color32 = Color32::from_rgb(0x2F, 0x26, 0x1F);
pub const CARD_ACTIVE: Color32 = Color32::from_rgb(0x3B, 0x2F, 0x25);
pub const BORDER: Color32 = Color32::from_rgb(0x4A, 0x3C, 0x31);
pub const SAND_TEXT: Color32 = Color32::from_rgb(0xED, 0xE4, 0xD7);
pub const DRIFTWOOD_MUTED: Color32 = Color32::from_rgb(0xA8, 0x99, 0x89);
pub const TERRACOTTA_FOCUS: Color32 = Color32::from_rgb(0xC8, 0x7A, 0x4B);
pub const TERRACOTTA_ACCENT: Color32 = Color32::from_rgb(0xB8, 0x5D, 0x3B);

pub const SAGE_FG: Color32 = Color32::from_rgb(0x3A, 0x52, 0x30);
pub const SAGE_BG: Color32 = Color32::from_rgb(0xDC, 0xE5, 0xD5);
pub const AMBER_FG: Color32 = Color32::from_rgb(0x8A, 0x5A, 0x19);
pub const AMBER_BG: Color32 = Color32::from_rgb(0xF3, 0xE5, 0xC8);
pub const RUST_FG: Color32 = Color32::from_rgb(0x8E, 0x30, 0x26);
pub const RUST_BG: Color32 = Color32::from_rgb(0xF2, 0xDB, 0xD8);

pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();
    visuals.panel_fill = CANVAS;
    visuals.window_fill = HEADER_DETAIL;
    visuals.faint_bg_color = SIDEBAR;
    visuals.extreme_bg_color = CANVAS;
    visuals.override_text_color = Some(SAND_TEXT);
    visuals.selection.bg_fill = CARD_ACTIVE;
    visuals.selection.stroke = Stroke::new(2.0, TERRACOTTA_FOCUS);
    visuals.widgets.noninteractive.bg_fill = CARD;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, SAND_TEXT);
    visuals.widgets.inactive.bg_fill = CARD;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_fill = CARD_ACTIVE;
    visuals.widgets.hovered.bg_stroke = Stroke::new(2.0, TERRACOTTA_FOCUS);
    visuals.widgets.active.bg_fill = TERRACOTTA_ACCENT;
    visuals.widgets.active.bg_stroke = Stroke::new(2.0, TERRACOTTA_FOCUS);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(24.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(16.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(15.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(13.0));
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    ctx.set_style(style);
}
