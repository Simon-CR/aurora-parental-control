#!/usr/bin/env python3
"""Fast parallel XboxUnity box-art fetcher + Aurora /Game/User/Import FTP stager."""

from __future__ import annotations

import argparse
import ftplib
import io
import json
import os
import sqlite3
import tempfile
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass


CONTENT_DB_PATH = "/Game/Data/Databases/content.db"
GAMEDATA_ROOT = "/Game/Data/GameData"
DEFAULT_IMPORT_ROOT = "/Game/User/Import"
COVER_MIN_ASSET_BYTES = 24_576
COVER_INFO_URL = "http://xboxunity.net/Resources/Lib/CoverInfo.php?titleid={tid}"
COVER_IMG_URL = "http://xboxunity.net/Resources/Lib/Cover.php?size={size}&cid={cid}"
USER_AGENT = "Aurora/0.7b"


@dataclass(frozen=True)
class TitleRecord:
    title_id_hex: str
    db_id_hex: str
    title_name: str


class PersistentFTP:
    """Maintains a single passive FTP connection with clean transparent reconnect."""

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
            except (ftplib.error_temp, ftplib.error_perm) as exc:
                # Propagate permanent 5xx errors (e.g. missing path) directly except timeouts.
                if isinstance(exc, ftplib.error_perm) and not str(exc).startswith("550"):
                    last_err = exc
                    self.connect()
                    continue
                raise
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

    def list_dir(self, remote_dir: str) -> list[tuple[bool, str, int]]:
        """Returns list of (is_dir, name, size) tuples."""
        def op(ftp: ftplib.FTP) -> list[tuple[bool, str, int]]:
            lines: list[str] = []
            ftp.retrlines(f"LIST {remote_dir}", lines.append)
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
                self._run(lambda f, d=cur: f.mkd(d))
            except ftplib.error_perm:
                pass

    def stor_bytes(self, remote_path: str, data: bytes) -> None:
        self._run(lambda f: f.storbinary(f"STOR {remote_path}", io.BytesIO(data)))

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


def load_content_titles(db_bytes: bytes) -> dict[str, list[TitleRecord]]:
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

    by_title: dict[str, list[TitleRecord]] = {}
    for db_id, raw_tid, name in rows:
        tid_hex = f"{int(raw_tid) & 0xFFFFFFFF:08X}"
        if tid_hex == "00000000":
            continue
        db_hex = f"{int(db_id) & 0xFFFFFFFF:08X}"
        rec = TitleRecord(title_id_hex=tid_hex, db_id_hex=db_hex, title_name=(name or tid_hex))
        by_title.setdefault(tid_hex, []).append(rec)
    return by_title


class AdaptiveRateLimiter:
    """Caps concurrency and backs off temporarily when XboxUnity returns 429/5xx."""

    def __init__(self, max_workers: int) -> None:
        self.max_workers = max(1, max_workers)
        self.min_workers = min(4, self.max_workers)
        self._lock = threading.Lock()
        self._active_limit = self.max_workers
        self._cooldown_until = 0.0

    def wait_slot(self) -> None:
        while True:
            with self._lock:
                delay = self._cooldown_until - time.monotonic()
            if delay <= 0:
                return
            time.sleep(min(delay, 1.0))

    def penalize(self, backoff_sec: float) -> None:
        with self._lock:
            self._active_limit = max(self.min_workers, self._active_limit // 2)
            self._cooldown_until = max(self._cooldown_until, time.monotonic() + backoff_sec)


def http_get(url: str, timeout: int = 12) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read()


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

    best = max(covers, key=score)
    cid = best.get("CoverID")
    return str(cid) if cid else None


def is_valid_image(data: bytes) -> bool:
    return len(data) > 512 and (data.startswith(b"\x89PNG\r\n\x1a\n") or data.startswith(b"\xff\xd8\xff"))


def fetch_cover_for_title(
    tid_hex: str, cache_dir: str, limiter: AdaptiveRateLimiter
) -> tuple[str, str, bytes | None]:
    """Returns (tid_hex, status, image_bytes) where status is cached|downloaded|missing|error."""
    cache_path = os.path.join(cache_dir, tid_hex, "cover.png")
    if os.path.isfile(cache_path) and os.path.getsize(cache_path) > 512:
        with open(cache_path, "rb") as f:
            data = f.read()
        if is_valid_image(data):
            return tid_hex, "cached", data

    for attempt in range(3):
        limiter.wait_slot()
        try:
            raw_info = http_get(COVER_INFO_URL.format(tid=tid_hex))
            info = json.loads(raw_info.decode("utf-8", "replace"))
            cid = pick_best_cover_id(info)
            if not cid:
                return tid_hex, "missing", None

            img_bytes = b""
            for size in ("large", "small"):
                try:
                    candidate = http_get(COVER_IMG_URL.format(size=size, cid=cid))
                    if is_valid_image(candidate):
                        img_bytes = candidate
                        break
                except urllib.error.HTTPError as exc:
                    if exc.code in (429, 500, 502, 503, 504):
                        raise
            if not img_bytes:
                return tid_hex, "missing", None

            os.makedirs(os.path.dirname(cache_path), exist_ok=True)
            with open(cache_path, "wb") as f:
                f.write(img_bytes)
            return tid_hex, "downloaded", img_bytes
        except urllib.error.HTTPError as exc:
            if exc.code in (429, 500, 502, 503, 504) and attempt < 2:
                limiter.penalize(1.5 * (2**attempt))
                continue
            return tid_hex, "error", None
        except Exception:
            if attempt < 2:
                time.sleep(0.75 * (attempt + 1))
                continue
            return tid_hex, "error", None

    return tid_hex, "error", None


def scan_existing_covers(
    ftp: PersistentFTP, by_title: dict[str, list[TitleRecord]], import_root: str
) -> tuple[set[str], set[str]]:
    covered: set[str] = set()
    staged: set[str] = set()

    gamedata_dirs: set[str] = set()
    try:
        for is_dir, name, _ in ftp.list_dir(GAMEDATA_ROOT):
            if is_dir:
                gamedata_dirs.add(name.upper())
    except ftplib.error_perm:
        pass

    for tid_hex, records in by_title.items():
        for rec in records:
            folder_name = f"{rec.title_id_hex}_{rec.db_id_hex}"
            if folder_name.upper() not in gamedata_dirs:
                continue
            try:
                files = ftp.list_dir(f"{GAMEDATA_ROOT}/{folder_name}")
            except ftplib.error_perm:
                continue
            if any(
                (not is_dir) and name.upper().startswith("GC") and name.lower().endswith(".asset") and size > COVER_MIN_ASSET_BYTES
                for is_dir, name, size in files
            ):
                covered.add(tid_hex)
                break

    try:
        for is_dir, name, _ in ftp.list_dir(import_root):
            if is_dir and len(name) == 8:
                tid_hex = name.upper()
                try:
                    sub = ftp.list_dir(f"{import_root}/{name}")
                except ftplib.error_perm:
                    continue
                if any((not d) and n.lower() in {"cover.png", "cover.jpg"} and sz > 512 for d, n, sz in sub):
                    staged.add(tid_hex)
    except ftplib.error_perm:
        pass

    return covered, staged


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Download missing Xbox 360 box covers from XboxUnity in parallel and pre-stage into Aurora User/Import."
    )
    parser.add_argument("--host", required=True, help="Xbox 360 FTP host/IP")
    parser.add_argument("--port", type=int, default=21, help="FTP port (default: 21)")
    parser.add_argument("--user", default="xboxftp", help="FTP username (default: xboxftp)")
    parser.add_argument("--password", default="xboxftp", help="FTP password (default: xboxftp)")
    parser.add_argument("--workers", type=int, default=16, help="Parallel XboxUnity workers (default: 16)")
    parser.add_argument("--cache-dir", default=".art_cache", help="Local image cache directory")
    parser.add_argument("--import-root", default=DEFAULT_IMPORT_ROOT, help="Aurora Import folder over FTP")
    parser.add_argument("--limit", type=int, default=0, help="Optional cap on titles to process (0 = all)")
    parser.add_argument("--apply", action="store_true", help="Upload staged covers to console via FTP (default is dry-run)")
    args = parser.parse_args()

    ftp = PersistentFTP(args.host, args.port, args.user, args.password)
    try:
        db_bytes = ftp.retr_bytes(CONTENT_DB_PATH)
        by_title = load_content_titles(db_bytes)
        covered, staged = scan_existing_covers(ftp, by_title, args.import_root)
        missing = sorted(tid for tid in by_title if tid not in covered and tid not in staged)
        if args.limit > 0:
            missing = missing[: args.limit]

        print(f"Unique TitleIDs in content.db : {len(by_title)}")
        print(f"Already covered (>24KB asset)  : {len(covered)}")
        print(f"Already staged in User/Import  : {len(staged)}")
        print(f"Missing covers to fetch        : {len(missing)}")

        limiter = AdaptiveRateLimiter(args.workers)
        fetched: list[tuple[str, bytes]] = []
        counts = {"cached": 0, "downloaded": 0, "missing": 0, "error": 0}

        if missing:
            with ThreadPoolExecutor(max_workers=max(1, args.workers)) as pool:
                futs = {
                    pool.submit(fetch_cover_for_title, tid, args.cache_dir, limiter): tid
                    for tid in missing
                }
                for fut in as_completed(futs):
                    tid_hex, status, data = fut.result()
                    counts[status] = counts.get(status, 0) + 1
                    if data is not None:
                        fetched.append((tid_hex, data))

        uploaded = 0
        upload_errors = 0
        if args.apply and fetched:
            ftp.ensure_dir(args.import_root)
            for tid_hex, data in sorted(fetched, key=lambda x: x[0]):
                target_dir = f"{args.import_root}/{tid_hex}"
                try:
                    ftp.ensure_dir(target_dir)
                    ftp.stor_bytes(f"{target_dir}/cover.png", data)
                    uploaded += 1
                except Exception as exc:
                    upload_errors += 1
                    print(f"[WARN] Failed upload for {tid_hex}: {exc}")

        mode = "APPLY" if args.apply else "DRY-RUN"
        print(f"[{mode}] Downloaded new covers  : {counts['downloaded']} (from cache: {counts['cached']})")
        print(f"[{mode}] Not found on XboxUnity : {counts['missing']} (HTTP errors: {counts['error']})")
        if args.apply:
            print(f"[{mode}] Uploaded to {args.import_root}: {uploaded} (upload errors: {upload_errors})")
            print("Next on Xbox 360: Open Aurora -> Start -> Settings -> Assets -> Import.")
        else:
            print("[DRY-RUN] Re-run with --apply to stage covers into Aurora User/Import.")
    finally:
        ftp.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
