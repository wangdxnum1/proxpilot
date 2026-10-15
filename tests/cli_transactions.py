"""Offline acceptance against the actual exe; Python is a developer/CI tool only."""
import argparse
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.parse


def console_break(pid, event=1):
    """Use a separate helper so attaching never changes the test runner's console."""
    helper = r'''
import ctypes, sys, time
from ctypes import wintypes
k = ctypes.WinDLL("kernel32", use_last_error=True)
k.FreeConsole()
if not k.AttachConsole(int(sys.argv[1])): raise ctypes.WinError(ctypes.get_last_error())
handler_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.DWORD)
@handler_type
def ignore(event): return True
k.SetConsoleCtrlHandler.argtypes = [handler_type, wintypes.BOOL]
if not k.SetConsoleCtrlHandler(ignore, True): raise ctypes.WinError(ctypes.get_last_error())
event = int(sys.argv[2])
if not k.GenerateConsoleCtrlEvent(event, int(sys.argv[1]) if event else 0): raise ctypes.WinError(ctypes.get_last_error())
time.sleep(0.1)
k.FreeConsole()
'''
    import sys
    subprocess.run([sys.executable, "-c", helper, str(pid), str(event)], check=True,
                   creationflags=subprocess.CREATE_NO_WINDOW, capture_output=True, timeout=5)

parser = argparse.ArgumentParser(add_help=False)
parser.add_argument("--bin", required=True, type=Path)
options, remaining = parser.parse_known_args()
EXECUTABLE = options.bin.resolve()


class Fixture:
    def __init__(self, responses):
        self.responses = responses
        self.selected = "original"
        self.requests = []
        self.proxy_requests = 0
        self.proxy_started = threading.Event()
        self.delay_queries = []
        self.delay_active = 0
        self.delay_peak = 0
        self.truncate_first_probe = False
        self.probe_status = 200
        self.health_failure_after_probe = False
        self.mutex = threading.Lock()
        self.temporary = tempfile.TemporaryDirectory(prefix="proxpilot-cli-")
        fixture = self

        class Core(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def reply(self, body, status=200):
                data = b"" if status == 204 else json.dumps(body, ensure_ascii=False).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
            def do_GET(self):
                path = urllib.parse.urlsplit(self.path).path
                with fixture.mutex:
                    fixture.requests.append(("GET", path))
                    selected = fixture.selected
                if path == "/version": self.reply({"version": "fixture"}, 503 if fixture.health_failure_after_probe and fixture.delay_queries else 200)
                elif path == "/configs": self.reply({"mode": "rule", "mixed-port": fixture.proxy.server_port})
                elif path == "/proxies": self.reply({"proxies": {
                    "group": {"type": "Selector", "all": ["original", "target"], "now": selected},
                    "original": {"type": "Shadowsocks"}, "target": {"type": "Shadowsocks"},
                }})
                elif path.endswith("/delay"):
                    with fixture.mutex:
                        fixture.delay_queries.append(self.path)
                        truncate = fixture.truncate_first_probe and len(fixture.delay_queries) == 1
                        fixture.delay_active += 1
                        fixture.delay_peak = max(fixture.delay_peak, fixture.delay_active)
                    time.sleep(0.03)
                    with fixture.mutex: fixture.delay_active -= 1
                    if truncate:
                        self.send_response(200)
                        self.send_header("Content-Length", "100")
                        self.send_header("Connection", "close")
                        self.end_headers()
                        self.wfile.write(b'{')
                    else: self.reply({"delay": 10 if "/target/" in path else 50}, fixture.probe_status)
                else: self.reply({}, 404)
            def do_PUT(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with fixture.mutex:
                    fixture.requests.append(("PUT", body["name"]))
                    fixture.selected = body["name"]
                self.reply({}, 204)

        class Proxy(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_GET(self):
                with fixture.mutex:
                    index = fixture.proxy_requests
                    fixture.proxy_requests += 1
                fixture.proxy_started.set()
                status = fixture.responses[min(index, len(fixture.responses) - 1)]
                if callable(status): status = status()
                try:
                    self.send_response(status)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                except (ConnectionError, OSError): pass

        self.core = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Core)
        self.proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
        for server in (self.core, self.proxy):
            threading.Thread(target=server.serve_forever, daemon=True).start()

    def __enter__(self): return self
    def __exit__(self, *_):
        for server in (self.core, self.proxy):
            server.shutdown()
            server.server_close()
        self.temporary.cleanup()
    def command(self, *arguments):
        return [str(EXECUTABLE), *arguments, "--api", f"http://127.0.0.1:{self.core.server_port}",
                "--proxy", f"http://127.0.0.1:{self.proxy.server_port}", "--group", "group",
                "--url", "http://fixture.invalid/", "--samples", "1"]
    def environment(self):
        env = dict(os.environ, APPDATA=self.temporary.name, NO_COLOR="1", PROXPILOT_SPINNER="0")
        env.pop("CLICOLOR_FORCE", None)
        return env
    def run(self, *arguments):
        return subprocess.run(self.command(*arguments), env=self.environment(), capture_output=True,
                              text=True, encoding="utf-8", timeout=40)
    def background(self, *arguments):
        startup = subprocess.STARTUPINFO()
        startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
        startup.wShowWindow = 0
        return subprocess.Popen(self.command(*arguments), env=self.environment(),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, encoding="utf-8", startupinfo=startup,
                                creationflags=subprocess.CREATE_NEW_CONSOLE | subprocess.CREATE_NEW_PROCESS_GROUP)


class Transactions(unittest.TestCase):
    def test_global_controller_failure_is_environment_error_and_auth_is_not_retried(self):
        for status in (401, 503):
            with self.subTest(status=status), Fixture([200]) as fixture:
                fixture.probe_status = status
                fixture.health_failure_after_probe = status == 503
                result = fixture.run("scan", "--concurrency", "1", "--retries", "3")
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertEqual(len(fixture.delay_queries), 1 if status == 401 else 2)
                self.assertEqual(fixture.selected, "original")
                self.assertFalse(any(method == "PUT" for method, _ in fixture.requests))

    def test_probe_controls_reach_core_and_retry_truncated_response(self):
        with Fixture([200]) as fixture:
            fixture.truncate_first_probe = True
            result = fixture.run("scan", "--concurrency", "1", "--probe-timeout-ms", "777", "--retries", "1")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(len(fixture.delay_queries), 3)
            self.assertEqual(fixture.delay_peak, 1)
            self.assertTrue(all(urllib.parse.parse_qs(urllib.parse.urlsplit(q).query)["timeout"] == ["777"] for q in fixture.delay_queries))

    def test_website_timeout_is_applied_without_retrying_failed_sample(self):
        def pending():
            time.sleep(1)
            return 200
        with Fixture([pending]) as fixture:
            started = time.monotonic()
            result = fixture.run("use", "target", "--verify-timeout-ms", "100", "--retries", "3")
            elapsed = time.monotonic() - started
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertLess(elapsed, 0.9)
            self.assertEqual(fixture.selected, "original")
            self.assertEqual(fixture.proxy_requests, 1)

    def test_second_mutation_is_busy_but_readonly_scan_can_run(self):
        release = threading.Event()
        def pending():
            release.wait(10)
            return 200
        with Fixture([pending]) as fixture:
            process = fixture.background("use", "target")
            try:
                self.assertTrue(fixture.proxy_started.wait(5))
                other = fixture.run("use", "original")
                self.assertEqual(other.returncode, 1, other.stdout + other.stderr)
                self.assertEqual([value for method, value in fixture.requests if method == "PUT"], ["target"])
                scanned = fixture.run("scan")
                self.assertEqual(scanned.returncode, 0, scanned.stdout + scanned.stderr)
                self.assertIn("recover", scanned.stdout)
                self.assertEqual(fixture.selected, "target")
                console_break(process.pid)
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
                self.assertEqual(fixture.selected, "original")
            finally:
                release.set()
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_crashed_use_can_be_inspected_and_explicitly_recovered(self):
        release = threading.Event()
        def pending():
            release.wait(10)
            return 200
        with Fixture([pending]) as fixture:
            process = fixture.background("use", "target")
            try:
                self.assertTrue(fixture.proxy_started.wait(5))
                process.kill()
                process.communicate(timeout=5)
                release.set()
                journal = Path(fixture.temporary.name) / "ProxPilot/recovery.bin"
                self.assertEqual(fixture.selected, "target")
                self.assertTrue(journal.exists())
                count = len(fixture.requests)
                inspected = fixture.run("recover")
                self.assertEqual(inspected.returncode, 0, inspected.stdout + inspected.stderr)
                self.assertEqual(len(fixture.requests), count)
                self.assertTrue(journal.exists())
                recovered = fixture.run("recover", "--retry")
                self.assertEqual(recovered.returncode, 0, recovered.stdout + recovered.stderr)
                self.assertEqual(fixture.selected, "original")
                self.assertFalse(journal.exists())
            finally:
                release.set()
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_corrupted_journal_is_preserved_until_explicit_discard(self):
        with Fixture([200]) as fixture:
            journal = Path(fixture.temporary.name) / "ProxPilot/recovery.bin"
            journal.parent.mkdir()
            journal.write_bytes(b"broken-evidence")
            result = fixture.run("use", "target")
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertEqual(journal.read_bytes(), b"broken-evidence")
            self.assertFalse(any(method == "PUT" for method, _ in fixture.requests))
            count = len(fixture.requests)
            discarded = fixture.run("recover", "--discard")
            self.assertEqual(discarded.returncode, 0, discarded.stdout + discarded.stderr)
            self.assertFalse(journal.exists())
            self.assertEqual(len(fixture.requests), count)
            self.assertEqual(fixture.selected, "original")

    @unittest.skipUnless(os.name == "nt", "Windows console control event")
    def test_ctrl_break_recovers_selection_and_exits_130(self):
        self.assert_cancel_recovers(1)

    @unittest.skipUnless(os.name == "nt", "Windows console control event")
    def test_ctrl_c_recovers_selection_and_exits_130(self):
        self.assert_cancel_recovers(0)

    def assert_cancel_recovers(self, event):
        release = threading.Event()
        def pending():
            release.wait(10)
            return 200
        with Fixture([pending]) as fixture:
            process = fixture.background("use", "target")
            try:
                self.assertTrue(fixture.proxy_started.wait(5), "verification request did not start")
                self.assertEqual(fixture.selected, "target")
                console_break(process.pid, event)
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
                self.assertEqual(fixture.selected, "original", stdout)
                self.assertFalse((Path(fixture.temporary.name) / "ProxPilot/recovery.bin").exists())
            finally:
                release.set()
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    @unittest.skipUnless(os.name == "nt", "Windows console control event")
    def test_watch_idle_wait_is_cancellable_without_switching(self):
        with Fixture([200]) as fixture:
            process = fixture.background("watch", "--interval", "300", "--no-system-proxy")
            try:
                self.assertTrue(fixture.proxy_started.wait(5))
                console_break(process.pid, 0)
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
                self.assertEqual(fixture.selected, "original")
                self.assertFalse(any(method == "PUT" for method, _ in fixture.requests))
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_use_success_commits_and_clears_journal(self):
        with Fixture([200]) as fixture:
            result = fixture.run("use", "target")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(fixture.selected, "target")
            self.assertFalse((Path(fixture.temporary.name) / "ProxPilot/recovery.bin").exists())

    def test_dry_run_does_not_switch_or_verify_websites(self):
        for command in [("use", "target"), ("fix",)]:
            with self.subTest(command=command), Fixture([200]) as fixture:
                result = fixture.run(*command, "--dry-run", "--no-system-proxy")
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(any(method == "PUT" for method, _ in fixture.requests))
                self.assertEqual(fixture.proxy_requests, 0)
                self.assertFalse((Path(fixture.temporary.name) / "ProxPilot/recovery.bin").exists())

    def test_no_eligible_rate_never_probes_or_switches(self):
        with Fixture([200]) as fixture:
            result = fixture.run("fix", "--max-rate", "1", "--no-system-proxy")
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertFalse(any(method == "PUT" or value.endswith("/delay") for method, value in fixture.requests))
            self.assertEqual(fixture.proxy_requests, 0)

    def test_fix_final_verification_failure_is_nonzero_and_restores(self):
        with Fixture([200, 403]) as fixture:
            result = fixture.run("fix", "--top", "1", "--no-system-proxy")
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertEqual(fixture.selected, "original", result.stdout)
            self.assertFalse((Path(fixture.temporary.name) / "ProxPilot/recovery.bin").exists())

    def test_use_failed_verification_restores_original(self):
        with Fixture([403]) as fixture:
            result = fixture.run("use", "target")
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertEqual(fixture.selected, "original", result.stdout)
            self.assertEqual([value for method, value in fixture.requests if method == "PUT"],
                             ["target", "original"])
            self.assertFalse((Path(fixture.temporary.name) / "ProxPilot/recovery.bin").exists())


if __name__ == "__main__":
    unittest.main(argv=[__file__, *remaining])
