"""Offline acceptance against the actual exe; Python is a developer/CI tool only."""
import argparse
from contextlib import closing
import http.server
import hashlib
import json
import os
import sqlite3
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
        self.nodes = ["original", "target"]
        self.after_health_state_reads = 0
        self.change_before_operation = False
        self.changed_before_operation = threading.Event()
        self.url = "http://fixture.invalid/"
        self.samples = 1
        self.fail_restore = False
        self.fail_prepare_once = False
        self.prepare_outage = threading.Event()
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
                elif path == "/proxies":
                    if fixture.proxy_requests == 1:
                        fixture.after_health_state_reads += 1
                        if fixture.fail_prepare_once and fixture.after_health_state_reads == 2:
                            fixture.fail_prepare_once = False
                            fixture.prepare_outage.set()
                            self.reply({}, 503)
                            return
                        if fixture.change_before_operation and fixture.after_health_state_reads == 2:
                            fixture.selected = selected = "target"
                            fixture.changed_before_operation.set()
                    proxies = {node: {"type":"Shadowsocks"} for node in fixture.nodes}
                    proxies["group"] = {"type":"Selector", "all":fixture.nodes, "now":selected}
                    self.reply({"proxies":proxies})
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
                    failed = fixture.fail_restore and body["name"] == "original" and fixture.selected == "target"
                    if not failed: fixture.selected = body["name"]
                self.reply({}, 503 if failed else 204)

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
                "--url",self.url,"--samples",str(self.samples),"--no-system-proxy"]
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
    def test_all_probe_api_errors_are_environment_errors_even_when_version_endpoint_is_healthy(self):
        with Fixture([200]) as fixture:
            fixture.probe_status = 503
            result = fixture.run("scan", "--concurrency", "1")
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertEqual(fixture.proxy_requests, 0)
            path = Path(fixture.temporary.name) / "ProxPilot/history.sqlite3"
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute("SELECT count(*) FROM measurements").fetchone()[0], 0)

    def test_watch_controller_outage_does_not_cool_nodes_or_consume_reopt_schedule(self):
        with Fixture([200]) as fixture:
            fixture.fail_prepare_once = True
            def response():
                if fixture.selected == "original": time.sleep(0.08)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "1", "--no-system-proxy")
            try:
                self.assertTrue(fixture.prepare_outage.wait(5))
                self.wait_watch_cycle(fixture)
                self.assertEqual(len(fixture.delay_queries), 1)
                self.assertEqual(fixture.selected, "original")
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_watch_sufficient_gain_commits_with_equal_samples_and_minimum_interval(self):
        with Fixture([200]) as fixture:
            fixture.samples = 2
            def response():
                time.sleep(0.6 if fixture.selected == "original" else 0.01)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "1", "--reopt", "1", "--no-system-proxy")
            try:
                self.wait_watch_cycle(fixture)
                self.assertEqual(fixture.selected, "target")
                self.assertEqual(fixture.proxy_requests, 7)
                probes = len(fixture.delay_queries)
                time.sleep(2.4)
                self.assertEqual(len(fixture.delay_queries), probes)
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_watch_final_validation_must_still_meet_benefit_threshold(self):
        with Fixture([200]) as fixture:
            target_calls = 0
            def response():
                nonlocal target_calls
                if fixture.selected == "original": time.sleep(0.6)
                else:
                    target_calls += 1
                    time.sleep(0.01 if target_calls == 1 else 0.55)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "300", "--no-system-proxy")
            try:
                self.wait_watch_cycle(fixture)
                self.assertEqual(fixture.proxy_requests, 4)
                self.assertEqual(fixture.selected, "original")
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_watch_keep_rollback_failure_retains_journal_and_never_claims_restore(self):
        with Fixture([200]) as fixture:
            fixture.fail_restore = True
            def response():
                if fixture.selected == "original": time.sleep(0.08)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "300", "--no-system-proxy")
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if ("PUT","original") in fixture.requests: break
                    time.sleep(0.01)
                self.assertIn(("PUT","original"), fixture.requests)
                time.sleep(0.1)
                self.assertTrue((Path(fixture.temporary.name) / "ProxPilot/recovery.bin").exists())
                self.assertEqual(fixture.selected, "target")
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
                self.assertIn("503", stdout)
                self.assertNotIn("已恢复原选择", stdout)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_watch_invalid_url_is_configuration_error_without_node_penalty(self):
        with Fixture([200]) as fixture:
            fixture.url = "not-a-url"
            process = fixture.background("watch", "--interval", "300", "--no-system-proxy")
            try:
                try: stdout,stderr = process.communicate(timeout=5)
                except subprocess.TimeoutExpired: self.fail("invalid URL must stop as configuration error, not enter failure rechecks")
                self.assertEqual(process.returncode, 1, stdout + stderr)
                self.assertEqual(fixture.proxy_requests, 0)
                self.assertFalse(any(method == "PUT" for method,_ in fixture.requests))
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_manual_change_between_health_and_transaction_is_not_overwritten(self):
        with Fixture([200]) as fixture:
            fixture.nodes.append("third")
            fixture.change_before_operation = True
            def response():
                if fixture.selected == "target": time.sleep(0.6)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "300", "--no-system-proxy")
            try:
                self.assertTrue(fixture.changed_before_operation.wait(5))
                time.sleep(1.5)
                self.assertEqual(fixture.selected, "target")
                self.assertFalse(any(method == "PUT" for method,_ in fixture.requests))
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def wait_watch_cycle(self, fixture):
        deadline = time.monotonic() + 8
        journal = Path(fixture.temporary.name) / "ProxPilot/recovery.bin"
        while time.monotonic() < deadline:
            if fixture.proxy_requests >= 3 and not journal.exists(): return
            time.sleep(0.01)
        self.fail("watch operation did not finish")

    def test_watch_insufficient_benefit_restores_original(self):
        with Fixture([200]) as fixture:
            def response():
                if fixture.selected == "original": time.sleep(0.08)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "300", "--no-system-proxy")
            try:
                self.wait_watch_cycle(fixture)
                self.assertEqual(fixture.selected, "original")
                self.assertEqual([value for method,value in fixture.requests if method == "PUT"], ["target","original"])
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_watch_respects_manual_selection_after_optimization(self):
        with Fixture([200]) as fixture:
            def response():
                if fixture.selected == "original": time.sleep(0.08)
                return 200
            fixture.responses = [response]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "1", "--reopt", "1", "--no-system-proxy")
            try:
                self.wait_watch_cycle(fixture)
                manual = "target" if fixture.selected == "original" else "original"
                fixture.selected = manual
                before = len([r for r in fixture.requests if r[0] == "PUT"])
                time.sleep(2.4)
                self.assertEqual(fixture.selected, manual)
                self.assertEqual(len([r for r in fixture.requests if r[0] == "PUT"]), before)
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def test_watch_failed_alternative_is_not_repeated_during_cooldown(self):
        with Fixture([200]) as fixture:
            fixture.responses = [lambda: 403 if fixture.selected == "target" else 200]
            process = fixture.background("watch", "--optimize", "--top", "1", "--interval", "1", "--reopt", "1", "--no-system-proxy")
            try:
                self.wait_watch_cycle(fixture)
                probes = len(fixture.delay_queries)
                time.sleep(2.4)
                self.assertEqual(len(fixture.delay_queries), probes)
                self.assertEqual(fixture.selected, "original")
                console_break(process.pid)
                stdout,stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 130, stdout + stderr)
            finally:
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)

    def seed_verification_history(self, fixture, node, passed):
        path = Path(fixture.temporary.name) / "ProxPilot/history.sqlite3"
        with closing(sqlite3.connect(path)) as db:
            scope,target = db.execute("SELECT scope,target FROM measurements LIMIT 1").fetchone()
            evidence = {"stage":"verification","availability":"webpage" if passed else "failed", "delay_ms":200.0 if passed else None,
                        "error_code":None if passed else "website_http", "http_status":200 if passed else 403, "samples_requested":2, "samples_passed":2 if passed else 0}
            for _ in range(3):
                db.execute("INSERT INTO measurements(scope,target,node,timestamp,evidence) VALUES(?,?,?,?,?)", (scope,target,hashlib.sha256(node.encode()).hexdigest(),int(time.time()),json.dumps(evidence)))
            db.commit()

    def test_fix_uses_history_score_within_current_grade(self):
        with Fixture([200]) as fixture:
            def measured():
                if fixture.selected == "original": time.sleep(0.08)
                return 200
            fixture.responses = [measured]
            self.assertEqual(fixture.run("scan").returncode, 0)
            self.seed_verification_history(fixture, "target", False)
            self.seed_verification_history(fixture, "original", True)
            result = fixture.run("fix", "--top", "2", "--no-system-proxy")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(fixture.selected, "original", result.stdout)
            self.assertIn("50/25/15/10", result.stdout)

    def test_good_history_does_not_select_currently_failed_node(self):
        with Fixture([403,200,200]) as fixture:
            self.assertEqual(fixture.run("scan").returncode, 0)
            self.seed_verification_history(fixture, "target", True)
            result = fixture.run("fix", "--top", "2", "--no-system-proxy")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(fixture.selected, "original", result.stdout)

    def test_external_switch_during_verification_is_not_attributed_to_candidate_history(self):
        with Fixture([200]) as fixture:
            def externally_changed():
                fixture.selected = "external"
                return 200
            fixture.responses = [externally_changed]
            result = fixture.run("use", "target")
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertEqual(fixture.selected, "external")
            self.assertEqual([value for method,value in fixture.requests if method == "PUT"], ["target"])
            path = Path(fixture.temporary.name) / "ProxPilot/history.sqlite3"
            with closing(sqlite3.connect(path)) as db:
                self.assertEqual(db.execute("SELECT count(*) FROM measurements").fetchone()[0], 0)

    def test_history_records_probe_and_verification_offline_and_clears_only_own_rows(self):
        with Fixture([200]) as fixture:
            for command in (("scan",), ("use", "target")):
                result = fixture.run(*command)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            path = Path(fixture.temporary.name) / "ProxPilot/history.sqlite3"
            self.assertTrue(path.exists())
            with closing(sqlite3.connect(path)) as db:
                evidence = [json.loads(row[0]) for row in db.execute("SELECT evidence FROM measurements")]
            self.assertEqual(sum(row["stage"] == "probe" for row in evidence), 2)
            self.assertEqual(sum(row["stage"] == "verification" for row in evidence), 1)
            count = len(fixture.requests)
            listed = fixture.run("history")
            self.assertEqual(listed.returncode, 0, listed.stdout + listed.stderr)
            self.assertEqual(len(fixture.requests), count)
            self.assertNotIn("target", listed.stdout)
            self.assertIn("50/25/15/10", listed.stdout)
            preview = fixture.run("history", "clear", "--dry-run")
            self.assertEqual(preview.returncode, 0, preview.stdout + preview.stderr)
            with closing(sqlite3.connect(path)) as db: self.assertEqual(db.execute("SELECT count(*) FROM measurements").fetchone()[0], 3)
            cleared = fixture.run("history", "clear")
            self.assertEqual(cleared.returncode, 0, cleared.stdout + cleared.stderr)
            self.assertEqual(len(fixture.requests), count)
            self.assertEqual(fixture.selected, "target")
            with closing(sqlite3.connect(path)) as db: self.assertEqual(db.execute("SELECT count(*) FROM measurements").fetchone()[0], 0)

    def test_corrupt_history_never_breaks_scan_or_replaces_evidence(self):
        with Fixture([200]) as fixture:
            path = Path(fixture.temporary.name) / "ProxPilot/history.sqlite3"
            path.parent.mkdir()
            path.write_bytes(b"corrupt-history-evidence")
            scanned = fixture.run("scan")
            self.assertEqual(scanned.returncode, 0, scanned.stdout + scanned.stderr)
            self.assertEqual(path.read_bytes(), b"corrupt-history-evidence")
            listed = fixture.run("history")
            self.assertEqual(listed.returncode, 1, listed.stdout + listed.stderr)
            self.assertEqual(path.read_bytes(), b"corrupt-history-evidence")

    def test_locked_history_degrades_without_breaking_scan(self):
        with Fixture([200]) as fixture:
            self.assertEqual(fixture.run("scan").returncode, 0)
            path = Path(fixture.temporary.name) / "ProxPilot/history.sqlite3"
            with closing(sqlite3.connect(path)) as db:
                db.execute("BEGIN EXCLUSIVE")
                scanned = fixture.run("scan")
                self.assertEqual(scanned.returncode, 0, scanned.stdout + scanned.stderr)
                self.assertEqual(db.execute("SELECT count(*) FROM measurements").fetchone()[0], 2)
                db.rollback()

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
