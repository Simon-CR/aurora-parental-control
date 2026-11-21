import http.client
import io
import os
import socketserver
import threading
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch

import xbox_debug


class MockShelfHandler(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        try:
            self.wfile.write(b"SHELFDBG/1 ready (type 'help')\n")
            self.wfile.flush()
            while True:
                raw = self.rfile.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", errors="replace").strip()
                self.server.received.append(line)  # type: ignore[attr-defined]
                if line == "ping":
                    self.wfile.write(b"pong uptime_ms=123 stage=15_frame10_ok frame=12 mode=normal\n.END\n")
                elif line.startswith("log"):
                    self.wfile.write(b"[+10ms] [01_main_enter] ok\n[+20ms] [05_vd_info] surf=1280x720\n.END\n")
                elif line == "gpu":
                    self.wfile.write(b"reg[0x1841] (SCANOUT_CTRL) = 0x00000100\nreg[0x1844] (SCANOUT_ADDR) = 0x01000000\n.END\n")
                elif line == "modes":
                    self.wfile.write(b"modes_count=1 total_seen=1\n[0] 1280x720 @ 60.00Hz kind=3 flags=0x00000000 (progressive)\n.END\n")
                elif line.startswith(("solid ", "cpu ")):
                    self.wfile.write(f"OK {line}\n.END\n".encode("utf-8"))
                elif line == "dash force":
                    self.wfile.write(b"OK returning to dashboard (forced)\n.END\n")
                    self.wfile.flush()
                    break
                elif line == "quit":
                    self.server.quit_seen.set()  # type: ignore[attr-defined]
                    self.wfile.write(b"bye\n.END\n")
                    self.wfile.flush()
                    break
                else:
                    self.wfile.write(f"ERR unknown {line}\n.END\n".encode("utf-8"))
                self.wfile.flush()
        except OSError:
            pass


class MockXbdmHandler(socketserver.StreamRequestHandler):
    def handle(self) -> None:
        self.wfile.write(b"201- connected\r\n")
        self.wfile.flush()
        raw = self.rfile.readline()
        if raw:
            self.wfile.write(b"202- multiline response follows\r\nline1\r\n.\r\n")
            self.wfile.flush()


class MockServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True

    def __init__(self, handler: type[socketserver.BaseRequestHandler]) -> None:
        self.received: list[str] = []
        self.quit_seen = threading.Event()
        super().__init__(("127.0.0.1", 0), handler)


class XboxDebugTests(unittest.TestCase):
    def setUp(self) -> None:
        self.srv = MockServer(MockShelfHandler)
        self.port = self.srv.server_address[1]
        self.thread = threading.Thread(target=self.srv.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self) -> None:
        self.srv.shutdown()
        self.srv.server_close()

    def test_client_single_and_multiline_commands(self) -> None:
        with xbox_debug.XboxDebugClient("127.0.0.1", self.port, timeout=2.0) as client:
            self.assertTrue(client.banner.startswith("SHELFDBG/1"))
            self.assertEqual(
                client.command("ping"),
                ["pong uptime_ms=123 stage=15_frame10_ok frame=12 mode=normal"],
            )
            self.assertEqual(
                client.command("log 10"),
                ["[+10ms] [01_main_enter] ok", "[+20ms] [05_vd_info] surf=1280x720"],
            )
            self.assertEqual(len(client.command("gpu")), 2)
            self.assertEqual(len(client.command("modes")), 2)

        self.assertTrue(self.srv.quit_seen.wait(timeout=2.0))
        self.assertIn("quit", self.srv.received)

    def test_cli_subcommands_solid_cpu_and_dash_force(self) -> None:
        for argv, expect in [
            (["--host", "127.0.0.1", "--port", str(self.port), "solid", "C87A4B"], "OK solid C87A4B"),
            (["--host", "127.0.0.1", "--port", str(self.port), "cpu", "off"], "OK cpu off"),
            (["--host", "127.0.0.1", "--port", str(self.port), "dash", "--force"], "OK returning to dashboard (forced)"),
        ]:
            buf = io.StringIO()
            with redirect_stdout(buf):
                rc = xbox_debug.main(argv)
            self.assertEqual(rc, 0)
            self.assertIn(expect, buf.getvalue())

    def test_requires_host_when_env_unset(self) -> None:
        old = os.environ.pop("XBOX_HOST", None)
        try:
            err = io.StringIO()
            with redirect_stderr(err):
                rc = xbox_debug.main(["ping"])
            self.assertEqual(rc, 2)
            self.assertIn("Target Xbox IP required", err.getvalue())
        finally:
            if old is not None:
                os.environ["XBOX_HOST"] = old

    def test_rejects_embedded_newlines_and_cleans_socket_on_bad_banner(self) -> None:
        with xbox_debug.XboxDebugClient("127.0.0.1", self.port, timeout=2.0) as client:
            with self.assertRaises(xbox_debug.XboxDebugError):
                client.command("ping\nstatus")

        class BadBannerHandler(socketserver.StreamRequestHandler):
            def handle(self) -> None:
                self.wfile.write(b"NOT_SHELF\n")
                self.wfile.flush()

        bad_srv = MockServer(BadBannerHandler)
        t = threading.Thread(target=bad_srv.serve_forever, daemon=True)
        t.start()
        try:
            bad_client = xbox_debug.XboxDebugClient("127.0.0.1", bad_srv.server_address[1], timeout=2.0)
            with self.assertRaises(xbox_debug.XboxDebugError):
                bad_client.connect()
            self.assertIsNone(bad_client._sock)
        finally:
            bad_srv.shutdown()
            bad_srv.server_close()

    def test_watch_tracks_rolling_buffer_over_64_and_http_exception(self) -> None:
        base = [{"at": f"t{i}", "stage": f"s{i}", "detail": "ok"} for i in range(64)]
        rolled = base[1:] + [{"at": "t64", "stage": "s64", "detail": "new_ev"}]
        with patch.object(
            xbox_debug,
            "fetch_controller_diag",
            side_effect=[base, http.client.RemoteDisconnected("reset"), rolled],
        ):
            buf = io.StringIO()
            with redirect_stdout(buf):
                rc = xbox_debug.main(
                    [
                        "watch",
                        "--interval",
                        "0.01",
                        "--controller-url",
                        "http://127.0.0.1:1",
                        "--count",
                        "3",
                    ]
                )
        self.assertEqual(rc, 0)
        out = buf.getvalue()
        self.assertIn("controller diag error", out)
        self.assertIn("s64=new_ev", out)

    def test_xbdm_probe(self) -> None:
        xbdm_srv = MockServer(MockXbdmHandler)
        port = xbdm_srv.server_address[1]
        t = threading.Thread(target=xbdm_srv.serve_forever, daemon=True)
        t.start()
        try:
            lines = xbox_debug.probe_xbdm("127.0.0.1", port=port, timeout=2.0, command="modules")
            self.assertEqual(lines[0], "201- connected")
            self.assertIn("line1", lines)
        finally:
            xbdm_srv.shutdown()
            xbdm_srv.server_close()


if __name__ == "__main__":
    unittest.main()
