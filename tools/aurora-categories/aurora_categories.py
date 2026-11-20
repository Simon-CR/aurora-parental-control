#!/usr/bin/env python3
"""Read-only Aurora content.db genre breakdown & settings.db QuickViews auditor."""

from __future__ import annotations

import argparse
import ftplib
import io
import os
import sqlite3
import tempfile


CONTENT_DB_PATH = "/Game/Data/Databases/content.db"
SETTINGS_DB_PATH = "/Game/Data/Databases/settings.db"

GENRE_BITS: list[tuple[int, str]] = [
    (0x0002, "Action & Adventure"),
    (0x1000, "Classics"),
    (0x0100, "Shooter"),
    (0x2000, "Puzzle & Trivia"),
    (0x0040, "Racing & Flying"),
    (0x0004, "Family"),
    (0x0020, "Platformer"),
    (0x0400, "Sports & Recreation"),
    (0x0200, "Strategy & Simulation"),
    (0x0008, "Fighting"),
    (0x0080, "Role Playing"),
    (0x0800, "Card & Board"),
    (0x0001, "Other"),
    (0x0010, "Music"),
]


def fetch_bytes(ftp: ftplib.FTP, remote_path: str) -> bytes:
    buf = io.BytesIO()
    ftp.retrbinary(f"RETR {remote_path}", buf.write)
    return buf.getvalue()


def query_sqlite_bytes(db_bytes: bytes, query: str) -> list[tuple]:
    fd, tmp_path = tempfile.mkstemp(suffix=".db")
    os.close(fd)
    try:
        with open(tmp_path, "wb") as f:
            f.write(db_bytes)
        con = sqlite3.connect(tmp_path)
        try:
            return con.execute(query).fetchall()
        finally:
            con.close()
    finally:
        if os.path.exists(tmp_path):
            os.remove(tmp_path)


def main() -> int:
    parser = argparse.ArgumentParser(description="Audit Aurora genre counts and QuickViews over read-only FTP.")
    parser.add_argument("--host", required=True, help="Xbox 360 FTP host/IP")
    parser.add_argument("--port", type=int, default=21, help="FTP port (default: 21)")
    parser.add_argument("--user", default="xboxftp", help="FTP username (default: xboxftp)")
    parser.add_argument("--password", default="xboxftp", help="FTP password (default: xboxftp)")
    parser.add_argument("--audit", action="store_true", help="Print full genre breakdown and QuickViews")
    args = parser.parse_args()

    ftp = ftplib.FTP()
    ftp.connect(args.host, args.port, timeout=15)
    ftp.login(args.user, args.password)
    ftp.set_pasv(True)
    try:
        content_bytes = fetch_bytes(ftp, CONTENT_DB_PATH)
        settings_bytes = fetch_bytes(ftp, SETTINGS_DB_PATH)
    finally:
        try:
            ftp.quit()
        except Exception:
            pass

    items = query_sqlite_bytes(content_bytes, "SELECT GenreFlag FROM ContentItems")
    total = len(items)
    flags = [int(r[0] or 0) for r in items]
    categorized = sum(1 for f in flags if f != 0)
    uncategorized = total - categorized

    print(f"Total ContentItems : {total}")
    print(f"Categorized        : {categorized}")
    print(f"Uncategorized (0)  : {uncategorized}")
    print("\nGenre Counts (titles may carry multiple flags):")
    for bit, label in GENRE_BITS:
        cnt = sum(1 for f in flags if (f & bit) != 0)
        print(f"  {label:<24} (0x{bit:04X}): {cnt}")

    qviews = query_sqlite_bytes(
        settings_bytes,
        "SELECT Id, DisplayName, FilterMethod, OrderIndex FROM QuickViews ORDER BY OrderIndex",
    )
    print("\nCurrent Aurora QuickViews:")
    for qid, name, filt, order_idx in qviews:
        print(f"  [{order_idx}] {name} (Id={qid}, Filter={filt!r})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
