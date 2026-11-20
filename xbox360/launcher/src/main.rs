//! Shelf: Grounded-Earth Plex-style Xbox 360 game launcher.

#![no_main]

mod assets;
mod launch;
mod net_worker;
mod theme;
mod ui;

use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use launcher_core::catalog::Catalog;
use launcher_core::guard::{
    GuardStatus, LauncherConfig, VK_PAD_DPAD_DOWN, VK_PAD_DPAD_LEFT, VK_PAD_DPAD_RIGHT,
    VK_PAD_DPAD_UP,
};
use launcher_core::launch_plan::{LaunchTarget, plan_aurora_launch, plan_game_launch};
use launcher_core::nav::{FocusZone, InputAction, NavEvent, NavState};
use xbox360::xam::input;
use xbox360_draw::draw::DrawList;
use xbox360_egui::Painter;
use xbox360_gpu::Gpu;
use xbox360_sys::xam::{Keystroke, KeystrokeFlags, PadKey};
use xbox360_video::Display;

use crate::assets::TextureManager;
use crate::net_worker::{NetWorker, WorkerCmd, WorkerMsg};

const UI_STACK_BYTES: usize = 1 << 20;
const PIXELS_PER_POINT: f32 = 1.0;

const CFG_SEARCH_PATHS: [&str; 4] = [
    r"\Device\Harddisk0\Partition1\PeerPressure\Launcher\screentime.cfg",
    r"\Device\Harddisk0\Partition1\PeerPressure\SideLoading\screentime.cfg",
    r"\Device\Harddisk0\Partition1\screentime.cfg",
    r"\Device\Mass0\screentime.cfg",
];

enum PendingAction {
    Launch {
        target: LaunchTarget,
        played_tid: Option<String>,
    },
    Exit,
}

xbox360::sys::bridged!(#[unsafe(no_mangle)] pub _start, main);

extern "C" fn main(_context: u32) -> u32 {
    let join = std::thread::Builder::new()
        .stack_size(UI_STACK_BYTES)
        .spawn(run_app);
    match join {
        Ok(handle) => {
            if handle.join().is_err() {
                println!("shelf_launcher: UI thread panicked");
            }
        }
        Err(error) => {
            println!("shelf_launcher: failed to spawn UI thread: {error}");
        }
    }
    xbox360::title::terminate()
}

fn load_config() -> LauncherConfig {
    for path in CFG_SEARCH_PATHS {
        if let Ok(raw) = fs::read_to_string(path) {
            println!("shelf: loaded config from {path}");
            return LauncherConfig::parse_cfg(&raw);
        }
    }
    println!("shelf: no screentime.cfg found; starting in offline cache mode");
    LauncherConfig::default()
}

fn load_cached_catalog(cache_dir: &str) -> Catalog {
    let base = Path::new(cache_dir);
    let mut cat = match fs::read_to_string(base.join("catalog.tsv")) {
        Ok(raw) => Catalog::parse_tsv(&raw).unwrap_or_else(|e| {
            println!("shelf: local catalog invalid: {e}");
            Catalog::empty()
        }),
        Err(_) => Catalog::empty(),
    };
    if let Ok(us) = fs::read_to_string(base.join("userstate.tsv")) {
        cat.apply_userstate_tsv(&us);
    }
    cat
}

fn persist_userstate(cache_dir: &str, catalog: &Catalog) {
    let path = Path::new(cache_dir).join("userstate.tsv");
    let _ = fs::write(path, catalog.serialize_userstate_tsv());
}

fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1)
        .max(1)
}

fn map_pad_key(key: PadKey) -> Option<InputAction> {
    match key {
        PadKey::DPadUp | PadKey::LeftStickUp => Some(InputAction::Up),
        PadKey::DPadDown | PadKey::LeftStickDown => Some(InputAction::Down),
        PadKey::DPadLeft | PadKey::LeftStickLeft => Some(InputAction::Left),
        PadKey::DPadRight | PadKey::LeftStickRight => Some(InputAction::Right),
        PadKey::A => Some(InputAction::A),
        PadKey::B => Some(InputAction::B),
        PadKey::X => Some(InputAction::X),
        PadKey::Y => Some(InputAction::Y),
        PadKey::LeftShoulder => Some(InputAction::LB),
        PadKey::RightShoulder => Some(InputAction::RB),
        PadKey::LeftTrigger => Some(InputAction::LT),
        PadKey::RightTrigger => Some(InputAction::RT),
        PadKey::Start => Some(InputAction::Start),
        PadKey::Back => Some(InputAction::Back),
        _ => None,
    }
}

fn map_pin_vk(key: PadKey, raw_vk: u16) -> Option<u16> {
    match key {
        PadKey::DPadUp | PadKey::LeftStickUp => Some(VK_PAD_DPAD_UP),
        PadKey::DPadDown | PadKey::LeftStickDown => Some(VK_PAD_DPAD_DOWN),
        PadKey::DPadLeft | PadKey::LeftStickLeft => Some(VK_PAD_DPAD_LEFT),
        PadKey::DPadRight | PadKey::LeftStickRight => Some(VK_PAD_DPAD_RIGHT),
        PadKey::X | PadKey::Y | PadKey::RightShoulder | PadKey::LeftShoulder
        | PadKey::LeftTrigger | PadKey::RightTrigger => Some(raw_vk),
        _ => None,
    }
}

fn process_keystroke(
    ks: &Keystroke,
    nav: &mut NavState,
    catalog: &mut Catalog,
    filtered: &[usize],
    guard: &GuardStatus,
    pin_override: bool,
    cfg: &LauncherConfig,
) -> NavEvent {
    let flags = ks.flags;
    if !flags.contains(KeystrokeFlags::KEY_DOWN) && !flags.contains(KeystrokeFlags::REPEAT) {
        return NavEvent::None;
    }
    let Some(pad_key) = PadKey::from_raw(ks.virtual_key) else {
        return NavEvent::None;
    };

    if nav.zone == FocusZone::LockModal && nav.entering_pin {
        if matches!(pad_key, PadKey::B | PadKey::Back) {
            return nav.apply_input(
                InputAction::B,
                catalog,
                filtered,
                guard.is_launch_blocked(pin_override),
                guard.paused,
            );
        }
        if !flags.contains(KeystrokeFlags::REPEAT)
            && let Some(vk) = map_pin_vk(pad_key, ks.virtual_key)
        {
            return nav.submit_pin_key(vk, &cfg.parent_pin);
        }
        return NavEvent::None;
    }

    let Some(action) = map_pad_key(pad_key) else {
        return NavEvent::None;
    };
    nav.apply_input(
        action,
        catalog,
        filtered,
        guard.is_launch_blocked(pin_override),
        guard.paused,
    )
}

fn run_app() {
    let cfg = load_config();
    let mut catalog = load_cached_catalog(&cfg.cache_dir);
    let mut nav = NavState::default();
    let mut guard = GuardStatus::default();
    let mut pin_override = false;
    let mut online_catalog = cfg.server_ip.is_some();
    let mut notice: Option<(String, Instant)> = None;

    loop {
        let mut display = match Display::new() {
            Ok(d) => d,
            Err(error) => {
                println!("shelf: Display::new failed: {error}");
                return;
            }
        };
        let mut gpu = match Gpu::new() {
            Ok(g) => g,
            Err(error) => {
                println!("shelf: Gpu::new failed: {error}");
                return;
            }
        };

        let size = display.size();
        let context = egui::Context::default();
        theme::apply_theme(&context);
        let mut painter = Painter::new();
        let mut list = DrawList::new();
        let mut textures = TextureManager::default();
        let worker = NetWorker::spawn(cfg.clone());
        let start = Instant::now();

        let pending_action = loop {
            while let Some(msg) = worker.try_recv() {
                match msg {
                    WorkerMsg::CatalogLoaded { catalog: next_cat } => {
                        let prev_user = catalog.serialize_userstate_tsv();
                        let mut merged = next_cat;
                        merged.apply_userstate_tsv(&prev_user);
                        catalog = merged;
                        online_catalog = true;
                        textures.reset_failures();
                    }
                    WorkerMsg::UserstateLoaded(us) => {
                        catalog.apply_userstate_tsv(&us);
                    }
                    WorkerMsg::GuardUpdated(next_guard) => {
                        guard = next_guard;
                    }
                    WorkerMsg::PosterReady { title_id, image } => {
                        textures.ingest_poster(&context, &title_id, image);
                    }
                    WorkerMsg::BackdropReady { title_id, image } => {
                        textures.ingest_backdrop(&context, &title_id, image);
                    }
                    WorkerMsg::Notice(msg) => {
                        notice = Some((msg, Instant::now()));
                    }
                }
            }

            if let Some((_, ts)) = notice.as_ref()
                && ts.elapsed().as_secs() >= 4
            {
                notice = None;
            }

            let mut filtered = catalog.filtered_indices_with_mp(
                nav.active_filter_tab(),
                nav.sort_mode,
                nav.letter_filter,
                nav.local_mp_only,
            );
            nav.clamp_grid(filtered.len());

            let mut chosen_launch: Option<PendingAction> = None;
            while let Some(ks) = input::next_keystroke() {
                let ev = process_keystroke(
                    &ks,
                    &mut nav,
                    &mut catalog,
                    &filtered,
                    &guard,
                    pin_override,
                    &cfg,
                );
                match ev {
                    NavEvent::None => {}
                    NavEvent::ToggleFavorite { title_id, favorite } => {
                        persist_userstate(&cfg.cache_dir, &catalog);
                        worker.send(WorkerCmd::SetFavorite { title_id, favorite });
                        filtered = catalog.filtered_indices_with_mp(
                            nav.active_filter_tab(),
                            nav.sort_mode,
                            nav.letter_filter,
                            nav.local_mp_only,
                        );
                        nav.clamp_grid(filtered.len());
                    }
                    NavEvent::RequestMoreTime(minutes) => {
                        worker.send(WorkerCmd::RequestMoreTime { minutes });
                    }
                    NavEvent::RequestResume => {
                        worker.send(WorkerCmd::RequestResume);
                    }
                    NavEvent::PinUnlocked => {
                        pin_override = true;
                        notice = Some((String::from("Parent PIN accepted"), Instant::now()));
                        if let Some(entry_idx) = nav.take_pending_launch()
                            && let Some(entry) = catalog.get(entry_idx)
                        {
                            match plan_game_launch(entry) {
                                Ok(target) => {
                                    chosen_launch = Some(PendingAction::Launch {
                                        target,
                                        played_tid: Some(entry.title_id.clone()),
                                    });
                                    break;
                                }
                                Err(err) => nav.set_launch_error(err),
                            }
                        }
                    }
                    NavEvent::LaunchGame(entry_idx) => {
                        if let Some(entry) = catalog.get(entry_idx) {
                            match plan_game_launch(entry) {
                                Ok(target) => {
                                    chosen_launch = Some(PendingAction::Launch {
                                        target,
                                        played_tid: Some(entry.title_id.clone()),
                                    });
                                    break;
                                }
                                Err(err) => nav.set_launch_error(err),
                            }
                        }
                    }
                    NavEvent::LaunchAurora => match plan_aurora_launch(&cfg.aurora_path) {
                        Ok(target) => {
                            chosen_launch = Some(PendingAction::Launch {
                                target,
                                played_tid: None,
                            });
                            break;
                        }
                        Err(err) => nav.set_launch_error(err),
                    },
                }
            }

            if let Some(action) = chosen_launch {
                break action;
            }

            let selected_entry = filtered.get(nav.grid_idx).and_then(|&i| catalog.get(i));
            let (poster_jobs, backdrop_job) = textures.plan_loads(
                &filtered,
                &catalog,
                nav.scroll_row,
                selected_entry,
                online_catalog,
            );
            for (title_id, fetch_remote) in poster_jobs {
                worker.send(WorkerCmd::LoadPoster {
                    title_id,
                    fetch_remote,
                });
            }
            if let Some((title_id, fetch_remote)) = backdrop_job {
                worker.send(WorkerCmd::LoadBackdrop {
                    title_id,
                    fetch_remote,
                });
            }

            let raw_input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(size.width as f32, size.height as f32),
                )),
                time: Some(start.elapsed().as_secs_f64()),
                ..egui::RawInput::default()
            };

            let notice_text = notice.as_ref().map(|(s, _)| s.as_str());
            let output = context.run(raw_input, |ctx| {
                ui::draw_launcher_ui(
                    ctx,
                    &catalog,
                    &filtered,
                    &nav,
                    &guard,
                    pin_override,
                    &textures,
                    notice_text,
                );
            });
            let primitives = context.tessellate(output.shapes, output.pixels_per_point);
            list.clear();
            list.set_clear_color(Some(theme::CLEAR_BG));
            if let Err(error) = painter.paint(
                &mut gpu,
                &mut list,
                &output.textures_delta,
                &primitives,
                PIXELS_PER_POINT,
            ) {
                println!("shelf: painter error: {error}");
                break PendingAction::Exit;
            }
            if let Err(error) = gpu.render(&mut display, &list) {
                println!("shelf: gpu render error: {error}");
                break PendingAction::Exit;
            }
        };

        match pending_action {
            PendingAction::Exit => {
                worker.shutdown();
                textures.clear_all();
                drop(painter);
                drop(context);
                drop(list);
                drop(gpu);
                drop(display);
                return;
            }
            PendingAction::Launch { target, played_tid } => {
                if let Some(ref tid) = played_tid {
                    catalog.record_played(tid, unix_timestamp_now());
                    persist_userstate(&cfg.cache_dir, &catalog);
                    worker.send(WorkerCmd::RecordPlayed {
                        title_id: tid.clone(),
                    });
                }
                worker.shutdown();
                textures.clear_all();
                drop(painter);
                drop(context);
                drop(list);
                drop(gpu);
                drop(display);

                if let Err(err) = launch::execute_launch(&target) {
                    println!("shelf: launch failed: {err}");
                    nav.set_launch_error(err);
                    continue;
                }
                return;
            }
        }
    }
}
