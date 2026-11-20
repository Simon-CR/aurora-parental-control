#!/usr/bin/env python3
"""Sync Xbox 360 Shelf launcher catalog and pre-sized JPEG artwork from Aurora DBs.

Generates a deterministic UTF-8 `catalog.tsv` plus resized 180x248 posters and
640x360 backdrops into `--out-dir` without altering Aurora `.asset` files or DBs.
"""

from __future__ import annotations

import argparse
import ftplib
import http.client
import io
import json
import os
import re
import sqlite3
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass


CONTENT_DB_PATH = "/Game/Data/Databases/content.db"
SETTINGS_DB_PATH = "/Game/Data/Databases/settings.db"
DEFAULT_FTP_CACHE_DIR = "/Hdd1/PeerPressure/Launcher/cache"

XBOXUNITY_HOST = "xboxunity.net"
XBOX_CDN_HOST = "download.xbox.com"
COVER_INFO_PATH = "/Resources/Lib/CoverInfo.php?titleid={tid}"
COVER_IMG_PATH = "/Resources/Lib/Cover.php?size={size}&cid={cid}"
XBOX_IMG_BASE = "/content/images/66acd000-77fe-1000-9115-d802{tid_lower}/1033"
USER_AGENT = "Aurora/0.7b"

POSTER_SIZE = (180, 248)
BACKDROP_SIZE = (640, 360)
YEAR_RE = re.compile(r"\b(19\d{2}|20\d{2})\b")
LOCAL_MP_DESC_RE = re.compile(
    r"(?:"
    r"local\s+multiplayer(?:\s+(\d+)\s*-\s*(\d+)|\s+(\d+))?"
    r"|local\s+co-?op(?:\s+(\d+)\s*-\s*(\d+)|\s+(\d+))?"
    r"|offline\s+(?:multiplayer|co-?op|versus|players?)(?:\s+(\d+)\s*-\s*(\d+)|\s+(\d+))?"
    r"|split[- ]?screen(?:\s+(\d+)\s*-\s*(\d+))?"
    r"|same\s+(?:console|screen|xbox)"
    r"|\b([2-9]|1[0-6])\s+players?\s+(?:local|offline|split[- ]?screen|co-?op|on\s+one)"
    r"|\b([2-9]|1[0-6])\s+player\s+co-?op"
    r"|multiplayer\s+local"
    r")",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class CatalogRow:
    item_id: int
    title_id: str
    media_id: str
    title_name: str
    genre_flag: int
    content_group: int
    file_type: int
    device: str
    directory: str
    executable: str
    publisher: str
    release_year: str
    description: str
    local_players: int = 0


class PersistentFTP:
    """Single passive FTP session with Aurora FtpDll-safe CWD before LIST."""

    def __init__(self, host: str, port: int, user: str, password: str, timeout: int = 20) -> None:
        self.host = host
        self.port = port
        self.user = user
        self.password = password
        self.timeout = timeout
        self.ftp: ftplib.FTP | None = None
        self.connect()

    def connect(self) -> None:
        if self.ftp is not None:
            try:
                self.ftp.close()
            except Exception:
                pass
        ftp = ftplib.FTP()
        ftp.connect(self.host, self.port, timeout=self.timeout)
        ftp.login(self.user, self.password)
        ftp.set_pasv(True)
        self.ftp = ftp

    def close(self) -> None:
        if self.ftp is not None:
            try:
                self.ftp.quit()
            except Exception:
                try:
                    self.ftp.close()
                except Exception:
                    pass
            self.ftp = None

    def _run(self, fn):
        last_err: Exception | None = None
        for attempt in range(3):
            try:
                if self.ftp is None:
                    self.connect()
                assert self.ftp is not None
                return fn(self.ftp)
            except ftplib.error_perm as exc:
                if str(exc).startswith("550"):
                    raise
                last_err = exc
                self.connect()
            except (EOFError, OSError, TimeoutError, ftplib.all_errors) as exc:
                last_err = exc
                if attempt < 2:
                    time.sleep(0.4 * (attempt + 1))
                    self.connect()
        assert last_err is not None
        raise last_err

    def retr_bytes(self, remote_path: str) -> bytes:
        def op(ftp: ftplib.FTP) -> bytes:
            buf = io.BytesIO()
            ftp.retrbinary(f"RETR {remote_path}", buf.write)
            return buf.getvalue()

        return self._run(op)

    def ensure_dir(self, remote_dir: str) -> None:
        parts = [p for p in remote_dir.replace("\\", "/").split("/") if p]
        cur = ""
        for part in parts:
            cur += "/" + part
            try:
                self._run(lambda f, d=cur: f.cwd(d))
            except ftplib.error_perm:
                try:
                    self._run(lambda f, d=cur: f.mkd(d))
                except ftplib.error_perm:
                    pass
                self._run(lambda f, d=cur: f.cwd(d))

    def list_dir_files(self, remote_dir: str) -> dict[str, int]:
        """List files inside remote_dir after CWD (required by Aurora FtpDll)."""
        def op(ftp: ftplib.FTP) -> dict[str, int]:
            ftp.cwd(remote_dir)
            lines: list[str] = []
            ftp.retrlines("LIST", lines.append)
            files: dict[str, int] = {}
            for line in lines:
                parts = line.split(maxsplit=8)
                if len(parts) < 9:
                    continue
                name = parts[8].strip()
                if name in {".", ".."} or parts[0].startswith("d"):
                    continue
                try:
                    size = int(parts[4])
                except ValueError:
                    size = 0
                files[name.lower()] = size
            return files

        return self._run(op)

    def stor_bytes(self, remote_dir: str, filename: str, data: bytes) -> None:
        def op(ftp: ftplib.FTP) -> None:
            ftp.cwd(remote_dir)
            ftp.storbinary(f"STOR {filename}", io.BytesIO(data))

        self._run(op)


def clean_tsv_text(raw: object, max_len: int | None = None) -> str:
    text = str(raw or "")
    text = text.replace("\r", " ").replace("\n", " ").replace("\t", " ")
    text = " ".join(text.split())
    if max_len is not None and len(text) > max_len:
        if max_len > 3:
            text = text[: max_len - 3].rstrip() + "..."
        else:
            text = text[:max_len]
    return text


def format_u32_hex(val: object) -> str:
    if val is None:
        return "00000000"
    return f"{int(val) & 0xFFFFFFFF:08X}"


def format_u32_dec(val: object) -> int:
    if val is None:
        return 0
    return int(val) & 0xFFFFFFFF


def extract_release_year(raw: object) -> str:
    if raw is None:
        return ""
    m = YEAR_RE.search(str(raw))
    return m.group(1) if m else ""


def detect_local_players(game_caps_offline: object, description: object) -> int:
    """Return max local player count (>=2 for local multiplayer, 1 single-player, 0 unknown)."""
    caps = int(game_caps_offline or 0) & 0xFFFFFFFFFFFFFFFF
    off_max = (caps >> 8) & 0xFF
    coop_max = (caps >> 24) & 0xFF
    cap_max = max(off_max, coop_max)
    if cap_max >= 2:
        return min(cap_max, 16)
    if off_max == 1 and coop_max == 0:
        return 1

    text = str(description or "")
    m = LOCAL_MP_DESC_RE.search(text)
    if not m:
        return 0
    nums = [int(g) for g in m.groups() if g and g.isdigit()]
    inferred = max(nums) if nums else 2
    return max(2, min(inferred, 16))


def normalize_mount_point(raw: object, device_name: object = None) -> str:
    mp = str(raw or "").strip()
    if mp:
        return mp if mp.endswith(":") else mp + ":"
    dn = str(device_name or "").strip()
    if dn:
        return dn if dn.endswith(":") else dn + ":"
    return "Usb0:"


def merge_scan_directory(scan_path: str, item_dir: str) -> str:
    sp = (scan_path or "").strip().replace("/", "\\")
    d = (item_dir or "").strip().replace("/", "\\")
    if sp in ("", "\\"):
        if not d:
            return "\\"
        return d if d.startswith("\\") else "\\" + d
    sp_norm = sp if sp.startswith("\\") else "\\" + sp
    sp_norm = sp_norm.rstrip("\\")
    d_norm = d if d.startswith("\\") else "\\" + d
    if d_norm.lower().startswith(sp_norm.lower() + "\\") or d_norm.lower() == sp_norm.lower():
        return d_norm
    if not d:
        return sp_norm
    return sp_norm + d_norm


def load_catalog_rows(content_db_bytes: bytes, settings_db_bytes: bytes | None) -> list[CatalogRow]:
    """Read Aurora ContentItems joined with MountedDevices and optional ScanPaths."""
    scan_paths: dict[int, tuple[str, str]] = {}
    if settings_db_bytes:
        s_fd, s_path = tempfile.mkstemp(suffix=".db")
        os.close(s_fd)
        try:
            with open(s_path, "wb") as f:
                f.write(settings_db_bytes)
            scon = sqlite3.connect(s_path)
            try:
                for row in scon.execute("SELECT Id, Path, DeviceId FROM ScanPaths"):
                    scan_paths[int(row[0])] = (str(row[1] or ""), str(row[2] or ""))
            finally:
                scon.close()
        finally:
            if os.path.exists(s_path):
                os.remove(s_path)

    c_fd, c_path = tempfile.mkstemp(suffix=".db")
    os.close(c_fd)
    try:
        with open(c_path, "wb") as f:
            f.write(content_db_bytes)
        con = sqlite3.connect(c_path)
        try:
            devices: dict[str, str] = {}
            for dev_id, dev_name, _vroot, mount_point in con.execute(
                "SELECT DeviceId, DeviceName, VirtualRoot, MountPoint FROM MountedDevices"
            ):
                if dev_id:
                    devices[str(dev_id)] = normalize_mount_point(mount_point, dev_name)

            query = (
                "SELECT Id, TitleId, MediaId, TitleName, GenreFlag, ContentGroup, FileType, "
                "Directory, Executable, Publisher, ReleaseDate, Description, ScanPathId, GameCapsOffline "
                "FROM ContentItems "
                "WHERE TitleId IS NOT NULL AND TitleId != 0 "
                "ORDER BY LOWER(COALESCE(TitleName, '')), Id"
            )
            rows: list[CatalogRow] = []
            for r in con.execute(query):
                (
                    item_id,
                    title_id,
                    media_id,
                    title_name,
                    genre_flag,
                    content_group,
                    file_type,
                    directory,
                    executable,
                    publisher,
                    release_date,
                    description,
                    scan_path_id,
                    game_caps_offline,
                ) = r
                tid_hex = format_u32_hex(title_id)
                if tid_hex == "00000000":
                    continue
                sp_path, dev_id = scan_paths.get(int(scan_path_id or 0), ("\\", ""))
                device = devices.get(dev_id) or "Usb0:"
                resolved_dir = merge_scan_directory(sp_path, str(directory or ""))
                local_players = detect_local_players(game_caps_offline, description)
                rows.append(
                    CatalogRow(
                        item_id=int(item_id),
                        title_id=tid_hex,
                        media_id=format_u32_hex(media_id),
                        title_name=clean_tsv_text(title_name) or tid_hex,
                        genre_flag=format_u32_dec(genre_flag),
                        content_group=int(content_group or 0),
                        file_type=int(file_type or 0),
                        device=clean_tsv_text(device),
                        directory=clean_tsv_text(resolved_dir),
                        executable=clean_tsv_text(executable),
                        publisher=clean_tsv_text(publisher),
                        release_year=extract_release_year(release_date),
                        description=clean_tsv_text(description, max_len=240),
                        local_players=local_players,
                    )
                )
            return rows
        finally:
            con.close()
    finally:
        if os.path.exists(c_path):
            os.remove(c_path)


def render_catalog_tsv(
    rows: list[CatalogRow],
    poster_ids: set[str],
    backdrop_ids: set[str],
    timestamp: int | None = None,
) -> str:
    ts = int(timestamp if timestamp is not None else time.time())
    lines = [f"SHELF_CATALOG\tv1\t{ts}\t{len(rows)}"]
    for row in rows:
        has_poster = "1" if row.title_id in poster_ids else "0"
        has_backdrop = "1" if row.title_id in backdrop_ids else "0"
        cols = [
            str(row.item_id),
            row.title_id,
            row.media_id,
            row.title_name,
            str(row.genre_flag),
            str(row.content_group),
            str(row.file_type),
            row.device,
            row.directory,
            row.executable,
            row.publisher,
            row.release_year,
            has_poster,
            has_backdrop,
            row.description,
            str(row.local_players),
        ]
        lines.append("\t".join(cols))
    return "\n".join(lines) + "\n"


def _http_get_bytes(host: str, path: str, user_agent: str = USER_AGENT, timeout: float = 12.0) -> bytes | None:
    conn: http.client.HTTPConnection | None = None
    try:
        conn = http.client.HTTPConnection(host, timeout=timeout)
        conn.request("GET", path, headers={"User-Agent": user_agent, "Accept": "*/*"})
        resp = conn.getresponse()
        body = resp.read()
        if resp.status == 200 and body:
            return body
        return None
    except (OSError, http.client.HTTPException):
        return None
    finally:
        if conn is not None:
            try:
                conn.close()
            except Exception:
                pass


def fetch_raw_cover_bytes(title_id: str) -> bytes | None:
    info_raw = _http_get_bytes(XBOXUNITY_HOST, COVER_INFO_PATH.format(tid=title_id))
    if info_raw:
        try:
            payload = json.loads(info_raw.decode("utf-8", errors="ignore"))
            covers = payload.get("Covers") or []
            if covers:
                official = [c for c in covers if str(c.get("Official", "0")) == "1"]
                pick = official[0] if official else covers[0]
                cid = pick.get("CoverID")
                if cid:
                    img = _http_get_bytes(XBOXUNITY_HOST, COVER_IMG_PATH.format(size="large", cid=cid))
                    if img:
                        return img
        except (ValueError, KeyError, TypeError):
            pass

    cdn_path = f"{XBOX_IMG_BASE.format(tid_lower=title_id.lower())}/boxartlg.jpg"
    return _http_get_bytes(XBOX_CDN_HOST, cdn_path)


def fetch_raw_backdrop_bytes(title_id: str) -> bytes | None:
    cdn_path = f"{XBOX_IMG_BASE.format(tid_lower=title_id.lower())}/background.jpg"
    return _http_get_bytes(XBOX_CDN_HOST, cdn_path)


def resize_cover_or_backdrop(raw_bytes: bytes, size: tuple[int, int], quality: int) -> bytes:
    from PIL import Image, ImageOps  # imported after startup check when --fetch-art is active

    with Image.open(io.BytesIO(raw_bytes)) as img:
        rgb = img.convert("RGB")
        fitted = ImageOps.fit(rgb, size, method=Image.Resampling.LANCZOS, centering=(0.5, 0.5))
        out = io.BytesIO()
        fitted.save(out, format="JPEG", quality=quality, optimize=True)
        return out.getvalue()


def sync_missing_art(
    title_ids: list[str],
    out_dir: str,
    workers: int,
    force: bool,
) -> tuple[int, int, int]:
    posters_dir = os.path.join(out_dir, "posters")
    backdrops_dir = os.path.join(out_dir, "backdrops")
    os.makedirs(posters_dir, exist_ok=True)
    os.makedirs(backdrops_dir, exist_ok=True)

    sem = threading.Semaphore(max(1, workers))

    def process_title(tid: str) -> tuple[int, int, int]:
        p_ok = 0
        b_ok = 0
        errs = 0
        poster_path = os.path.join(posters_dir, f"{tid}.jpg")
        backdrop_path = os.path.join(backdrops_dir, f"{tid}.jpg")

        with sem:
            if force or not os.path.exists(poster_path):
                time.sleep(0.05)
                raw = fetch_raw_cover_bytes(tid)
                if raw:
                    try:
                        jpg = resize_cover_or_backdrop(raw, POSTER_SIZE, quality=82)
                        with open(poster_path, "wb") as f:
                            f.write(jpg)
                        p_ok = 1
                    except Exception:
                        errs += 1

            if force or not os.path.exists(backdrop_path):
                time.sleep(0.05)
                raw = fetch_raw_backdrop_bytes(tid)
                if raw:
                    try:
                        jpg = resize_cover_or_backdrop(raw, BACKDROP_SIZE, quality=80)
                        with open(backdrop_path, "wb") as f:
                            f.write(jpg)
                        b_ok = 1
                    except Exception:
                        errs += 1
        return p_ok, b_ok, errs

    posters_saved = 0
    backdrops_saved = 0
    errors = 0
    with ThreadPoolExecutor(max_workers=max(1, workers)) as pool:
        futs = [pool.submit(process_title, tid) for tid in title_ids]
        for fut in as_completed(futs):
            p_ok, b_ok, errs = fut.result()
            posters_saved += p_ok
            backdrops_saved += b_ok
            errors += errs
    return posters_saved, backdrops_saved, errors


def scan_existing_jpeg_ids(directory: str) -> set[str]:
    ids: set[str] = set()
    if not os.path.isdir(directory):
        return ids
    for name in os.listdir(directory):
        lower = name.lower()
        if lower.endswith(".jpg"):
            stem = name[:-4].upper()
        elif lower.endswith(".jpeg"):
            stem = name[:-5].upper()
        else:
            continue
        if len(stem) == 8 and all(c in "0123456789ABCDEF" for c in stem):
            ids.add(stem)
    return ids


def push_catalog_to_server(server_url: str, tsv_bytes: bytes) -> str:
    url = server_url.rstrip("/") + "/api/v1/launcher/catalog"
    req = urllib.request.Request(
        url,
        data=tsv_bytes,
        method="POST",
        headers={"Content-Type": "text/tab-separated-values; charset=utf-8"},
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        return resp.read().decode("utf-8", errors="replace").strip()


def seed_ftp_cache(
    ftp: PersistentFTP,
    ftp_cache_dir: str,
    out_dir: str,
    catalog_bytes: bytes,
) -> tuple[int, int]:
    base = ftp_cache_dir.rstrip("/")
    posters_remote = f"{base}/posters"
    backdrops_remote = f"{base}/backdrops"
    ftp.ensure_dir(base)
    ftp.ensure_dir(posters_remote)
    ftp.ensure_dir(backdrops_remote)
    ftp.stor_bytes(base, "catalog.tsv", catalog_bytes)

    uploaded_posters = 0
    existing_posters = ftp.list_dir_files(posters_remote)
    local_posters_dir = os.path.join(out_dir, "posters")
    if os.path.isdir(local_posters_dir):
        for fname in sorted(os.listdir(local_posters_dir)):
            if not fname.lower().endswith(".jpg"):
                continue
            if fname.lower() in existing_posters:
                continue
            with open(os.path.join(local_posters_dir, fname), "rb") as f:
                ftp.stor_bytes(posters_remote, fname, f.read())
            uploaded_posters += 1

    uploaded_backdrops = 0
    existing_backdrops = ftp.list_dir_files(backdrops_remote)
    local_backdrops_dir = os.path.join(out_dir, "backdrops")
    if os.path.isdir(local_backdrops_dir):
        for fname in sorted(os.listdir(local_backdrops_dir)):
            if not fname.lower().endswith(".jpg"):
                continue
            if fname.lower() in existing_backdrops:
                continue
            with open(os.path.join(local_backdrops_dir, fname), "rb") as f:
                ftp.stor_bytes(backdrops_remote, fname, f.read())
            uploaded_backdrops += 1

    return uploaded_posters, uploaded_backdrops


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Export Aurora content.db into Shelf catalog.tsv and pre-sized JPEG artwork."
    )
    parser.add_argument("--host", help="Xbox 360 FTP host/IP (reads Aurora DBs over read-only FTP)")
    parser.add_argument("--port", type=int, default=21, help="FTP port (default: 21)")
    parser.add_argument("--user", default="xboxftp", help="FTP username (default: xboxftp)")
    parser.add_argument("--password", default="xboxftp", help="FTP password (default: xboxftp)")
    parser.add_argument("--content-db", help="Local copy of Aurora content.db (alternative to --host)")
    parser.add_argument("--settings-db", help="Local copy of Aurora settings.db (used with --content-db)")
    parser.add_argument("--out-dir", default="./launcher-cache", help="Output cache directory (default: ./launcher-cache)")
    parser.add_argument("--fetch-art", action="store_true", help="Fetch missing covers/backdrops and resize to JPEG thumbnails")
    parser.add_argument("--catalog-only", action="store_true", help="Generate/push catalog.tsv without fetching or resizing art")
    parser.add_argument("--force", action="store_true", help="Re-fetch and overwrite existing JPEGs when --fetch-art is set")
    parser.add_argument("--workers", type=int, default=4, help="Concurrent artwork workers (default: 4)")
    parser.add_argument("--push-server", help="Upload generated catalog.tsv to xbox-guard URL (e.g. http://host:8420)")
    parser.add_argument("--seed-ftp", action="store_true", help="Upload catalog.tsv and new JPEGs to Xbox 360 cache dir via FTP")
    parser.add_argument("--ftp-cache-dir", default=DEFAULT_FTP_CACHE_DIR, help=f"Target FTP cache dir (default: {DEFAULT_FTP_CACHE_DIR})")
    args = parser.parse_args()

    if not args.host and not args.content_db:
        parser.error("Provide either --host (Aurora FTP) or --content-db (local SQLite DB)")
    if args.seed_ftp and not args.host:
        parser.error("--seed-ftp requires --host")
    if args.fetch_art and not args.catalog_only:
        try:
            import PIL  # noqa: F401
        except ImportError:
            print(
                "Error: Pillow is required for --fetch-art thumbnail resizing. Run: pip install pillow",
                file=sys.stderr,
            )
            return 2

    ftp: PersistentFTP | None = None
    settings_bytes: bytes | None = None
    if args.host:
        ftp = PersistentFTP(args.host, args.port, args.user, args.password)
        content_bytes = ftp.retr_bytes(CONTENT_DB_PATH)
        try:
            settings_bytes = ftp.retr_bytes(SETTINGS_DB_PATH)
        except Exception:
            settings_bytes = None
    else:
        with open(args.content_db, "rb") as f:
            content_bytes = f.read()
        settings_path = args.settings_db
        if not settings_path:
            sibling = os.path.join(os.path.dirname(os.path.abspath(args.content_db)), "settings.db")
            if os.path.exists(sibling):
                settings_path = sibling
        if settings_path:
            with open(settings_path, "rb") as f:
                settings_bytes = f.read()

    try:
        rows = load_catalog_rows(content_bytes, settings_bytes)
        os.makedirs(args.out_dir, exist_ok=True)
        os.makedirs(os.path.join(args.out_dir, "posters"), exist_ok=True)
        os.makedirs(os.path.join(args.out_dir, "backdrops"), exist_ok=True)

        unique_tids = sorted({r.title_id for r in rows})
        if args.fetch_art and not args.catalog_only:
            p_new, b_new, errs = sync_missing_art(unique_tids, args.out_dir, args.workers, args.force)
            print(f"Artwork sync complete: +{p_new} posters, +{b_new} backdrops ({errs} download/resize errors)")

        poster_ids = scan_existing_jpeg_ids(os.path.join(args.out_dir, "posters"))
        backdrop_ids = scan_existing_jpeg_ids(os.path.join(args.out_dir, "backdrops"))
        tsv_text = render_catalog_tsv(rows, poster_ids, backdrop_ids)
        tsv_bytes = tsv_text.encode("utf-8")

        catalog_path = os.path.join(args.out_dir, "catalog.tsv")
        tmp_path = catalog_path + ".tmp"
        with open(tmp_path, "wb") as f:
            f.write(tsv_bytes)
        os.replace(tmp_path, catalog_path)

        print(
            f"Wrote {catalog_path}: {len(rows)} titles ({len(unique_tids)} unique TitleIDs), "
            f"{len(poster_ids)} posters, {len(backdrop_ids)} backdrops"
        )

        if args.push_server:
            resp = push_catalog_to_server(args.push_server, tsv_bytes)
            print(f"Pushed catalog to {args.push_server}: {resp}")

        if args.seed_ftp and ftp is not None:
            up_p, up_b = seed_ftp_cache(ftp, args.ftp_cache_dir, args.out_dir, tsv_bytes)
            print(f"Seeded FTP {args.ftp_cache_dir}: catalog.tsv, +{up_p} posters, +{up_b} backdrops")

        return 0
    finally:
        if ftp is not None:
            ftp.close()


if __name__ == "__main__":
    raise SystemExit(main())
