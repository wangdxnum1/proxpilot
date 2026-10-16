import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from cli_transactions import Fixture, EXECUTABLE, remaining, console_break
import threading

class Diagnose(unittest.TestCase):
    def test_cancelled_collection_exits_130_and_creates_no_file(self):
        started,release=threading.Event(),threading.Event()
        def pending(): started.set();release.wait(10)
        with Fixture([200]) as f:
            f.proxies_gate=pending
            output=Path(f.temporary.name)/"cancelled-report.json"
            process=f.background("diagnose","--json","--output",str(output))
            try:
                self.assertTrue(started.wait(8))
                console_break(process.pid)
                release.set()
                stdout,stderr=process.communicate(timeout=10)
                self.assertEqual(process.returncode,130,stdout+stderr)
                d=json.loads(stdout)
                self.assertTrue(d["cancelled"])
                self.assertEqual(d["error"]["code"],"cancelled")
                self.assertFalse(output.exists())
            finally:
                release.set()
                if process.poll() is None: process.kill()
                process.communicate(timeout=5)
    def test_anonymous_readonly_report_and_file_protection(self):
        with Fixture([200]) as f:
            secrets=["SECRET-alpha-123", "password-6789", "e99bca34-18de-4800-9696-5399bbd9bc19", "Cookie-x=y", "C:\\Users\\PrivatePerson", "https://subscription.invalid/token"]
            f.nodes=["original", " ".join(secrets)]
            before=list(Path(f.temporary.name).rglob("*"))
            r=f.run("diagnose","--json","--secret",secrets[0])
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            d=json.loads(r.stdout)["data"]
            self.assertEqual(d["dependencies"]["scope"],"build_time")
            self.assertTrue(d["dependencies"]["verified"])
            self.assertTrue(d["source_integrity"]["available"])
            for secret in secrets: self.assertNotIn(secret,r.stdout)
            self.assertEqual(list(Path(f.temporary.name).rglob("*")),before)
            self.assertTrue(all(method=="GET" and "delay" not in path for method,path in f.requests))
            output=Path(f.temporary.name)/"diagnostic.json"
            r=f.run("diagnose","--json","--output",str(output))
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            original=output.read_bytes()
            r=f.run("diagnose","--json","--output",str(output))
            self.assertEqual(r.returncode,1)
            self.assertEqual(output.read_bytes(),original)
            self.assertEqual(f.run("diagnose","--json","--output",str(output),"--overwrite").returncode,0)

    def test_single_exe_offline_report_and_no_state_creation(self):
        with Fixture([200]) as f, tempfile.TemporaryDirectory() as cwd:
            r=subprocess.run([str(EXECUTABLE),"diagnose","--json","--api","http://127.0.0.1:1"],cwd=cwd,env=f.environment(),capture_output=True,text=True,encoding="utf-8",timeout=15)
            self.assertEqual(r.returncode,0,r.stdout+r.stderr)
            d=json.loads(r.stdout)["data"]
            self.assertTrue(d["partial"])
            self.assertFalse(d["source_integrity"]["available"])
            self.assertTrue(d["dependencies"]["verified"])
            self.assertEqual(list(Path(f.temporary.name).rglob("*")),[])

if __name__=="__main__": unittest.main(argv=[__file__,*remaining])
