//! Shelf: Grounded-Earth Plex-style Xbox 360 game launcher.

#![no_main]

mod assets;
mod debug_server;
mod diag;
mod launch;
mod net_worker;
mod theme;
mod ui;
mod xnet;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use launcher_core::catalog::Catalog;
use launcher_core::debug_proto::RenderOverride;
use launcher_core::guard::{
    GuardStatus, LauncherConfig, VK_PAD_DPAD_DOWN, VK_PAD_DPAD_LEFT, VK_PAD_DPAD_RIGHT,
    VK_PAD_DPAD_UP,
};
use launcher_core::launch_plan::{LaunchTarget, plan_aurora_launch, plan_game_launch};
use launcher_core::nav::{FocusZone, InputAction, NavEvent, NavState};
use xbox360::xam::input;
use xbox360_draw::draw::{DrawList, Rgba8};
use xbox360_egui::Painter;
use xbox360_gpu::Gpu;
use xbox360_sys::mmio::GpuRegisters;
use xbox360_sys::xam::{Keystroke, KeystrokeFlags, PadKey};
use xbox360_sys::xboxkrnl::{
    DisplayInformation, VdGetCurrentDisplayInformation, VdSetDisplayMode,
};
use xbox360_video::{Display, FrameSize, FrameView};

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
    diag::reset_log();
    std::panic::set_hook(Box::new(|info| {
        match diag::handle_panic(&format!("{info}")) {
            diag::PanicAction::ParkWorkerOrDebug => {
                if diag::debug_panicked() {
                    debug_server::mark_down_on_panic();
                }
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            diag::PanicAction::ParkUiIfDebug => {
                if debug_server::is_spawned() && !diag::debug_panicked() {
                    loop {
                        std::thread::sleep(Duration::from_secs(3600));
                    }
                }
                xbox360::title::return_to_dashboard();
            }
            diag::PanicAction::Unregistered => {
                xbox360::title::return_to_dashboard();
            }
        }
    }));
    diag::stage("01_main_enter", "entered _start -> main");
    let join = std::thread::Builder::new()
        .stack_size(UI_STACK_BYTES)
        .spawn(run_app);
    match join {
        Ok(handle) => {
            if handle.join().is_err() {
                diag::stage("01_main_join_panic", "UI thread panicked");
            } else {
                diag::stage("01_main_join_ok", "UI thread exited cleanly");
            }
        }
        Err(error) => {
            diag::stage("01_main_spawn_err", &format!("{error}"));
        }
    }
    xbox360::title::return_to_dashboard()
}

fn load_config() -> (LauncherConfig, &'static str) {
    for path in CFG_SEARCH_PATHS {
        if let Ok(raw) = fs::read_to_string(path) {
            println!("shelf: loaded config from {path}");
            return (LauncherConfig::parse_cfg(&raw), path);
        }
    }
    println!("shelf: no screentime.cfg found; starting in offline cache mode");
    (LauncherConfig::default(), "default")
}

fn query_vd_info() -> (DisplayInformation, u32, u32) {
    let mut info = DisplayInformation::new();
    // SAFETY: `info` is an aligned 0x58-byte block owned by this stack frame.
    unsafe { VdGetCurrentDisplayInformation(info.as_mut_ptr()) };
    let regs = GpuRegisters::get();
    let ctrl = regs.display_graphics_control().read();
    let addr = regs.display_primary_surface().read();
    (info, ctrl, addr)
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

fn park_on_fatal(
    cfg: &LauncherConfig,
    debug_srv: &mut Option<debug_server::DebugServer>,
    reason: &str,
) {
    diag::stage(
        "fatal_park",
        &format!("{reason}; debug server active on tcp/7331 (send 'dash'/'aurora' or press B)"),
    );
    loop {
        while let Some(ks) = input::next_keystroke() {
            if ks.flags.contains(KeystrokeFlags::KEY_DOWN)
                && matches!(PadKey::from_raw(ks.virtual_key), Some(PadKey::B | PadKey::Start))
            {
                if let Some(srv) = debug_srv.take() {
                    srv.shutdown();
                }
                return;
            }
        }
        match debug_server::take_remote_action() {
            1 => {
                debug_server::acknowledge_remote_action();
                if let Some(srv) = debug_srv.take() {
                    srv.shutdown();
                }
                return;
            }
            2 => {
                if let Ok(target) = plan_aurora_launch(&cfg.aurora_path) {
                    debug_server::acknowledge_remote_action();
                    if let Some(srv) = debug_srv.take() {
                        srv.shutdown();
                    }
                    let _ = launch::execute_launch(&target);
                    return;
                }
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_app() {
    diag::register_thread(diag::ThreadRole::Ui);
    diag::stage("02_ui_thread_start", "1MB stack running");
    let (cfg, cfg_src) = load_config();
    if let Some(ip) = cfg.server_ip {
        diag::set_server(ip, cfg.server_port);
        diag::stage(
            "03_cfg_loaded",
            &format!("src={cfg_src} cache={} port={}", cfg.cache_dir, cfg.server_port),
        );
    } else {
        diag::stage("03_cfg_loaded", &format!("offline src={cfg_src}"));
    }
    let (link, xnet_rc, wsa_rc) = xnet::init_once();
    diag::stage(
        "04_xnet_init",
        &format!("link=0x{link:x} xnet={xnet_rc} wsa={wsa_rc}"),
    );

    let (mut vd_info, ctrl, primary_addr) = query_vd_info();
    diag::stage(
        "05_vd_info",
        &format!(
            "surf={}x{} win={},{},{},{} scaled={}x{} cs={} ctrl=0x{ctrl:08x} addr=0x{primary_addr:08x}",
            vd_info.surface_width,
            vd_info.surface_height,
            vd_info.left,
            vd_info.top,
            vd_info.right,
            vd_info.bottom,
            vd_info.scaled_width,
            vd_info.scaled_height,
            vd_info.colorspace,
        ),
    );

    let mut catalog = load_cached_catalog(&cfg.cache_dir);
    diag::stage("06_catalog_loaded", &format!("games={}", catalog.entries().len()));

    let mut debug_srv = Some(debug_server::DebugServer::spawn(cfg.aurora_path.clone()));

    let mut nav = NavState::default();
    let mut guard = GuardStatus::default();
    let mut pin_override = false;
    let mut online_catalog = cfg.server_ip.is_some();
    let mut notice: Option<(String, Instant)> = None;

    loop {
        diag::stage("07_display_new_begin", "calling Display::new");
        let mut display = match Display::new() {
            Ok(d) => d,
            Err(error) => {
                diag::stage("07_display_new_err", &format!("{error}"));
                if vd_info.surface_width == 0 || vd_info.surface_height == 0 {
                    // SAFETY: re-initializes video mode if dashboard left surface unset.
                    unsafe { VdSetDisplayMode(0) };
                    let (next_vd, next_ctrl, next_addr) = query_vd_info();
                    vd_info = next_vd;
                    diag::stage(
                        "07_display_retry_vdsetmode",
                        &format!(
                            "surf={}x{} ctrl=0x{next_ctrl:08x} addr=0x{next_addr:08x}",
                            vd_info.surface_width, vd_info.surface_height
                        ),
                    );
                    match Display::new() {
                        Ok(d) => d,
                        Err(err2) => {
                            diag::stage("07_display_fatal", &format!("{err2}"));
                            park_on_fatal(&cfg, &mut debug_srv, &format!("Display::new: {err2}"));
                            return;
                        }
                    }
                } else {
                    diag::stage("07_display_fatal", &format!("{error}"));
                    park_on_fatal(&cfg, &mut debug_srv, &format!("Display::new: {error}"));
                    return;
                }
            }
        };

        let size = display.size();
        diag::stage("07_display_new_ok", &format!("size={}x{}", size.width, size.height));

        diag::stage("08a_cpu_splash1_begin", "terracotta 320x180 CPU scanout test");
        let splash = vec![0x004B_7AC8u32; 320 * 180];
        if let Some(frame) = FrameView::new(&splash, FrameSize { width: 320, height: 180 }, 320) {
            display.present(frame);
            diag::stage("08a_cpu_splash1_ok", "submit_flip #1 returned");
            display.present(frame);
            diag::stage("08b_cpu_splash2_ok", "wait_for_flip + submit_flip #2 returned");
        }
        drop(splash);

        diag::stage("09_gpu_new_begin", "calling Gpu::new");
        let mut gpu = match Gpu::new() {
            Ok(g) => g,
            Err(error) => {
                diag::stage("09_gpu_new_err", &format!("{error}"));
                drop(display);
                park_on_fatal(&cfg, &mut debug_srv, &format!("Gpu::new: {error}"));
                return;
            }
        };
        diag::stage("09_gpu_new_ok", "GPU arenas and shaders ready");

        let context = egui::Context::default();
        theme::apply_theme(&context);
        let mut painter = Painter::new();
        let mut list = DrawList::new();
        let mut textures = TextureManager::default();
        let worker = NetWorker::spawn(cfg.clone());
        diag::stage("10_worker_spawned", "network/art worker thread started");
        let start = Instant::now();
        let mut frame_num: u32 = 0;
        let mut gpu_frame_num: u32 = 0;
        let mut cpu_fill: Option<(u32, Vec<u32>)> = None;

        let pending_action = loop {
            diag::record_frame(frame_num);

            match debug_server::take_remote_action() {
                1 => {
                    debug_server::acknowledge_remote_action();
                    break PendingAction::Exit;
                }
                2 => match plan_aurora_launch(&cfg.aurora_path) {
                    Ok(target) => {
                        debug_server::acknowledge_remote_action();
                        break PendingAction::Launch {
                            target,
                            played_tid: None,
                        };
                    }
                    Err(err) => nav.set_launch_error(err),
                },
                _ => {}
            }

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

            let mut is_gpu_frame = true;
            match debug_server::render_override() {
                RenderOverride::CpuSolid(r, g, b) => {
                    is_gpu_frame = false;
                    let pixel = (u32::from(b) << 16) | (u32::from(g) << 8) | u32::from(r);
                    let buf = match cpu_fill {
                        Some((ref mut prev_px, ref mut vec)) => {
                            if *prev_px != pixel {
                                vec.fill(pixel);
                                *prev_px = pixel;
                            }
                            vec.as_slice()
                        }
                        None => {
                            cpu_fill = Some((pixel, vec![pixel; 320 * 180]));
                            cpu_fill.as_ref().map(|(_, v)| v.as_slice()).unwrap_or(&[])
                        }
                    };
                    if let Some(frame) =
                        FrameView::new(buf, FrameSize { width: 320, height: 180 }, 320)
                    {
                        display.present(frame);
                    }
                }
                RenderOverride::GpuSolid(r, g, b) => {
                    list.clear();
                    list.set_clear_color(Some(Rgba8::new(r, g, b, 0xFF)));
                    if gpu_frame_num == 0 {
                        diag::stage("13_frame1_render_begin", "calling gpu.render (solid override)");
                    }
                    if let Err(error) = gpu.render(&mut display, &list) {
                        diag::stage("13_frame1_render_err", &format!("frame={frame_num} err={error}"));
                        break PendingAction::Exit;
                    }
                }
                RenderOverride::Normal => {
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
                    let shapes_len = output.shapes.len();
                    let tex_set_len = output.textures_delta.set.len();
                    let primitives = context.tessellate(output.shapes, output.pixels_per_point);
                    if gpu_frame_num == 0 {
                        diag::stage(
                            "11_frame1_egui_ok",
                            &format!(
                                "shapes={shapes_len} prims={} tex_set={tex_set_len}",
                                primitives.len()
                            ),
                        );
                    }
                    list.clear();
                    list.set_clear_color(Some(theme::CLEAR_BG));
                    if let Err(error) = painter.paint(
                        &mut gpu,
                        &mut list,
                        &output.textures_delta,
                        &primitives,
                        PIXELS_PER_POINT,
                    ) {
                        diag::stage("frame_paint_err", &format!("frame={frame_num} err={error}"));
                        break PendingAction::Exit;
                    }
                    if gpu_frame_num == 0 {
                        diag::stage(
                            "12_frame1_paint_ok",
                            &format!("tris={} cmds={}", list.triangles().len(), list.commands().len()),
                        );
                        diag::stage("13_frame1_render_begin", "calling gpu.render");
                    }
                    if let Err(error) = gpu.render(&mut display, &list) {
                        diag::stage("13_frame1_render_err", &format!("frame={frame_num} err={error}"));
                        break PendingAction::Exit;
                    }
                }
            }
            if is_gpu_frame {
                if gpu_frame_num == 0 {
                    diag::stage("13_frame1_render_ok", "first GPU frame submitted");
                } else if gpu_frame_num == 1 {
                    diag::stage("14_frame2_render_ok", "GPU flip #1 latched cleanly");
                } else if gpu_frame_num == 9 {
                    diag::stage("15_frame10_ok", "steady-state rendering active");
                }
                gpu_frame_num = gpu_frame_num.saturating_add(1);
            }
            frame_num = frame_num.saturating_add(1);
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
                if let Some(srv) = debug_srv.take() {
                    srv.shutdown();
                }
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
                if let Some(srv) = debug_srv.take() {
                    srv.shutdown();
                }

                if let Err(err) = launch::execute_launch(&target) {
                    diag::stage("launch_err", &err);
                    nav.set_launch_error(err);
                    (vd_info, _, _) = query_vd_info();
                    debug_srv = Some(debug_server::DebugServer::spawn(cfg.aurora_path.clone()));
                    continue;
                }
                return;
            }
        }
    }
}
