"""Machine output acceptance against the real executable and isolated fake cores."""
import json
import subprocess
import time
import unittest
from cli_transactions import Fixture, EXECUTABLE, remaining, console_break


class JsonOutput(unittest.TestCase):
    def test_text_connection_summary_does_not_echo_proxy_credentials(self):
        with Fixture([200]) as f:
            proxy=f"http://private-user:private-password@127.0.0.1:{f.proxy.server_port}"
            r=subprocess.run(f.command("scan")+["--proxy",proxy],env=f.environment(),capture_output=True,text=True,encoding="utf-8",timeout=10)
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            self.assertNotIn("private-user",r.stdout)
            self.assertNotIn("private-password",r.stdout)
    def test_cancel_overrides_prior_partial_controller_error(self):
        import threading
        started,release=threading.Event(),threading.Event()
        with Fixture([200]) as f:
            f.probe_status=503
            def pending():
                if len(f.delay_queries)==2: started.set();release.wait(10)
            f.delay_gate=pending
            p=f.background("scan","--json","--concurrency","1","--retries","0")
            try:
                self.assertTrue(started.wait(8))
                console_break(p.pid);release.set()
                out,err=p.communicate(timeout=10)
                d=self.document(subprocess.CompletedProcess([],p.returncode,out,err),"scan",130)
                self.assertEqual(d["error"]["code"],"cancelled")
            finally:
                release.set()
                if p.poll() is None: p.kill()
                p.communicate(timeout=5)
    def document(self, result, command, code=0):
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        document = json.loads(result.stdout)
        self.assertNotIn("\x1b", result.stdout)
        self.assertEqual(document["schema_version"], 1)
        self.assertEqual(document["command"], command)
        self.assertEqual(document["success"], code == 0)
        self.assertEqual(document["cancelled"], code == 130)
        return document

    def test_finite_commands_return_one_document(self):
        with Fixture([200]) as f:
            for arguments in [("scan",), ("nodes",), ("info", "target"), ("check",),
                              ("use", "target"), ("fix",), ("clients", "--supported"),
                              ("clients",), ("config", "show"), ("history",), ("recover",)]:
                with self.subTest(arguments=arguments):
                    self.document(f.run(*arguments, "--json", "--no-system-proxy"), arguments[0])

    def test_scan_full_rank_metadata_and_nullable_unknowns(self):
        with Fixture([200]) as f:
            f.nodes = ["original", "target"] + [f"node-{i}" for i in range(35)]
            d = self.document(f.run("scan", "--json"), "scan")["data"]
            self.assertEqual(len(d["nodes"]), 37)
            self.assertEqual(sorted(n["rank"] for n in d["nodes"]), list(range(1, 38)))
            for node in d["nodes"]:
                self.assertIsNone(node["rate"])
                self.assertEqual(node["protocol_source"], "kernel_api")
                self.assertEqual(node["labels_source"], "name_unverified")

    def test_parser_and_setup_errors_have_safe_envelope(self):
        with Fixture([200]) as f:
            r = f.run("scan", "--json", "--unknown-secret-DO-NOT-PRINT")
            d = self.document(r, "scan", 1)
            self.assertEqual(d["error"]["code"], "invalid_input")
            self.assertNotIn("DO-NOT-PRINT", r.stdout)
            r = subprocess.run([str(EXECUTABLE), "scan", "--json", "--api", "http://127.0.0.1:1"],
                               env=f.environment(), capture_output=True, text=True, encoding="utf-8", timeout=15)
            self.document(r, "scan", 1)

    def test_option_value_named_watch_is_not_the_command(self):
        with Fixture([200]) as f:
            self.document(f.run("--secret","watch","nodes","--json"),"nodes")

    def test_output_permission_failure_is_not_success(self):
        from pathlib import Path
        with Fixture([200]) as f:
            path=Path(f.temporary.name)/"readonly-output.txt"
            path.write_bytes(b"preserve")
            with path.open("rb") as output:
                r=subprocess.run(f.command("nodes","--json"),env=f.environment(),stdout=output,stderr=subprocess.PIPE,timeout=10)
            self.assertEqual(r.returncode,1,r.stderr)
            self.assertEqual(path.read_bytes(),b"preserve")

    def test_domain_errors_preserve_their_typed_category(self):
        from pathlib import Path
        with Fixture([200]) as f:
            for arguments in [("scan",),("use","target")]:
                r=subprocess.run(f.command(*arguments,"--json")+["--group","missing"],env=f.environment(),capture_output=True,text=True,encoding="utf-8",timeout=10)
                e=self.document(r,arguments[0],1)["error"]
                self.assertEqual((e["code"],e["stage"],e["retryable"]),("invalid_input","configuration",False))
        with Fixture([200]) as f:
            f.invalid_proxies=True
            e=self.document(f.run("nodes","--json"),"nodes",1)["error"]
            self.assertEqual((e["code"],e["stage"],e["retryable"]),("invalid_response","controller",False))
        with Fixture([200]) as f:
            path=Path(f.temporary.name)/"ProxPilot/history.sqlite3";path.parent.mkdir();path.write_bytes(b"damaged")
            e=self.document(f.run("history","--json"),"history",1)["error"]
            self.assertEqual((e["code"],e["stage"],e["retryable"]),("state_unavailable","state",False))
        with Fixture([200]) as f:
            e=self.document(f.run("nodes","--client","cutecloud","--json"),"nodes",1)["error"]
            self.assertEqual((e["code"],e["stage"],e["retryable"]),("identity_mismatch","discovery",False))

    def test_invalid_node_and_empty_billing_scope_are_input_errors(self):
        with Fixture([200]) as f:
            for arguments in [("info","missing"),("nodes","--max-rate","1"),("scan","--max-rate","1")]:
                with self.subTest(arguments=arguments):
                    e=self.document(f.run(*arguments,"--json"),arguments[0],1)["error"]
                    self.assertEqual((e["code"],e["stage"],e["retryable"]),("invalid_input","configuration",False))

    def test_validation_failure_retains_rollback(self):
        with Fixture([403]) as f:
            d = self.document(f.run("use", "target", "--json"), "use", 2)
            self.assertEqual(d["data"]["rollback"]["status"], "restored")
            self.assertEqual(f.selected, "original")

    def test_mutation_cancel_is_one_document_and_restores(self):
        with Fixture([200]) as f:
            f.responses = [lambda: (time.sleep(2) or 200)]
            p = f.background("use", "target", "--json")
            try:
                self.assertTrue(f.proxy_started.wait(5))
                console_break(p.pid)
                out, err = p.communicate(timeout=10)
                self.document(subprocess.CompletedProcess([],p.returncode,out,err),"use",130)
                self.assertEqual(f.selected,"original")
            finally:
                if p.poll() is None: p.kill(); p.wait()

    def test_watch_parser_error_is_ndjson(self):
        with Fixture([200]) as f:
            r=f.run("watch","--json","--interval","0")
            self.assertEqual(r.returncode,1,r.stdout+r.stderr)
            events=[json.loads(line) for line in r.stdout.splitlines()]
            self.assertEqual(events[-1]["type"],"stopped")
            self.assertEqual(events[0]["payload"]["error"]["code"],"invalid_input")

    def test_watch_ndjson_cancel_and_pipe_close(self):
        with Fixture([200]) as f:
            p = f.background("watch", "--json", "--no-system-proxy", "--interval", "60")
            try:
                self.assertTrue(f.proxy_started.wait(5))
                console_break(p.pid)
                out, err = p.communicate(timeout=10)
                self.assertEqual(p.returncode, 130, out + err)
                events = [json.loads(line) for line in out.splitlines()]
                self.assertEqual([e["sequence"] for e in events], list(range(1, len(events) + 1)))
                self.assertEqual(len({e["run_id"] for e in events}), 1)
                self.assertEqual(events[-1]["type"], "stopped")
                self.assertTrue(events[-1]["payload"]["cancelled"])
            finally:
                if p.poll() is None: p.kill(); p.wait()
                if p.stdout: p.stdout.close()
                if p.stderr: p.stderr.close()
            p = f.background("watch", "--json", "--no-system-proxy", "--interval", "1")
            p.stdout.close()
            try:
                self.assertEqual(p.wait(timeout=8), 0)
                self.assertNotIn("panicked", p.stderr.read())
            finally:
                if p.poll() is None: p.kill(); p.wait()


if __name__ == "__main__":
    unittest.main(argv=[__file__, *remaining])

