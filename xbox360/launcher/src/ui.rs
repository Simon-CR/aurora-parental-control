//! Plex-style Warm Espresso TV interface built over egui DrawLists.

use egui::{Align, Color32, CornerRadius, Frame, Layout, RichText, Stroke, Vec2};
use launcher_core::catalog::{Catalog, GameEntry, SidebarTab};
use launcher_core::guard::GuardStatus;
use launcher_core::nav::{FocusZone, GRID_COLS, LockChoice, NavState, VISIBLE_ROWS};

use crate::assets::TextureManager;
use crate::theme;

const SIDEBAR_WIDTH: f32 = 228.0;
const TOP_BAR_HEIGHT: f32 = 54.0;
const HERO_BAR_HEIGHT: f32 = 148.0;

pub fn draw_launcher_ui(
    ctx: &egui::Context,
    catalog: &Catalog,
    filtered: &[usize],
    nav: &NavState,
    guard: &GuardStatus,
    pin_override: bool,
    textures: &TextureManager,
    notice: Option<&str>,
) {
    // 1. Top Bar
    egui::TopBottomPanel::top("shelf_topbar")
        .exact_height(TOP_BAR_HEIGHT)
        .frame(
            Frame::new()
                .fill(theme::HEADER_DETAIL)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::symmetric(16, 10)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    RichText::new("Shelf")
                        .strong()
                        .size(22.0)
                        .color(theme::TERRACOTTA_FOCUS),
                );
                ui.add_space(10.0);
                let active_tab = nav.active_filter_tab();
                ui.label(
                    RichText::new(format!("{} ({})", active_tab.label(), filtered.len()))
                        .size(18.0)
                        .strong()
                        .color(theme::SAND_TEXT),
                );
                ui.add_space(14.0);
                draw_chip(
                    ui,
                    &format!("Y: {}", nav.sort_mode.label()),
                    theme::CARD,
                    theme::DRIFTWOOD_MUTED,
                );
                draw_chip(
                    ui,
                    &format!("LT/RT: {}", nav.letter_filter.label()),
                    theme::CARD,
                    theme::DRIFTWOOD_MUTED,
                );
                if nav.local_mp_only {
                    draw_chip(ui, "Back: Local MP ON", theme::SAGE_BG, theme::SAGE_FG);
                } else {
                    draw_chip(ui, "Back: Local MP", theme::CARD, theme::DRIFTWOOD_MUTED);
                }
                if let Some(msg) = notice {
                    draw_chip(ui, msg, theme::AMBER_BG, theme::AMBER_FG);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (pill_bg, pill_fg) = guard_pill_colors(guard, pin_override);
                    draw_chip(
                        ui,
                        &guard.status_badge_text(pin_override),
                        pill_bg,
                        pill_fg,
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("LB/RB Category  |  X Fav  |  Start Aurora")
                            .size(13.0)
                            .color(theme::DRIFTWOOD_MUTED),
                    );
                });
            });
        });

    // 2. Bottom Hero Details Strip
    let selected_entry = filtered
        .get(nav.grid_idx)
        .and_then(|&idx| catalog.get(idx));

    egui::TopBottomPanel::bottom("shelf_hero")
        .exact_height(HERO_BAR_HEIGHT)
        .frame(
            Frame::new()
                .fill(theme::HEADER_DETAIL)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::symmetric(16, 12)),
        )
        .show(ctx, |ui| {
            draw_hero_strip(ui, selected_entry, textures);
        });

    // 3. Left Category Sidebar
    egui::SidePanel::left("shelf_sidebar")
        .exact_width(SIDEBAR_WIDTH)
        .resizable(false)
        .frame(
            Frame::new()
                .fill(theme::SIDEBAR)
                .stroke(Stroke::new(1.0, theme::BORDER))
                .inner_margin(egui::Margin::symmetric(10, 10)),
        )
        .show(ctx, |ui| {
            draw_sidebar(ui, catalog, nav);
        });

    // 4. Main 5-Column Poster Wall
    egui::CentralPanel::default()
        .frame(
            Frame::new()
                .fill(theme::CANVAS)
                .inner_margin(egui::Margin::symmetric(16, 12)),
        )
        .show(ctx, |ui| {
            draw_poster_grid(ui, catalog, filtered, nav, textures);
        });

    // 5. Overlays (Parental Lock / Launch Error)
    if nav.zone == FocusZone::LockModal {
        draw_lock_modal(ctx, nav, guard, notice);
    } else if nav.zone == FocusZone::LaunchError {
        draw_error_modal(ctx, nav.launch_error.as_deref().unwrap_or("Launch failed"));
    }
}

fn draw_chip(ui: &mut egui::Ui, text: &str, fill: Color32, fg: Color32) {
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(13.0).strong().color(fg));
        });
}

fn guard_pill_colors(guard: &GuardStatus, pin_override: bool) -> (Color32, Color32) {
    if pin_override {
        return (theme::AMBER_BG, theme::AMBER_FG);
    }
    if !guard.online {
        return (theme::CARD, theme::DRIFTWOOD_MUTED);
    }
    if guard.is_launch_blocked(false) {
        return (theme::RUST_BG, theme::RUST_FG);
    }
    if guard.remaining_sec <= 900 {
        return (theme::AMBER_BG, theme::AMBER_FG);
    }
    (theme::SAGE_BG, theme::SAGE_FG)
}

fn draw_sidebar(ui: &mut egui::Ui, catalog: &Catalog, nav: &NavState) {
    let filter_count = nav.tabs.iter().filter(|t| t.is_filter_tab()).count();
    let utility_height = 46.0;
    let scroll_height = (ui.available_height() - utility_height - 8.0).max(120.0);

    egui::ScrollArea::vertical()
        .id_salt("sidebar_scroll")
        .max_height(scroll_height)
        .show(ui, |ui| {
            for (idx, tab) in nav.tabs[..filter_count].iter().enumerate() {
                let is_hovered = idx == nav.sidebar_idx;
                let is_active = idx == nav.active_filter_tab_idx;
                let is_focused = nav.zone == FocusZone::Sidebar && is_hovered;
                let count = catalog
                    .tab_count_with_mp(*tab, nav.local_mp_only && *tab != SidebarTab::LocalMultiplayer);
                let resp = draw_sidebar_row(
                    ui,
                    tab.label(),
                    Some(count),
                    is_active,
                    is_focused,
                );
                if is_hovered {
                    resp.scroll_to_me(Some(Align::Center));
                }
            }
        });

    ui.separator();
    if let Some((idx, tab)) = nav
        .tabs
        .iter()
        .enumerate()
        .find(|(_, t)| **t == SidebarTab::OpenAurora)
    {
        let is_focused = nav.zone == FocusZone::Sidebar && nav.sidebar_idx == idx;
        draw_sidebar_row(ui, tab.label(), None, false, is_focused);
    }
}

fn draw_sidebar_row(
    ui: &mut egui::Ui,
    label: &str,
    count: Option<usize>,
    active: bool,
    focused: bool,
) -> egui::Response {
    let bg = if focused {
        theme::CARD_ACTIVE
    } else if active {
        theme::CARD
    } else {
        Color32::TRANSPARENT
    };
    let border = if focused {
        Stroke::new(2.0, theme::TERRACOTTA_FOCUS)
    } else if active {
        Stroke::new(1.0, theme::BORDER)
    } else {
        Stroke::NONE
    };

    let inner = Frame::new()
        .fill(bg)
        .stroke(border)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if active || focused {
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(4.0, 16.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(2),
                        theme::TERRACOTTA_FOCUS,
                    );
                }
                ui.label(
                    RichText::new(truncate_chars(label, 17))
                        .size(14.0)
                        .strong()
                        .color(if active || focused {
                            theme::SAND_TEXT
                        } else {
                            theme::DRIFTWOOD_MUTED
                        }),
                );
                if let Some(n) = count {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(n.to_string())
                                .size(12.0)
                                .color(theme::DRIFTWOOD_MUTED),
                        );
                    });
                }
            });
        });
    inner.response
}

fn draw_poster_grid(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    filtered: &[usize],
    nav: &NavState,
    textures: &TextureManager,
) {
    if filtered.is_empty() {
        ui.centered_and_justified(|ui| {
            let msg = if catalog.is_empty() {
                "No games cached yet.\nRun launcher_sync.py or press Start to open Aurora."
            } else {
                "No games match this category or letter filter."
            };
            ui.label(RichText::new(msg).size(18.0).color(theme::DRIFTWOOD_MUTED));
        });
        return;
    }

    let avail = ui.available_size();
    let gap = 12.0;
    let card_w = ((avail.x - gap * (GRID_COLS as f32 - 1.0)) / GRID_COLS as f32).floor().max(120.0);
    let card_h = ((avail.y - gap) / VISIBLE_ROWS as f32).floor().max(180.0);
    let label_h = 34.0;
    let poster_h = (card_h - label_h - 16.0).max(110.0);
    let poster_w = (poster_h * (180.0 / 248.0)).min(card_w - 14.0);

    for vis_row in 0..VISIBLE_ROWS {
        let row_idx = nav.scroll_row + vis_row;
        let row_start = row_idx * GRID_COLS;
        if row_start >= filtered.len() {
            break;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for col in 0..GRID_COLS {
                let pos = row_start + col;
                if let Some(&entry_idx) = filtered.get(pos)
                    && let Some(entry) = catalog.get(entry_idx)
                {
                    let focused = nav.zone == FocusZone::Grid && nav.grid_idx == pos;
                    draw_poster_card(
                        ui,
                        entry,
                        textures.poster(&entry.title_id),
                        Vec2::new(card_w, card_h),
                        Vec2::new(poster_w, poster_h),
                        focused,
                    );
                } else {
                    ui.allocate_exact_size(Vec2::new(card_w, card_h), egui::Sense::hover());
                }
            }
        });
        if vis_row + 1 < VISIBLE_ROWS {
            ui.add_space(gap);
        }
    }
}

fn draw_poster_card(
    ui: &mut egui::Ui,
    entry: &GameEntry,
    poster: Option<&egui::TextureHandle>,
    card_size: Vec2,
    poster_size: Vec2,
    focused: bool,
) {
    let border = if focused {
        Stroke::new(3.0, theme::TERRACOTTA_FOCUS)
    } else {
        Stroke::new(1.0, theme::BORDER)
    };
    let fill = if focused {
        theme::CARD_ACTIVE
    } else {
        theme::CARD
    };

    Frame::new()
        .fill(fill)
        .stroke(border)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.set_min_size(card_size - Vec2::splat(14.0));
            ui.set_max_size(card_size - Vec2::splat(14.0));
            ui.vertical_centered(|ui| {
                if let Some(tex) = poster {
                    ui.add(
                        egui::Image::from_texture(tex)
                            .fit_to_exact_size(poster_size)
                            .corner_radius(CornerRadius::same(4)),
                    );
                } else {
                    Frame::new()
                        .fill(theme::SIDEBAR)
                        .stroke(Stroke::new(1.0, theme::BORDER))
                        .corner_radius(CornerRadius::same(4))
                        .show(ui, |ui| {
                            ui.set_min_size(poster_size);
                            ui.set_max_size(poster_size);
                            ui.vertical_centered(|ui| {
                                ui.add_space(poster_size.y * 0.28);
                                ui.label(
                                    RichText::new(entry.monogram())
                                        .size(34.0)
                                        .strong()
                                        .color(theme::TERRACOTTA_FOCUS),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(entry.badge().label())
                                        .size(12.0)
                                        .color(theme::DRIFTWOOD_MUTED),
                                );
                            });
                        });
                }

                ui.add_space(4.0);
                let star = if entry.favorite { "* " } else { "" };
                let title_line = format!("{star}{}", truncate_chars(&entry.title_name, 19));
                ui.label(
                    RichText::new(title_line)
                        .size(13.0)
                        .strong()
                        .color(if focused {
                            theme::SAND_TEXT
                        } else {
                            theme::DRIFTWOOD_MUTED
                        }),
                );
            });
        });
}

fn draw_hero_strip(
    ui: &mut egui::Ui,
    entry: Option<&GameEntry>,
    textures: &TextureManager,
) {
    let Some(entry) = entry else {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label(
                RichText::new("Select a game to view details and launch")
                    .size(16.0)
                    .color(theme::DRIFTWOOD_MUTED),
            );
        });
        return;
    };

    let backdrop_size = Vec2::new(212.0, 118.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let text_w = (ui.available_width() - backdrop_size.x - 24.0).max(320.0);
            ui.set_max_width(text_w);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(truncate_chars(&entry.title_name, 48))
                        .size(21.0)
                        .strong()
                        .color(theme::SAND_TEXT),
                );
                ui.add_space(8.0);
                draw_chip(
                    ui,
                    entry.badge().label(),
                    theme::CARD,
                    theme::TERRACOTTA_FOCUS,
                );
                if let Some(mp_badge) = entry.local_players_badge() {
                    draw_chip(ui, &mp_badge, theme::SAGE_BG, theme::SAGE_FG);
                }
                if entry.favorite {
                    draw_chip(ui, "Favorite", theme::AMBER_BG, theme::AMBER_FG);
                }
            });

            let genres: Vec<&str> = entry.genres().into_iter().map(|g| g.label()).collect();
            let genre_str = if genres.is_empty() {
                String::from("Uncategorized")
            } else {
                genres.join(", ")
            };
            let year = if entry.release_year.is_empty() {
                "-"
            } else {
                &entry.release_year
            };
            let pub_name = if entry.publisher.is_empty() {
                "Unknown publisher"
            } else {
                &entry.publisher
            };
            ui.label(
                RichText::new(format!(
                    "{year}  |  {pub_name}  |  {genre_str}  |  ID {}",
                    entry.title_id
                ))
                .size(14.0)
                .color(theme::TERRACOTTA_FOCUS),
            );

            ui.add_space(4.0);
            let desc = if entry.description.is_empty() {
                "Press A to launch this title, X to favorite, or B to switch categories."
            } else {
                &entry.description
            };
            ui.label(
                RichText::new(truncate_chars(desc, 175))
                    .size(13.0)
                    .color(theme::DRIFTWOOD_MUTED),
            );
        });

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some(tex) = textures.backdrop(&entry.title_id) {
                ui.add(
                    egui::Image::from_texture(tex)
                        .fit_to_exact_size(backdrop_size)
                        .corner_radius(CornerRadius::same(6)),
                );
            } else {
                Frame::new()
                    .fill(theme::CARD)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(CornerRadius::same(6))
                    .show(ui, |ui| {
                        ui.set_min_size(backdrop_size);
                        ui.set_max_size(backdrop_size);
                        ui.centered_and_justified(|ui| {
                            ui.label(
                                RichText::new("A: Play Game")
                                    .size(15.0)
                                    .strong()
                                    .color(theme::SAND_TEXT),
                            );
                        });
                    });
            }
        });
    });
}

fn draw_lock_modal(
    ctx: &egui::Context,
    nav: &NavState,
    guard: &GuardStatus,
    notice: Option<&str>,
) {
    egui::Area::new(egui::Id::new("shelf_lock_modal"))
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::new()
                .fill(theme::HEADER_DETAIL)
                .stroke(Stroke::new(2.0, theme::TERRACOTTA_FOCUS))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(22))
                .show(ui, |ui| {
                    ui.set_min_width(480.0);
                    ui.label(
                        RichText::new("Screen Time Locked")
                            .size(22.0)
                            .strong()
                            .color(theme::RUST_BG),
                    );
                    ui.add_space(6.0);
                    let msg = if guard.message.is_empty() {
                        "Ask a parent for more time or enter the 4-button parent PIN."
                    } else {
                        &guard.message
                    };
                    ui.label(RichText::new(msg).size(15.0).color(theme::SAND_TEXT));
                    if let Some(n) = notice {
                        ui.add_space(4.0);
                        ui.label(RichText::new(n).size(14.0).color(theme::TERRACOTTA_FOCUS));
                    }
                    ui.add_space(12.0);

                    if nav.entering_pin {
                        let stars = "* ".repeat(nav.pin_buffer.len());
                        let blanks = "_ ".repeat(4usize.saturating_sub(nav.pin_buffer.len()));
                        ui.label(
                            RichText::new(format!("Enter 4-Button Parent PIN:  {stars}{blanks}"))
                                .size(18.0)
                                .strong()
                                .color(theme::SAND_TEXT),
                        );
                        if let Some(ref err) = nav.pin_error {
                            ui.add_space(4.0);
                            ui.label(RichText::new(err).size(14.0).color(theme::RUST_BG));
                        }
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Use D-Pad / X / Y / LB / RB / LT / RT  |  B: Cancel")
                                .size(13.0)
                                .color(theme::DRIFTWOOD_MUTED),
                        );
                    } else {
                        let choices = LockChoice::choices(guard.paused);
                        for (idx, choice) in choices.iter().enumerate() {
                            let active = idx == nav.lock_cursor;
                            Frame::new()
                                .fill(if active { theme::CARD_ACTIVE } else { theme::CARD })
                                .stroke(if active {
                                    Stroke::new(2.0, theme::TERRACOTTA_FOCUS)
                                } else {
                                    Stroke::new(1.0, theme::BORDER)
                                })
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.set_width(450.0);
                                    ui.label(
                                        RichText::new(choice.label())
                                            .size(16.0)
                                            .strong()
                                            .color(theme::SAND_TEXT),
                                    );
                                });
                            ui.add_space(4.0);
                        }
                    }
                });
        });
}

fn draw_error_modal(ctx: &egui::Context, message: &str) {
    egui::Area::new(egui::Id::new("shelf_error_modal"))
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            Frame::new()
                .fill(theme::HEADER_DETAIL)
                .stroke(Stroke::new(2.0, theme::RUST_FG))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::same(22))
                .show(ui, |ui| {
                    ui.set_min_width(480.0);
                    ui.label(
                        RichText::new("Could Not Launch Title")
                            .size(22.0)
                            .strong()
                            .color(theme::RUST_BG),
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new(message).size(15.0).color(theme::SAND_TEXT));
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Press A or B to return to Shelf")
                            .size(14.0)
                            .color(theme::TERRACOTTA_FOCUS),
                    );
                });
        });
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}
