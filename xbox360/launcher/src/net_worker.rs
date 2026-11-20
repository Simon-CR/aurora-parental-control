//! Background worker thread for Hdd1 cache reads, xbox-guard LAN HTTP sync, and JPEG decoding.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use launcher_core::catalog::Catalog;
use launcher_core::guard::{
    GuardStatus, LauncherConfig, backdrop_path, catalog_path, favorite_path, format_get,
    format_post, heartbeat_path, parse_heartbeat_body, parse_http_response, played_path,
    poster_path, request_resume_path, request_time_path, userstate_path,
};

use crate::assets::DecodedRgba;

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
    join: Option<JoinHandle<()>>,
}

impl NetWorker {
    pub fn spawn(cfg: LauncherConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        let join = thread::Builder::new()
            .name(String::from("shelf-worker"))
            .stack_size(256 * 1024)
            .spawn(move || worker_loop(cfg, cmd_rx, msg_tx))
            .ok();
        Self {
            cmd_tx,
            msg_rx,
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
        let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

fn ensure_cache_dirs(cache_dir: &str) {
    let base = Path::new(cache_dir);
    let _ = fs::create_dir_all(base.join("posters"));
    let _ = fs::create_dir_all(base.join("backdrops"));
}

fn endpoint(cfg: &LauncherConfig) -> Option<(SocketAddr, String)> {
    let ip = cfg.server_ip?;
    let host_hdr = cfg.server_host_header()?;
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), cfg.server_port));
    Some((addr, host_hdr))
}

fn http_exchange(
    addr: SocketAddr,
    req: &[u8],
    max_body_bytes: usize,
) -> Result<(u16, Vec<u8>), String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(1200))
        .map_err(|e| format!("connect: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(4)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    stream.write_all(req).map_err(|e| format!("write: {e}"))?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > max_body_bytes + 4096 {
                    return Err(String::from("HTTP response too large"));
                }
                raw.extend_from_slice(&chunk[..n]);
            }
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let (code, body) = parse_http_response(&raw)?;
    Ok((code, body.to_vec()))
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
    ep: Option<&(SocketAddr, String)>,
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
    let (addr, host_hdr) = ep?;
    let req = format_get(&remote_path, host_hdr);
    let (code, body) = http_exchange(*addr, &req, MAX_JPEG_BYTES).ok()?;
    if code != 200 || body.is_empty() {
        return None;
    }
    let decoded = decode_image_bytes(&body)?;
    let _ = fs::write(&disk_path, &body);
    Some(decoded)
}

fn poll_heartbeat(ep: &(SocketAddr, String)) -> GuardStatus {
    let req = format_get(&heartbeat_path(LAUNCHER_TITLE_ID_HEX), &ep.1);
    match http_exchange(ep.0, &req, 64 * 1024) {
        Ok((200, body)) => match std::str::from_utf8(&body) {
            Ok(text) => parse_heartbeat_body(text).unwrap_or_default(),
            Err(_) => GuardStatus::default(),
        },
        _ => GuardStatus::default(),
    }
}

fn sync_startup_from_server(cfg: &LauncherConfig, ep: &(SocketAddr, String), msg_tx: &Sender<WorkerMsg>) {
    let _ = msg_tx.send(WorkerMsg::GuardUpdated(poll_heartbeat(ep)));

    let cat_req = format_get(catalog_path(), &ep.1);
    if let Ok((200, body)) = http_exchange(ep.0, &cat_req, MAX_TSV_BYTES)
        && let Ok(text) = std::str::from_utf8(&body)
        && let Ok(parsed) = Catalog::parse_tsv(text)
    {
        if !parsed.is_empty() {
            let local_cat = Path::new(&cfg.cache_dir).join("catalog.tsv");
            let _ = fs::write(local_cat, text.as_bytes());
            let _ = msg_tx.send(WorkerMsg::CatalogLoaded { catalog: parsed });
        }
    }

    let us_req = format_get(userstate_path(), &ep.1);
    if let Ok((200, body)) = http_exchange(ep.0, &us_req, MAX_TSV_BYTES)
        && let Ok(text) = std::str::from_utf8(&body)
    {
        let local_us = Path::new(&cfg.cache_dir).join("userstate.tsv");
        let _ = fs::write(local_us, text.as_bytes());
        let _ = msg_tx.send(WorkerMsg::UserstateLoaded(String::from(text)));
    }
}

fn worker_loop(cfg: LauncherConfig, cmd_rx: Receiver<WorkerCmd>, msg_tx: Sender<WorkerMsg>) {
    ensure_cache_dirs(&cfg.cache_dir);
    let ep = endpoint(&cfg);

    if let Some(ref server) = ep {
        sync_startup_from_server(&cfg, server, &msg_tx);
    }

    let poll_every = Duration::from_secs(u64::from(cfg.poll_interval_sec.max(2)));
    let mut last_hb = Instant::now();

    loop {
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
                match item {
                    WorkerCmd::Shutdown => return,
                    WorkerCmd::LoadPoster {
                        title_id,
                        fetch_remote,
                    } => {
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
                        if let Some((addr, host_hdr)) = ep.as_ref() {
                            let req = format_post(&favorite_path(&title_id, favorite), host_hdr);
                            let _ = http_exchange(*addr, &req, 4096);
                        }
                    }
                    WorkerCmd::RecordPlayed { title_id } => {
                        if let Some((addr, host_hdr)) = ep.as_ref() {
                            let req = format_post(&played_path(&title_id), host_hdr);
                            let _ = http_exchange(*addr, &req, 4096);
                        }
                    }
                    WorkerCmd::RequestMoreTime { minutes } => {
                        if let Some((addr, host_hdr)) = ep.as_ref() {
                            let req = format_post(&request_time_path(minutes), host_hdr);
                            match http_exchange(*addr, &req, 4096) {
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
                        if let Some((addr, host_hdr)) = ep.as_ref() {
                            let req = format_post(request_resume_path(), host_hdr);
                            match http_exchange(*addr, &req, 4096) {
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

        if let Some(ref server) = ep
            && last_hb.elapsed() >= poll_every
        {
            last_hb = Instant::now();
            let st = poll_heartbeat(server);
            let _ = msg_tx.send(WorkerMsg::GuardUpdated(st));
        }
    }
}
