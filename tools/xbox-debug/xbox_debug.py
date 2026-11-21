#!/usr/bin/env python3
"""Remote live debug client for the Shelf Xbox 360 launcher (SHELFDBG/1 on TCP 7331)."""

from __future__ import annotations

import argparse
import http.client
import json
import os
import socket
import sys
import time
import urllib.error
import urllib.request

DEFAULT_PORT = 7331
DEFAULT_XBDM_PORT = 730
END_SENTINEL = ".END"
BANNER_PREFIX = "SHELFDBG/"


class XboxDebugError(RuntimeError):
    """Raised when the console debug connection or protocol fails."""


class XboxDebugClient:
    """Line-oriented TCP client for the embedded Shelf debug server."""

    def __init__(self, host: str, port: int = DEFAULT_PORT, timeout: float = 4.0) -> None:
        self.host = host
        self.port = port
        self.timeout = timeout
        self._sock: socket.socket | None = None
        self._rx_buf = bytearray()
        self.banner: str = ""

    def __enter__(self) -> XboxDebugClient:
        self.connect()
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def connect(self) -> str:
        try:
            sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        except OSError as exc:
            raise XboxDebugError(
                f"Failed to connect to {self.host}:{self.port}: {exc}"
            ) from exc
        sock.settimeout(self.timeout)
        self._sock = sock
        try:
            banner = self._read_line()
            if not banner.startswith(BANNER_PREFIX):
                raise XboxDebugError(f"Unexpected debug banner: {banner!r}")
        except BaseException:
            self._force_close()
            raise
        self.banner = banner
        return banner

    def _force_close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None
        self._rx_buf.clear()

    def close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.sendall(b"quit\n")
            except OSError:
                pass
            self._force_close()
        else:
            self._rx_buf.clear()

    def _read_line(self) -> str:
        if self._sock is None:
            raise XboxDebugError("Debug client is not connected")
        while True:
            nl_idx = self._rx_buf.find(b"\n")
            if nl_idx != -1:
                raw = bytes(self._rx_buf[:nl_idx])
                del self._rx_buf[: nl_idx + 1]
                return raw.rstrip(b"\r").decode("utf-8", errors="replace")
            try:
                chunk = self._sock.recv(4096)
            except OSError as exc:
                raise XboxDebugError(
                    f"Socket read error from {self.host}:{self.port}: {exc}"
                ) from exc
            if not chunk:
                raise XboxDebugError(
                    f"Connection closed by {self.host}:{self.port} before response end"
                )
            self._rx_buf.extend(chunk)

    def command(self, line: str) -> list[str]:
        """Send a single command line and return reply lines up to `.END`."""
        if self._sock is None:
            raise XboxDebugError("Debug client is not connected")
        if "\n" in line or "\r" in line:
            raise XboxDebugError("Command cannot contain CR/LF newlines")
        clean = line.strip()
        if not clean:
            return []
        try:
            self._sock.sendall((clean + "\n").encode("utf-8"))
        except OSError as exc:
            raise XboxDebugError(
                f"Socket write error to {self.host}:{self.port}: {exc}"
            ) from exc
        out: list[str] = []
        while True:
            reply_line = self._read_line()
            if reply_line == END_SENTINEL:
                return out
            out.append(reply_line)


def probe_xbdm(host: str, port: int = DEFAULT_XBDM_PORT, timeout: float = 3.0, command: str | None = None) -> list[str]:
    """Connect to optional DashLaunch/devkit XBDM listener on port 730."""
    try:
        sock = socket.create_connection((host, port), timeout=timeout)
    except OSError as exc:
        raise XboxDebugError(f"XBDM port {host}:{port} unreachable: {exc}") from exc
    sock.settimeout(timeout)
    buf = bytearray()

    def read_line() -> str:
        while True:
            nl = buf.find(b"\n")
            if nl != -1:
                line = bytes(buf[:nl]).rstrip(b"\r").decode("utf-8", errors="replace")
                del buf[: nl + 1]
                return line
            chunk = sock.recv(4096)
            if not chunk:
                raise XboxDebugError("XBDM closed connection")
            buf.extend(chunk)

    try:
        lines = [read_line()]
        if command:
            sock.sendall((command.strip() + "\r\n").encode("utf-8"))
            resp = read_line()
            lines.append(resp)
            if resp.startswith(("202-", "203-")):
                while True:
                    item = read_line()
                    lines.append(item)
                    if item == ".":
                        break
        return lines
    except OSError as exc:
        raise XboxDebugError(f"XBDM read/write error on {host}:{port}: {exc}") from exc
    finally:
        sock.close()


def fetch_controller_diag(controller_url: str, timeout: float = 2.0) -> list[dict[str, object]]:
    """Fetch recent launcher diagnostic stages from xbox-guard HTTP API."""
    url = controller_url.rstrip("/") + "/api/v1/launcher/diag/recent"
    req = urllib.request.Request(url, method="GET")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = json.loads(resp.read().decode("utf-8"))
    if isinstance(payload, list):
        return payload
    return []


def build_command_line(args: argparse.Namespace) -> str | None:
    cmd = args.subcommand
    if cmd in ("ping", "status", "gpu", "modes", "reboot"):
        return cmd
    if cmd == "log":
        return f"log {args.lines}" if args.lines is not None else "log"
    if cmd == "reg":
        return f"reg {args.offset}"
    if cmd == "dve":
        return f"dve {args.register}"
    if cmd == "vdmode":
        return f"vdmode {args.flags}"
    if cmd == "solid":
        return f"solid {args.color}"
    if cmd == "cpu":
        return f"cpu {args.color}"
    if cmd == "dash":
        return "dash force" if args.force else "dash"
    if cmd == "aurora":
        return "aurora force" if args.force else "aurora"
    if cmd == "cmd":
        joined = " ".join(args.raw).strip()
        if not joined:
            raise XboxDebugError("'cmd' requires a command string")
        return joined
    return None


def resolve_host(args: argparse.Namespace) -> str:
    host = args.host or os.environ.get("XBOX_HOST", "").strip()
    if not host:
        raise XboxDebugError("Target Xbox IP required via --host or XBOX_HOST env var")
    return host


def run_repl(client: XboxDebugClient) -> int:
    print(f"Connected: {client.banner}", file=sys.stderr)
    while True:
        try:
            line = input("xbox-dbg> ").strip()
        except EOFError:
            print()
            return 0
        except KeyboardInterrupt:
            print()
            return 0
        if not line:
            continue
        lines = client.command(line)
        for out in lines:
            print(out)
        if line.lower().split()[0] in ("quit", "exit", "dash", "aurora", "reboot"):
            return 0


def _diff_new_events(
    prev_sig: list[tuple[str, str, str]],
    curr_events: list[dict[str, object]],
) -> tuple[list[tuple[str, str, str]], list[dict[str, object]]]:
    curr_sig = [
        (str(e.get("at", "")), str(e.get("stage", "")), str(e.get("detail", "")))
        for e in curr_events
    ]
    if not prev_sig:
        return curr_sig, curr_events[-3:]
    if curr_sig == prev_sig:
        return curr_sig, []
    max_overlap = min(len(prev_sig), len(curr_sig))
    for k in range(max_overlap, 0, -1):
        if prev_sig[-k:] == curr_sig[:k]:
            return curr_sig, curr_events[k:]
    return curr_sig, curr_events[-3:]


def run_watch(args: argparse.Namespace) -> int:
    if args.interval <= 0:
        raise XboxDebugError("--interval must be greater than 0")
    host = args.host or os.environ.get("XBOX_HOST", "").strip()
    if not host and not args.controller_url:
        raise XboxDebugError("watch requires --host / XBOX_HOST or --controller-url")
    last_sig: list[tuple[str, str, str]] = []
    iterations = 0
    while args.count == 0 or iterations < args.count:
        iterations += 1
        stamp = time.strftime("%H:%M:%S")
        if host:
            try:
                with XboxDebugClient(host, args.port, timeout=args.timeout) as client:
                    reply = client.command("ping")
                    print(f"[{stamp}] tcp/{args.port}: {' | '.join(reply)}")
            except XboxDebugError as exc:
                print(f"[{stamp}] tcp/{args.port}: unreachable ({exc})")
        if args.controller_url:
            try:
                events = fetch_controller_diag(args.controller_url, timeout=args.timeout)
                last_sig, new_events = _diff_new_events(last_sig, events)
                if new_events:
                    summary = "; ".join(
                        f"{e.get('stage', '?')}={e.get('detail', '')}" for e in new_events
                    )
                    print(f"[{stamp}] controller diag ({len(events)}): {summary}")
            except (
                urllib.error.URLError,
                http.client.HTTPException,
                OSError,
                json.JSONDecodeError,
                UnicodeDecodeError,
            ) as exc:
                print(f"[{stamp}] controller diag error: {exc}")
        if args.count != 0 and iterations >= args.count:
            break
        time.sleep(args.interval)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Live remote debugger CLI for Shelf Xbox 360 launcher (TCP port 7331)."
    )
    parser.add_argument(
        "--host",
        default="",
        help="Xbox 360 LAN IP address (or set XBOX_HOST env var).",
    )
    parser.add_argument("--port", type=int, default=DEFAULT_PORT, help="Debug TCP port (default: 7331).")
    parser.add_argument("--timeout", type=float, default=4.0, help="Socket timeout in seconds.")

    sub = parser.add_subparsers(dest="subcommand", required=True)
    sub.add_parser("ping", help="Check debug server liveness, uptime, stage, and frame count.")
    sub.add_parser("status", help="Dump full Vd display info, scanout regs, and panic status.")

    p_log = sub.add_parser("log", help="Fetch in-memory boot/runtime log lines.")
    p_log.add_argument("--lines", type=int, default=None, help="Number of tail lines (1..128).")

    sub.add_parser("gpu", help="Dump Xenos GPU ring, status, and display MMIO registers.")

    p_reg = sub.add_parser("reg", help="Read one Xenos GPU MMIO register (word offset in hex).")
    p_reg.add_argument("offset", help="Hex word offset, e.g. 1841 or 0x1844.")

    p_dve = sub.add_parser("dve", help="Read one video encoder register via VdReadDVERegisterUlong.")
    p_dve.add_argument("register", help="Hex register index, e.g. 00 or 0x1a.")

    sub.add_parser("modes", help="List modes reported by VdEnumerateVideoModes.")

    p_vd = sub.add_parser("vdmode", help="Call VdSetDisplayMode(flags) live on console.")
    p_vd.add_argument("flags", help="Hex flags passed to VdSetDisplayMode (e.g. 0 or 0x10).")

    p_solid = sub.add_parser("solid", help="Override GPU render with solid clear color or 'off'.")
    p_solid.add_argument("color", help="6-digit hex RRGGBB or 'off'.")

    p_cpu = sub.add_parser("cpu", help="Override scanout with direct CPU color fill or 'off'.")
    p_cpu.add_argument("color", help="6-digit hex RRGGBB or 'off'.")

    p_dash = sub.add_parser("dash", help="Exit Shelf back to dashboard.")
    p_dash.add_argument("--force", action="store_true", help="Force immediate XAM call if UI loop is hung.")

    p_aur = sub.add_parser("aurora", help="Launch configured Aurora.xex.")
    p_aur.add_argument("--force", action="store_true", help="Force immediate launch if UI loop is hung.")

    sub.add_parser("reboot", help="Trigger quiesced console reboot.")

    p_cmd = sub.add_parser("cmd", help="Send a raw debug command string.")
    p_cmd.add_argument("raw", nargs="+", help="Raw command tokens.")

    sub.add_parser("repl", help="Open an interactive debug shell.")

    p_watch = sub.add_parser("watch", help="Poll live TCP ping and/or xbox-guard telemetry.")
    p_watch.add_argument("--interval", type=float, default=1.0, help="Poll interval seconds.")
    p_watch.add_argument("--controller-url", default="", help="Optional xbox-guard base URL.")
    p_watch.add_argument("--count", type=int, default=0, help="Max iterations (0 = run until Ctrl-C).")

    p_xbdm = sub.add_parser("xbdm", help="Probe optional port 730 XBDM plugin if loaded.")
    p_xbdm.add_argument("--xbdm-port", type=int, default=DEFAULT_XBDM_PORT, help="XBDM port (default: 730).")
    p_xbdm.add_argument("xbdm_cmd", nargs="*", help="Optional XBDM command (e.g. 'dbgname' or 'magicboot cold').")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.subcommand == "watch":
            return run_watch(args)
        host = resolve_host(args)
        if args.subcommand == "xbdm":
            cmd_str = " ".join(args.xbdm_cmd).strip() or None
            for line in probe_xbdm(host, port=args.xbdm_port, timeout=args.timeout, command=cmd_str):
                print(line)
            return 0
        with XboxDebugClient(host, args.port, timeout=args.timeout) as client:
            if args.subcommand == "repl":
                return run_repl(client)
            line = build_command_line(args)
            if line is None:
                raise XboxDebugError(f"Unsupported subcommand: {args.subcommand}")
            for out_line in client.command(line):
                print(out_line)
        return 0
    except XboxDebugError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
