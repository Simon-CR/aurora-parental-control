# Shelf: Plex-Style Xbox 360 Launcher

**Shelf** (`shelf_launcher.xex`) is an open-source, Plex-style Xbox 360 game launcher compiled natively with [`landaire/xenon-forge`](https://github.com/landaire/xenon-forge) in Rust. It displays a persistent left category sidebar (including all 14 Xbox 360 genres, Recently Played, Favorites, Uncategorized, and Open Aurora) paired with a 5-column poster wall, bottom details strip, and built-in **Xbox Guard** screen-time integration.

---

## How It Works Together

```text
  [1] Xbox 360 (Aurora content.db)
        |  (read-only FTP export via launcher_sync.py)
        v
  [2] launcher-cache/  (on xbox-guard host)
        |-- catalog.tsv          (single 15-column game list)
        |-- posters/<TID>.jpg    (pre-sized 180x248 JPEG cards)
        |-- backdrops/<TID>.jpg  (pre-sized 640x360 hero banners)
        |
        v  LAN HTTP (plus offline mirror in Hdd1:\PeerPressure\Launcher\cache\)
  [3] shelf_launcher.xex  (runs natively on Xbox 360 GPU)
        |-- Keeps up to 48 posters + 1 backdrop in graphics memory at a time
        |-- Polls xbox-guard for daily screen-time limits & PIN / More-Time prompts
        |-- Releases GPU engines and launches loose .xex, XBLA, or USB GOD games
```

### Why Pre-Sized JPEG Thumbnails?

- Aurora stores its covers and backgrounds in proprietary packed Xbox 360 GPU `.asset` files (`RXEA`), which require unpacking or repacking on a PC.
- Uncompressed 720p artwork for 745+ games would quickly exhaust the Xbox 360's 512 MiB unified memory.
- Instead, `tools/launcher-sync/launcher_sync.py` sizes covers once to **180 x 248** posters and **640 x 360** backdrops. Only missing titles are fetched when you add new games.
- `shelf_launcher.xex` reads cached JPEGs straight from `Hdd1:\PeerPressure\Launcher\cache\` (or fetches any missing thumbnail from `xbox-guard` on first view and caches it onto `Hdd1:` automatically), keeping at most **48 posters and 1 backdrop** decoded in graphics memory at once.

---

## Controller Controls

| Input | Action |
|---|---|
| `D-Pad` / `Left Stick` | Move focus across the left sidebar tabs or 5-column poster wall |
| `A` | Open highlighted sidebar category / Launch focused game / Confirm option |
| `B` | Return focus to the sidebar or close an open modal |
| `LB` / `RB` | Switch to previous / next category tab from anywhere |
| `LT` / `RT` | Cycle quick first-letter filter (`All` -> `0-9` -> `A` through `Z`) |
| `X` | Toggle Favorite on the focused game (syncs to `xbox-guard` & `userstate.tsv`) |
| `Y` | Cycle sort order (`A-Z` -> `Recently Played` -> `Release Year` -> `Favorites First`) |
| `Start` | Launch Aurora (`Hdd1:\PeerPressure\Dashboard\Aurora.xex`) for FTP or settings |

---

## Built-In Parental Control Integration

1. `shelf_launcher.xex` reads your existing `screentime.cfg` from `Hdd1:\PeerPressure\Launcher\screentime.cfg`, `Hdd1:\PeerPressure\SideLoading\screentime.cfg`, `Hdd1:\screentime.cfg`, or `Usb0:\screentime.cfg`.
2. While browsing, it displays a live status badge in the top-right corner (`Xm left today`, `Paused by Parent`, `Time Limit Reached`, or `Offline Cache Mode` if no server is configured).
3. If screen time is paused, outside allowed hours, or at the daily limit, pressing `A` on a game opens a friendly lock prompt where kids can send an **Ask Parent** request (`+15 min`, `+30 min`, or `Resume`) straight to Home Assistant / Web UI, or a parent can enter the 4-button controller PIN (`parent_pin`).
4. Once a game is running, `screentime_dll.xex` (or `screentime_sys.xex`) continues counting active play time and showing in-game warnings.

---

## Launching Loose `.xex`, XBLA, and USB GOD Games

- **Loose `.xex` titles** (such as `Usb0:\games\portal 2\default.xex` or `Aurora.xex`) are started with `XamLoaderLaunchTitleEx` using the game's directory as its mount root, with an `ObCreateSymbolicLink` drive fallback.
- **Packaged XBLA (`000d0000`) and USB Games-on-Demand (`00007000`) containers** are launched in place via XAM's `XamContentLaunchImageFromFileInternal` (`xam.xex` ordinal `0x257`), with an `XamContentOpenFile` (`con:`) mount fallback—so games stored under folders like `Usb0:\games\...` do **not** need to be moved into `\content\0000000000000000\` to boot.
- Before handing control to the kernel loader, Shelf shuts down background network I/O and explicitly drops `Painter`, `Gpu`, and `Display` so `VdShutdownEngines` cleanly returns the Xenos GPU to the kernel.

---

## Safe Single-Test Staging Guide

Do **not** replace `PeerPressure/System/Config.ini` right away. Test Shelf safely by launching it from inside Aurora first:

### 1. Sync Library & Thumbnails on Host

```bash
pip install pillow
python tools/launcher-sync/launcher_sync.py \
  --host <xbox-ip> \
  --out-dir ./launcher-cache \
  --fetch-art \
  --push-server http://<xbox-guard-ip>:8420 \
  --seed-ftp
```

Run `xbox-guard` pointing at the same `--launcher-dir ./launcher-cache` so the server can serve `catalog.tsv`, posters, and backdrops over HTTP as well.

### 2. Stage `shelf_launcher.xex` (Non-Destructive)

Upload via FTP to a new folder on `Hdd1:`:
- `Hdd1:\PeerPressure\Launcher\shelf_launcher.xex`
- (Optional) `Hdd1:\PeerPressure\Launcher\screentime.cfg` (or keep using your existing `Hdd1:\PeerPressure\SideLoading\screentime.cfg`)
- Cache seeded by `--seed-ftp` at `Hdd1:\PeerPressure\Launcher\cache\`

Launch **`shelf_launcher.xex`** directly from Aurora's File Manager to verify:
1. Categories, posters, and hero backdrops render smoothly in Warm Espresso colors.
2. One extracted `.xex` game (e.g., Portal 2), one XBLA package, and one USB GOD game boot cleanly.
3. Favorites (`X`), Screen-Time lock prompts, and **Open Aurora (`Start`)** work as expected.

Only after testing confirms all formats launch cleanly should you optionally point `PeerPressure/System/Config.ini` at `shelf_launcher.xex` as your default dashboard.
