//! Background worker thread for Hdd1 cache reads, xbox-guard LAN HTTP sync, and JPEG decoding.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use launcher_core::catalog::Catalog;
use launcher_core::guard::{
    GuardStatus, LauncherConfig, backdrop_path, catalog_path, favorite_path, format_get,
    format_post, heartbeat_path, parse_heartbeat_body, played_path, poster_path,
    request_resume_path, request_time_path, userstate_path,
};

use crate::assets::DecodedRgba;
use crate::diag;
use crate::xnet;

const LAUNCHER_TITLE_ID_HEX: &str = "58584C43";
const MAX_TSV_BYTES: usize = 4 * 1024 * 1024;
const MAX_JPEG_BYTES: usize = 1024 * 1024;

pub enum WorkerCmd {
    LoadPoster { title_id: String, fetch_remote: bool },
    LoadBackdrop { title_id: String, fetch_remote: bool },
    SetFavorite { title_id: String, favorite: bool },
    RecordPlayed { title_id: String },
    RequestMoreTime { minutes: u32 },
    RequestResume,
    Shutdown,
}

pub enum WorkerMsg {
    CatalogLoaded { catalog: Catalog },
    UserstateLoaded(String),
    GuardUpdated(GuardStatus),
    PosterReady { title_id: String, image: Option<DecodedRgba> },
    BackdropReady { title_id: String, image: Option<DecodedRgba> },
    Notice(String),
}

pub struct NetWorker {
    cmd_tx: Sender<WorkerCmd>,
    msg_rx: Receiver<WorkerMsg>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl NetWorker {
    pub fn spawn(cfg: LauncherConfig) -> Self {
        diag::clear_worker_panicked();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name(String::from("shelf-worker"))
            .stack_size(256 * 1024)
            .spawn(move || worker_loop(cfg, cmd_rx, msg_tx, worker_stop))
            .ok();
        Self {
            cmd_tx,
            msg_rx,
            stop,
            join,
        }
    }

    pub fn send(&self, cmd: WorkerCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn try_recv(&self) -> Option<WorkerMsg> {
        self.msg_rx.try_recv().ok()
    }

    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
        if let Some(handle) = self.join.take()
            && !diag::worker_panicked()
        {
            let _ = handle.join();
        }
    }
}

fn ensure_cache_dirs(cache_dir: &str) {
    let base = Path::new(cache_dir);
    let _ = fs::create_dir_all(base.join("posters"));
    let _ = fs::create_dir_all(base.join("backdrops"));
}

fn endpoint(cfg: &LauncherConfig) -> Option<([u8; 4], u16, String)> {
    let ip = cfg.server_ip?;
    let host_hdr = cfg.server_host_header()?;
    Some((ip, cfg.server_port, host_hdr))
}

fn http_exchange(
    ip: [u8; 4],
    port: u16,
    req: &[u8],
    max_body_bytes: usize,
) -> Result<(u16, Vec<u8>), String> {
    xnet::http_exchange(ip, port, req, max_body_bytes, 1500)
}

fn decode_image_bytes(raw: &[u8]) -> Option<DecodedRgba> {
    let decoded = xbox360_image::decode(raw).ok()?;
    let width = decoded.width().get() as usize;
    let height = decoded.height().get() as usize;
    let mut pixels = Vec::with_capacity(width * height * 4);
    for &px in decoded.pixels() {
        pixels.extend_from_slice(&[px.red(), px.green(), px.blue(), px.alpha()]);
    }
    Some(DecodedRgba {
        width,
        height,
        pixels,
    })
}

fn load_or_fetch_art(
    cache_dir: &str,
    subdir: &str,
    title_id: &str,
    remote_path: String,
    fetch_remote: bool,
    ep: Option<&([u8; 4], u16, String)>,
) -> Option<DecodedRgba> {
    let disk_path = Path::new(cache_dir).join(subdir).join(format!("{title_id}.jpg"));
    if let Ok(bytes) = fs::read(&disk_path)
        && let Some(img) = decode_image_bytes(&bytes)
    {
        return Some(img);
    }

    if !fetch_remote {
        return None;
    }
    let (ip, port, host_hdr) = ep?;
    let req = format_get(&remote_path, host_hdr);
    let (code, body) = http_exchange(*ip, *port, &req, MAX_JPEG_BYTES).ok()?;
    if code != 200 || body.is_empty() {
        return None;
    }
    let decoded = decode_image_bytes(&body)?;
    let _ = fs::write(&disk_path, &body);
    Some(decoded)
}

fn poll_heartbeat(ep: &([u8; 4], u16, String)) -> GuardStatus {
    let req = format_get(&heartbeat_path(LAUNCHER_TITLE_ID_HEX), &ep.2);
    match http_exchange(ep.0, ep.1, &req, 64 * 1024) {
        Ok((200, body)) => match std::str::from_utf8(&body) {
            Ok(text) => parse_heartbeat_body(text).unwrap_or_default(),
            Err(_) => GuardStatus::default(),
        },
        _ => GuardStatus::default(),
    }
}

fn sync_startup_from_server(
    cfg: &LauncherConfig,
    ep: &([u8; 4], u16, String),
    msg_tx: &Sender<WorkerMsg>,
    stop: &AtomicBool,
) {
    if stop.load(Ordering::Acquire) {
        return;
    }
    let st = poll_heartbeat(ep);
    let hb_online = st.online;
    let _ = msg_tx.send(WorkerMsg::GuardUpdated(st));
    if stop.load(Ordering::Acquire) {
        return;
    }

    let cat_req = format_get(catalog_path(), &ep.2);
    let mut cat_detail = String::from("err");
    if let Ok((200, body)) = http_exchange(ep.0, ep.1, &cat_req, MAX_TSV_BYTES)
        && let Ok(text) = std::str::from_utf8(&body)
        && let Ok(parsed) = Catalog::parse_tsv(text)
    {
        cat_detail = format!("games={}", parsed.entries().len());
        if !parsed.is_empty() {
            let local_cat = Path::new(&cfg.cache_dir).join("catalog.tsv");
            let _ = fs::write(local_cat, text.as_bytes());
            let _ = msg_tx.send(WorkerMsg::CatalogLoaded { catalog: parsed });
        }
    }
    diag::stage("worker_sync", &format!("hb_online={hb_online} {cat_detail}"));
    if stop.load(Ordering::Acquire) {
        return;
    }

    let us_req = format_get(userstate_path(), &ep.2);
    if let Ok((200, body)) = http_exchange(ep.0, ep.1, &us_req, MAX_TSV_BYTES)
        && let Ok(text) = std::str::from_utf8(&body)
    {
        let local_us = Path::new(&cfg.cache_dir).join("userstate.tsv");
        let _ = fs::write(local_us, text.as_bytes());
        let _ = msg_tx.send(WorkerMsg::UserstateLoaded(String::from(text)));
    }
}

fn worker_loop(
    cfg: LauncherConfig,
    cmd_rx: Receiver<WorkerCmd>,
    msg_tx: Sender<WorkerMsg>,
    stop: Arc<AtomicBool>,
) {
    diag::register_thread(diag::ThreadRole::Worker);
    ensure_cache_dirs(&cfg.cache_dir);
    let ep = endpoint(&cfg);

    if let Some(ref server) = ep {
        sync_startup_from_server(&cfg, server, &msg_tx, &stop);
    }

    let poll_every = Duration::from_secs(u64::from(cfg.poll_interval_sec.max(2)));
    let mut last_hb = Instant::now();

    loop {
        if stop.load(Ordering::Acquire) {
            while let Ok(item) = cmd_rx.try_recv() {
                match item {
                    WorkerCmd::SetFavorite { title_id, favorite } => {
                        if let Some((ip, port, host_hdr)) = ep.as_ref() {
                            let req = format_post(&favorite_path(&title_id, favorite), host_hdr);
                            let _ = http_exchange(*ip, *port, &req, 4096);
                        }
                    }
                    WorkerCmd::RecordPlayed { title_id } => {
                        if let Some((ip, port, host_hdr)) = ep.as_ref() {
                            let req = format_post(&played_path(&title_id), host_hdr);
                            let _ = http_exchange(*ip, *port, &req, 4096);
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        let cmd = match cmd_rx.recv_timeout(Duration::from_millis(80)) {
            Ok(cmd) => Some(cmd),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if let Some(first) = cmd {
            let mut batch = vec![first];
            while let Ok(more) = cmd_rx.try_recv() {
                batch.push(more);
            }
            for item in batch {
                let stopping = stop.load(Ordering::Acquire);
                match item {
                    WorkerCmd::Shutdown => return,
                    WorkerCmd::LoadPoster {
                        title_id,
                        fetch_remote,
                    } => {
                        if stopping {
                            continue;
                        }
                        let img = load_or_fetch_art(
                            &cfg.cache_dir,
                            "posters",
                            &title_id,
                            poster_path(&title_id),
                            fetch_remote,
                            ep.as_ref(),
                        );
                        let _ = msg_tx.send(WorkerMsg::PosterReady {
                            title_id,
                            image: img,
                        });
                    }
                    WorkerCmd::LoadBackdrop {
                        title_id,
                        fetch_remote,
                    } => {
                        if stopping {
                            continue;
                        }
                        let img = load_or_fetch_art(
                            &cfg.cache_dir,
                            "backdrops",
                            &title_id,
                            backdrop_path(&title_id),
                            fetch_remote,
                            ep.as_ref(),
                        );
                        let _ = msg_tx.send(WorkerMsg::BackdropReady {
                            title_id,
                            image: img,
                        });
                    }
                    WorkerCmd::SetFavorite { title_id, favorite } => {
                        if let Some((ip, port, host_hdr)) = ep.as_ref() {
                            let req = format_post(&favorite_path(&title_id, favorite), host_hdr);
                            let _ = http_exchange(*ip, *port, &req, 4096);
                        }
                    }
                    WorkerCmd::RecordPlayed { title_id } => {
                        if let Some((ip, port, host_hdr)) = ep.as_ref() {
                            let req = format_post(&played_path(&title_id), host_hdr);
                            let _ = http_exchange(*ip, *port, &req, 4096);
                        }
                    }
                    WorkerCmd::RequestMoreTime { minutes } => {
                        if let Some((ip, port, host_hdr)) = ep.as_ref() {
                            let req = format_post(&request_time_path(minutes), host_hdr);
                            match http_exchange(*ip, *port, &req, 4096) {
                                Ok((200, _)) => {
                                    let _ = msg_tx.send(WorkerMsg::Notice(format!(
                                        "Sent +{minutes} min request to parent"
                                    )));
                                }
                                _ => {
                                    let _ = msg_tx.send(WorkerMsg::Notice(String::from(
                                        "Could not reach xbox-guard server",
                                    )));
                                }
                            }
                        } else {
                            let _ = msg_tx.send(WorkerMsg::Notice(String::from(
                                "No xbox-guard server configured",
                            )));
                        }
                    }
                    WorkerCmd::RequestResume => {
                        if let Some((ip, port, host_hdr)) = ep.as_ref() {
                            let req = format_post(request_resume_path(), host_hdr);
                            match http_exchange(*ip, *port, &req, 4096) {
                                Ok((200, _)) => {
                                    let _ = msg_tx.send(WorkerMsg::Notice(String::from(
                                        "Sent resume request to parent",
                                    )));
                                }
                                _ => {
                                    let _ = msg_tx.send(WorkerMsg::Notice(String::from(
                                        "Could not reach xbox-guard server",
                                    )));
                                }
                            }
                        }
                    }
                }
            }
        }

        if !stop.load(Ordering::Acquire)
            && let Some(ref server) = ep
            && last_hb.elapsed() >= poll_every
        {
            last_hb = Instant::now();
            let st = poll_heartbeat(server);
            let _ = msg_tx.send(WorkerMsg::GuardUpdated(st));
        }
    }
}
