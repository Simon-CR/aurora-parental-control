#!/usr/bin/env python3
"""Parallel Xbox 360 artwork sync (Covers, Backgrounds, Icons/Banners, Screenshots — no video).

Supports two modes:
  --mode asset  : Packs native Aurora `RXEA` `.asset` files via `aurora_asset_packer.exe`
                  and uploads missing GC/BK/GL/SS files straight into `/Game/Data/GameData/`.
  --mode import : Stages raw image files into `/Game/User/Import/<TitleID>/` for
                  Aurora -> Settings -> Assets -> Import.
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
import subprocess
import tempfile
import threading
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field


CONTENT_DB_PATH = "/Game/Data/Databases/content.db"
GAMEDATA_ROOT = "/Game/Data/GameData"
DEFAULT_IMPORT_ROOT = "/Game/User/Import"
COVER_MIN_ASSET_BYTES = 24_576
OTHER_MIN_ASSET_BYTES = 4_096

XBOXUNITY_HOST = "xboxunity.net"
XBOX_CDN_HOST = "download.xbox.com"
COVER_INFO_PATH = "/Resources/Lib/CoverInfo.php?titleid={tid}"
COVER_IMG_PATH = "/Resources/Lib/Cover.php?size={size}&cid={cid}"
XBOX_IMG_BASE = "/content/images/66acd000-77fe-1000-9115-d802{tid_lower}/1033"
USER_AGENT = "Aurora/0.7b"

ALL_TYPES = ("cover", "background", "iconbanner", "screenshots")
ASSET_PREFIX = {
    "cover": "GC",
    "background": "BK",
    "iconbanner": "GL",
    "screenshots": "SS",
}
VIDEO_EXTENSIONS = (".asx", ".wmv", ".mp4", ".avi", ".mkv")
FILE_URL_RE = re.compile(r"<live:fileUrl>(https?://[^<]+)</live:fileUrl>", re.IGNORECASE)
SCREENSHOT_NUM_RE = re.compile(r"screen(?:lg|sm)?(\d+)\.(?:jpg|jpeg|png)$", re.IGNORECASE)


@dataclass
class FolderState:
    title_id_hex: str
    db_id_hex: str
    title_name: str
    folder_name: str
    present_assets: set[str] = field(default_factory=set)
    has_asset_info: bool = False


class PersistentFTP:
    """Maintains a single passive FTP connection with transparent reconnect."""

    def __init__(self, host: str, port: int, user: str, password: str, timeout: int = 15) -> None:
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
                    time.sleep(0.5 * (attempt + 1))
                    self.connect()
        assert last_err is not None
        raise last_err

    def retr_bytes(self, remote_path: str) -> bytes:
        def op(ftp: ftplib.FTP) -> bytes:
            buf = io.BytesIO()
            ftp.retrbinary(f"RETR {remote_path}", buf.write)
            return buf.getvalue()

        return self._run(op)

    def retr_in_cwd(self, remote_dir: str, filename: str) -> bytes:
        def op(ftp: ftplib.FTP) -> bytes:
            ftp.cwd(remote_dir)
            buf = io.BytesIO()
            ftp.retrbinary(f"RETR {filename}", buf.write)
            return buf.getvalue()

        return self._run(op)

    def list_dir(self, remote_dir: str) -> list[tuple[bool, str, int]]:
        """Returns (is_dir, name, size) tuples after issuing CWD (required by Aurora FtpDll)."""
        def op(ftp: ftplib.FTP) -> list[tuple[bool, str, int]]:
            ftp.cwd(remote_dir)
            lines: list[str] = []
            ftp.retrlines("LIST", lines.append)
            out: list[tuple[bool, str, int]] = []
            for line in lines:
                parts = line.split(maxsplit=8)
                if len(parts) < 9:
                    continue
                name = parts[8].strip()
                if name in {".", ".."}:
                    continue
                is_dir = parts[0].startswith("d")
                try:
                    size = int(parts[4])
                except ValueError:
                    size = 0
                out.append((is_dir, name, size))
            return out

        return self._run(op)

    def ensure_dir(self, remote_dir: str) -> None:
        parts = [p for p in remote_dir.split("/") if p]
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

    def stor_bytes(self, remote_dir: str, filename: str, data: bytes) -> None:
        def op(ftp: ftplib.FTP) -> None:
            ftp.cwd(remote_dir)
            ftp.storbinary(f"STOR {filename}", io.BytesIO(data))

        self._run(op)

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


class AdaptiveRateLimiter:
    """Bounds concurrency on rate-sensitive hosts (XboxUnity) and throttles on 429/5xx."""

    def __init__(self, max_workers: int, min_interval_sec: float = 0.08) -> None:
        self.max_workers = max(1, max_workers)
        self._sem = threading.Semaphore(self.max_workers)
        self._lock = threading.Lock()
        self._min_interval = min_interval_sec
        self._next_allowed = 0.0
        self._cooldown_until = 0.0
        self._throttled = False

    def __enter__(self) -> AdaptiveRateLimiter:
        self._sem.acquire()
        while True:
            now = time.monotonic()
            with self._lock:
                wait_until = max(self._cooldown_until, self._next_allowed)
                if now >= wait_until:
                    self._next_allowed = now + self._min_interval
                    return self
                delay = wait_until - now
            time.sleep(min(delay, 1.0))

    def __exit__(self, *_exc) -> None:
        self._sem.release()

    def penalize(self, backoff_sec: float) -> None:
        with self._lock:
            self._min_interval = min(0.6, max(self._min_interval * 1.5, 0.25))
            self._cooldown_until = max(self._cooldown_until, time.monotonic() + backoff_sec)
            if not self._throttled and self.max_workers > 2:
                self._throttled = True
                for _ in range(max(1, self.max_workers // 2)):
                    self._sem.acquire(blocking=False)


class HTTPTransientError(Exception):
    def __init__(self, code: int) -> None:
        super().__init__(f"HTTP {code}")
        self.code = code


_thread_local = threading.local()


def _get_conn(host: str) -> http.client.HTTPConnection:
    conns: dict[str, http.client.HTTPConnection] = getattr(_thread_local, "conns", None)
    if conns is None:
        conns = {}
        _thread_local.conns = conns
    conn = conns.get(host)
    if conn is None:
        conn = http.client.HTTPConnection(host, 80, timeout=12)
        conns[host] = conn
    return conn


def _reset_conn(host: str) -> None:
    conns: dict[str, http.client.HTTPConnection] | None = getattr(_thread_local, "conns", None)
    if conns and host in conns:
        try:
            conns[host].close()
        except Exception:
            pass
        del conns[host]


def http_get_host(host: str, path: str) -> bytes:
    """HTTP/1.1 keep-alive GET returning raw body or raising HTTPTransientError."""
    headers = {"User-Agent": USER_AGENT, "Connection": "keep-alive"}
    for _ in range(3):
        conn = _get_conn(host)
        try:
            conn.request("GET", path, headers=headers)
            resp = conn.getresponse()
            body = resp.read()
        except Exception:
            _reset_conn(host)
            raise

        if resp.status in (301, 302, 303, 307, 308):
            loc = resp.getheader("Location", "")
            parsed = urllib.parse.urlsplit(loc)
            if parsed.scheme == "https" or (parsed.netloc and parsed.netloc.lower() != host.lower()):
                _reset_conn(host)
                raise HTTPTransientError(resp.status)
            path = parsed.path + (f"?{parsed.query}" if parsed.query else "")
            continue
        if resp.status in (429, 500, 502, 503, 504):
            _reset_conn(host)
            raise HTTPTransientError(resp.status)
        if resp.status != 200:
            raise HTTPTransientError(resp.status)
        return body

    raise HTTPTransientError(503)


def http_get_url(url: str) -> bytes | None:
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != "http" or not parsed.netloc:
        return None
    path = parsed.path + (f"?{parsed.query}" if parsed.query else "")
    for attempt in range(3):
        try:
            return http_get_host(parsed.netloc, path)
        except HTTPTransientError as exc:
            if exc.code in (429, 500, 502, 503, 504) and attempt < 2:
                time.sleep(0.5 * (attempt + 1))
                continue
            return None
        except Exception:
            if attempt < 2:
                time.sleep(0.4 * (attempt + 1))
                continue
            return None
    return None


def is_valid_image(data: bytes) -> bool:
    return len(data) > 512 and (
        data.startswith(b"\x89PNG\r\n\x1a\n") or data.startswith(b"\xff\xd8\xff")
    )


def pick_best_cover_id(info_json: dict) -> str | None:
    covers = info_json.get("Covers") or []
    if not covers:
        return None

    def score(c: dict) -> tuple[int, float, int]:
        official = 1 if str(c.get("Official", "0")) == "1" else 0
        try:
            rating = float(c.get("Rating") or 0)
        except (TypeError, ValueError):
            rating = 0.0
        try:
            cid = int(c.get("CoverID") or 0)
        except (TypeError, ValueError):
            cid = 0
        return (official, rating, cid)

    cid = max(covers, key=score).get("CoverID")
    return str(cid) if cid else None


def load_content_folders(db_bytes: bytes) -> list[FolderState]:
    fd, tmp_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        with open(tmp_path, "wb") as f:
            f.write(db_bytes)
        con = sqlite3.connect(tmp_path)
        try:
            rows = con.execute(
                "SELECT Id, TitleId, TitleName FROM ContentItems "
                "WHERE TitleId IS NOT NULL AND TitleId != 0 "
                "ORDER BY Id"
            ).fetchall()
        finally:
            con.close()
    finally:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)

    out: list[FolderState] = []
    for db_id, raw_tid, name in rows:
        tid_hex = f"{int(raw_tid) & 0xFFFFFFFF:08X}"
        if tid_hex == "00000000":
            continue
        db_hex = f"{int(db_id) & 0xFFFFFFFF:08X}"
        out.append(
            FolderState(
                title_id_hex=tid_hex,
                db_id_hex=db_hex,
                title_name=(name or tid_hex),
                folder_name=f"{tid_hex}_{db_hex}",
            )
        )
    return out


def scan_gamedata_folders(
    ftp: PersistentFTP,
    folders: list[FolderState],
    wanted_types: set[str],
    cache_dir: str,
) -> None:
    existing_dirs: dict[str, str] = {}
    try:
        for is_dir, name, _ in ftp.list_dir(GAMEDATA_ROOT):
            if is_dir:
                existing_dirs[name.upper()] = name
    except ftplib.error_perm:
        pass

    for rec in folders:
        actual = existing_dirs.get(rec.folder_name.upper())
        if not actual:
            continue
        rec.folder_name = actual
        remote_dir = f"{GAMEDATA_ROOT}/{actual}"
        try:
            files = ftp.list_dir(remote_dir)
        except ftplib.error_perm:
            continue

        asset_info_name = None
        asset_info_size = 0
        for is_dir, fname, sz in files:
            if is_dir:
                continue
            fu = fname.upper()
            if fu == "GAMEASSETINFO.BIN" and sz > 100:
                rec.has_asset_info = True
                asset_info_name = fname
                asset_info_size = sz
            elif fu.startswith("GC") and fu.endswith(".ASSET") and sz > COVER_MIN_ASSET_BYTES:
                rec.present_assets.add("cover")
            elif fu.startswith("BK") and fu.endswith(".ASSET") and sz > OTHER_MIN_ASSET_BYTES:
                rec.present_assets.add("background")
            elif fu.startswith("GL") and fu.endswith(".ASSET") and sz > OTHER_MIN_ASSET_BYTES:
                rec.present_assets.add("iconbanner")
            elif fu.startswith("SS") and fu.endswith(".ASSET") and sz > OTHER_MIN_ASSET_BYTES:
                rec.present_assets.add("screenshots")

        missing_here = wanted_types - rec.present_assets
        if missing_here and asset_info_name and asset_info_size > 100:
            xml_cache = os.path.join(cache_dir, rec.title_id_hex, "GameAssetInfo.bin")
            if not (os.path.isfile(xml_cache) and os.path.getsize(xml_cache) > 100):
                try:
                    raw = ftp.retr_in_cwd(remote_dir, asset_info_name)
                    if raw:
                        os.makedirs(os.path.dirname(xml_cache), exist_ok=True)
                        with open(xml_cache, "wb") as f:
                            f.write(raw)
                except Exception:
                    pass


def parse_asset_info_urls(cache_dir: str, tid_hex: str) -> dict[str, object]:
    """Extracts image URLs from cached GameAssetInfo.bin XML, strictly ignoring videos."""
    result: dict[str, object] = {"screenshots": {}}
    xml_path = os.path.join(cache_dir, tid_hex, "GameAssetInfo.bin")
    if not os.path.isfile(xml_path):
        return result
    with open(xml_path, "r", encoding="utf-8", errors="replace") as f:
        text = f.read()

    for url in FILE_URL_RE.findall(text):
        low = url.lower()
        if any(low.endswith(ext) for ext in VIDEO_EXTENSIONS):
            continue
        fname = urllib.parse.urlsplit(url).path.rsplit("/", 1)[-1].lower()
        if fname in {"background.jpg", "background.png"}:
            result["background"] = url
        elif fname in {"banner.png", "banner.jpg"}:
            result["banner"] = url
        elif fname in {"tile.png", "icon.png"}:
            result["icon"] = url
        elif fname == "boxartlg.jpg":
            result["cover_lg"] = url
        elif fname == "boxartsm.jpg" and "cover_lg" not in result:
            result["cover_sm"] = url
        else:
            m = SCREENSHOT_NUM_RE.match(fname)
            if m:
                idx = int(m.group(1))
                ss_map = result["screenshots"]
                assert isinstance(ss_map, dict)
                # Prefer screenlg* over screensm* if both appear.
                if idx not in ss_map or "screenlg" in fname:
                    ss_map[idx] = url
    return result


def cached_image_path(cache_dir: str, tid_hex: str, slot_name: str, ext: str = ".img") -> str:
    return os.path.join(cache_dir, tid_hex, f"{slot_name}{ext}")


def find_existing_cached_image(cache_dir: str, tid_hex: str, slot_name: str) -> str | None:
    d = os.path.join(cache_dir, tid_hex)
    if not os.path.isdir(d):
        return None
    for ext in (".png", ".jpg", ".jpeg", ".img"):
        p = os.path.join(d, f"{slot_name}{ext}")
        if os.path.isfile(p) and os.path.getsize(p) > 512:
            return p
    return None


def save_cached_image(cache_dir: str, tid_hex: str, slot_name: str, data: bytes) -> str:
    ext = ".png" if data.startswith(b"\x89PNG") else ".jpg"
    p = os.path.join(cache_dir, tid_hex, f"{slot_name}{ext}")
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "wb") as f:
        f.write(data)
    return p


def download_xboxunity_cover(tid_hex: str, unity_limiter: AdaptiveRateLimiter) -> bytes | None:
    for attempt in range(4):
        with unity_limiter:
            try:
                raw_info = http_get_host(XBOXUNITY_HOST, COVER_INFO_PATH.format(tid=tid_hex))
                info = json.loads(raw_info.decode("utf-8", "replace"))
                cid = pick_best_cover_id(info)
                if not cid:
                    return None
                for size in ("large", "small"):
                    try:
                        img = http_get_host(XBOXUNITY_HOST, COVER_IMG_PATH.format(size=size, cid=cid))
                        if is_valid_image(img):
                            return img
                    except HTTPTransientError as exc:
                        if exc.code in (429, 500, 502, 503, 504):
                            raise
                return None
            except HTTPTransientError as exc:
                if exc.code in (429, 500, 502, 503, 504) and attempt < 3:
                    unity_limiter.penalize(min(6.0, 1.5 * (2**attempt)))
                    continue
                return None
            except Exception:
                if attempt < 3:
                    time.sleep(0.5 * (attempt + 1))
                    continue
                return None
    return None


def fetch_title_images(
    tid_hex: str,
    needed_types: set[str],
    cache_dir: str,
    max_screenshots: int,
    unity_limiter: AdaptiveRateLimiter,
) -> tuple[str, dict[str, list[tuple[str, str]]]]:
    """Downloads missing raw images (no video) for one TitleID into cache_dir.

    Returns (tid_hex, {type_name: [(slot, local_img_path), ...]}).
    """
    urls = parse_asset_info_urls(cache_dir, tid_hex)
    base = f"http://{XBOX_CDN_HOST}" + XBOX_IMG_BASE.format(tid_lower=tid_hex.lower())
    slots_out: dict[str, list[tuple[str, str]]] = {}

    def get_slot(slot_name: str, candidate_urls: list[str], use_unity_tid: bool = False) -> str | None:
        existing = find_existing_cached_image(cache_dir, tid_hex, slot_name)
        if existing:
            return existing
        if use_unity_tid:
            data = download_xboxunity_cover(tid_hex, unity_limiter)
            if data and is_valid_image(data):
                return save_cached_image(cache_dir, tid_hex, slot_name, data)
        for u in candidate_urls:
            if not u:
                continue
            data = http_get_url(u)
            if data and is_valid_image(data):
                return save_cached_image(cache_dir, tid_hex, slot_name, data)
        return None

    if "cover" in needed_types:
        cands = [str(urls.get("cover_lg", "")), str(urls.get("cover_sm", "")), f"{base}/boxartlg.jpg"]
        p = get_slot("cover", cands, use_unity_tid=True)
        if p:
            slots_out["cover"] = [("boxart", p)]

    if "background" in needed_types:
        cands = [str(urls.get("background", "")), f"{base}/background.jpg"]
        p = get_slot("background", cands)
        if p:
            slots_out["background"] = [("background", p)]

    if "iconbanner" in needed_types:
        pairs: list[tuple[str, str]] = []
        p_icon = get_slot("icon", [str(urls.get("icon", "")), f"{base}/tile.png"])
        if p_icon:
            pairs.append(("icon", p_icon))
        p_banner = get_slot("banner", [str(urls.get("banner", "")), f"{base}/banner.png"])
        if p_banner:
            pairs.append(("banner", p_banner))
        if pairs:
            slots_out["iconbanner"] = pairs

    if "screenshots" in needed_types:
        ss_map = urls.get("screenshots")
        assert isinstance(ss_map, dict)
        pairs = []
        if ss_map:
            for seq_idx, orig_idx in enumerate(sorted(ss_map)[:max_screenshots], start=1):
                p = get_slot(f"ss{seq_idx}", [ss_map[orig_idx]])
                if p:
                    pairs.append((f"ss{len(pairs) + 1}", p))
        else:
            for seq_idx in range(1, max_screenshots + 1):
                p = get_slot(f"ss{seq_idx}", [f"{base}/screenlg{seq_idx}.jpg"])
                if not p:
                    break
                pairs.append((f"ss{seq_idx}", p))
        if pairs:
            slots_out["screenshots"] = pairs

    return tid_hex, slots_out


def pack_asset_file(packer_exe: str, out_asset: str, slot_pairs: list[tuple[str, str]]) -> bool:
    if os.path.isfile(out_asset) and os.path.getsize(out_asset) >= 2048:
        return True
    cmd = [packer_exe, "pack", out_asset] + [f"{slot}={path}" for slot, path in slot_pairs]
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return res.returncode == 0 and os.path.isfile(out_asset) and os.path.getsize(out_asset) >= 2048


def upload_import_mode(
    ftp: PersistentFTP,
    import_root: str,
    tid_hex: str,
    fetched_by_type: dict[str, list[tuple[str, str]]],
) -> int:
    remote_dir = f"{import_root}/{tid_hex}"
    ftp.ensure_dir(remote_dir)
    slot_to_filename = {
        "boxart": "cover",
        "background": "background",
        "icon": "icon",
        "banner": "banner",
    }
    count = 0
    for pairs in fetched_by_type.values():
        for slot, local_path in pairs:
            stem = slot_to_filename.get(slot)
            if not stem and slot.startswith("ss"):
                stem = "screenshot" + slot[2:]
            if not stem:
                continue
            ext = ".png" if local_path.lower().endswith(".png") else ".jpg"
            with open(local_path, "rb") as f:
                ftp.stor_bytes(remote_dir, f"{stem}{ext}", f.read())
            count += 1
    return count


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Parallel Xbox 360 artwork sync for Aurora (covers, backgrounds, icons/banners, screenshots — no video)."
    )
    parser.add_argument("--host", required=True, help="Xbox 360 FTP host/IP")
    parser.add_argument("--port", type=int, default=21, help="FTP port (default: 21)")
    parser.add_argument("--user", default="xboxftp", help="FTP username (default: xboxftp)")
    parser.add_argument("--password", default="xboxftp", help="FTP password (default: xboxftp)")
    parser.add_argument(
        "--types",
        default=",".join(ALL_TYPES),
        help="Comma-separated artwork types: cover,background,iconbanner,screenshots (default: all)",
    )
    parser.add_argument("--max-screenshots", type=int, default=6, help="Max screenshots per game (1..20, default: 6)")
    parser.add_argument("--mode", choices=["asset", "import"], default=None, help="Output mode (default: asset if --packer, else import)")
    parser.add_argument("--packer", default="", help="Path to aurora_asset_packer.exe (required for --mode asset)")
    parser.add_argument("--workers", type=int, default=12, help="Parallel download/pack workers (default: 12)")
    parser.add_argument("--cache-dir", default=".art_cache", help="Local download & packed asset cache directory")
    parser.add_argument("--import-root", default=DEFAULT_IMPORT_ROOT, help="Aurora Import folder for --mode import")
    parser.add_argument("--title-id", default="", help="Optional comma-separated 8-hex TitleIDs to inspect/sync cleanly")
    parser.add_argument("--limit", type=int, default=0, help="Optional cap on TitleIDs to process (0 = all)")
    parser.add_argument("--apply", action="store_true", help="Upload assets to Xbox 360 over FTP (default is dry-run)")
    args = parser.parse_args()

    wanted_types = {t.strip().lower() for t in args.types.split(",") if t.strip()}
    unknown = wanted_types - set(ALL_TYPES)
    if unknown:
        parser.error(f"Unsupported --types entries: {', '.join(sorted(unknown))}")

    mode = args.mode or ("asset" if args.packer else "import")
    if mode == "asset" and (not args.packer or not os.path.isfile(args.packer)):
        parser.error("--mode asset requires an existing --packer path to aurora_asset_packer.exe")

    max_ss = max(1, min(20, args.max_screenshots))
    filter_tids = {t.strip().upper().removeprefix("0X") for t in args.title_id.split(",") if t.strip()}

    ftp = PersistentFTP(args.host, args.port, args.user, args.password)
    try:
        db_bytes = ftp.retr_bytes(CONTENT_DB_PATH)
        folders = load_content_folders(db_bytes)
        if filter_tids:
            folders = [rec for rec in folders if rec.title_id_hex in filter_tids]
        scan_gamedata_folders(ftp, folders, wanted_types, args.cache_dir)

        present_counts = {t: sum(1 for r in folders if t in r.present_assets) for t in ALL_TYPES if t in wanted_types}
        missing_by_tid: dict[str, set[str]] = {}
        folders_by_tid: dict[str, list[FolderState]] = {}
        for rec in folders:
            folders_by_tid.setdefault(rec.title_id_hex, []).append(rec)
            miss = wanted_types - rec.present_assets
            if miss:
                missing_by_tid.setdefault(rec.title_id_hex, set()).update(miss)

        target_tids = sorted(missing_by_tid)
        if args.limit > 0:
            target_tids = target_tids[: args.limit]

        print(f"Scanned GameData entries     : {len(folders)} ({len(folders_by_tid)} unique TitleIDs)")
        for t in ALL_TYPES:
            if t in wanted_types:
                have = present_counts[t]
                print(f"  {t:<13} ({ASSET_PREFIX[t]}*.asset): {have} present / {len(folders) - have} missing")
        print(f"Unique TitleIDs needing fetch: {len(target_tids)}")

        unity_limiter = AdaptiveRateLimiter(max_workers=min(4, max(1, args.workers)))
        fetched: dict[str, dict[str, list[tuple[str, str]]]] = {}
        with ThreadPoolExecutor(max_workers=max(1, args.workers)) as pool:
            futs = {
                pool.submit(
                    fetch_title_images,
                    tid,
                    missing_by_tid[tid],
                    args.cache_dir,
                    max_ss,
                    unity_limiter,
                ): tid
                for tid in target_tids
            }
            for fut in as_completed(futs):
                tid_hex, slots_by_type = fut.result()
                if slots_by_type:
                    fetched[tid_hex] = slots_by_type

        fetched_counts = {t: sum(1 for m in fetched.values() if t in m) for t in ALL_TYPES if t in wanted_types}
        print("Downloaded / cached image sets:")
        for t in ALL_TYPES:
            if t in wanted_types:
                print(f"  {t:<13}: {fetched_counts[t]} titles")

        if mode == "asset":
            pack_jobs: list[tuple[str, str, str, list[tuple[str, str]]]] = []
            for tid_hex, slots_by_type in fetched.items():
                for t, pairs in slots_by_type.items():
                    asset_name = f"{ASSET_PREFIX[t]}{tid_hex}.asset"
                    out_path = os.path.join(args.cache_dir, tid_hex, asset_name)
                    pack_jobs.append((tid_hex, t, out_path, pairs))

            packed_assets: list[tuple[str, str, str]] = []
            pack_errors = 0
            with ThreadPoolExecutor(max_workers=max(1, min(8, args.workers))) as pool:
                pfuts = {
                    pool.submit(pack_asset_file, args.packer, out_path, pairs): (tid_hex, t, out_path)
                    for tid_hex, t, out_path, pairs in pack_jobs
                }
                for fut in as_completed(pfuts):
                    tid_hex, t, out_path = pfuts[fut]
                    if fut.result():
                        packed_assets.append((tid_hex, t, out_path))
                    else:
                        pack_errors += 1

            print(f"Packed native RXEA .asset files: {len(packed_assets)} (pack errors: {pack_errors})")

            uploaded_counts = {t: 0 for t in wanted_types}
            upload_errors = 0
            if args.apply:
                for tid_hex, t, local_asset in sorted(packed_assets):
                    asset_name = os.path.basename(local_asset)
                    with open(local_asset, "rb") as f:
                        payload = f.read()
                    for rec in folders_by_tid.get(tid_hex, []):
                        if t in rec.present_assets:
                            continue
                        remote_dir = f"{GAMEDATA_ROOT}/{rec.folder_name}"
                        try:
                            ftp.ensure_dir(remote_dir)
                            ftp.stor_bytes(remote_dir, asset_name, payload)
                            uploaded_counts[t] += 1
                        except Exception as exc:
                            upload_errors += 1
                            print(f"[WARN] FTP upload failed for {rec.folder_name}/{asset_name}: {exc}")

                print(f"[APPLY] Uploaded directly to {GAMEDATA_ROOT} (upload errors: {upload_errors}):")
                for t in ALL_TYPES:
                    if t in wanted_types:
                        print(f"  {ASSET_PREFIX[t]} ({t}): {uploaded_counts[t]} uploaded")
                print("Reload/restart Aurora on the Xbox 360 to display all new artwork.")
            else:
                print("[DRY-RUN] Re-run with --apply to upload packed .asset files into GameData.")
        else:
            if args.apply:
                ftp.ensure_dir(args.import_root)
                total_files = 0
                for tid_hex in sorted(fetched):
                    total_files += upload_import_mode(ftp, args.import_root, tid_hex, fetched[tid_hex])
                print(f"[APPLY] Staged {total_files} image files across {len(fetched)} folders in {args.import_root}.")
                print("Next on Xbox 360: Open Aurora -> Start -> Settings -> Assets -> Import.")
            else:
                print("[DRY-RUN] Re-run with --apply to stage images into Aurora User/Import.")
    finally:
        ftp.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
