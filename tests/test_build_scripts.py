"""Failure injection for packaging. Never invokes real cargo/git/gh or a network."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
POWERSHELL = shutil.which("powershell.exe")

FAKE = r'''
import json, os, pathlib, sys
sys.stdout.reconfigure(encoding="utf-8")
tool, *args = sys.argv[1:]
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
with (root / "calls.jsonl").open("a") as log: log.write(json.dumps([tool, *args])+"\n")
case = os.environ.get("FAIL_CASE", "")
if tool == "cargo":
    if args[0] == "test": sys.exit(4 if case == "test" else 0)
    if args[0] == "build":
        if case == "build": sys.exit(5)
        if case != "stale": print(json.dumps({"reason":"compiler-artifact", "target":{"name":"proxpilot", "kind":["bin"]}, "executable":os.environ.get("ARTIFACT_PATH",str(root / "custom-target/triple/release/proxpilot.exe"))},ensure_ascii=False))
    if args[0] == "metadata": print(json.dumps({"packages":[{"name":"fixture", "version":"1", "license":"MIT"}]}))
elif tool == "git":
    if args[:2] == ["status", "--porcelain"]: print(" M user-file" if case == "dirty" else "")
    elif args[:2] == ["rev-parse", "HEAD"]: print("a"*40)
    elif args[:2] == ["rev-parse", "--verify"]: sys.exit(0 if case == "existing-tag" else 1)
    elif args[0] == "push": sys.exit(8 if case == "push" else 0)
    elif args[0] == "remote": print("https://github.com/example/fixture.git")
elif tool == "gh":
    if args[:2] == ["release", "create"]: sys.exit(9 if case == "upload" else 0)
elif tool == "rustc": print("rustc 1.99.0 fixture")
'''


@unittest.skipUnless(os.name == "nt" and POWERSHELL, "Windows PowerShell scripts")
class BuildScripts(unittest.TestCase):
    def test_utf8_cargo_artifact_path_from_legacy_codepage_is_copied_correctly(self):
        source = self.root / "中文 target/proxpilot.exe"
        source.parent.mkdir()
        source.write_bytes(b"unicode artifact")
        wrapper = self.root / "scripts/legacy-codepage.ps1"
        wrapper.write_text("[Console]::OutputEncoding=[Text.Encoding]::GetEncoding(936)\n& (Join-Path $PSScriptRoot 'build.ps1') -OutputDirectory (Join-Path $PSScriptRoot '../中文 bin')\nexit $LASTEXITCODE\n",encoding="utf-8-sig")
        result = subprocess.run([POWERSHELL,"-NoProfile","-ExecutionPolicy","Bypass","-File",str(wrapper)],cwd=self.root,env=dict(self.env,ARTIFACT_PATH=str(source)),capture_output=True,encoding="utf-8",errors="replace",timeout=45)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertEqual((self.root / "中文 bin/proxpilot.exe").read_bytes(),source.read_bytes())

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="proxpilot-scripts-")
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        for name in ("build.ps1", "release.ps1"):
            source = ROOT / "scripts" / name
            if source.exists(): shutil.copy2(source, self.root / "scripts" / name)
        (self.root / "scripts/check-toolchain.ps1").write_text("'fixture tools'\n")
        for name in ("Cargo.toml", "CHANGELOG.md"):
            shutil.copy2(ROOT / name, self.root / name)
        vendor = self.root / "vendor/btls-sys"
        vendor.mkdir(parents=True)
        (vendor / "verify.ps1").write_text("'verified'\n")
        (vendor / "LICENSE-MIT").write_text("fixture license")
        native = vendor / "prebuilt/x86_64-pc-windows-msvc"
        native.mkdir(parents=True)
        (native / "manifest.json").write_text('{}')
        (native / "LICENSE-BORINGSSL").write_text('fixture license')
        self.artifact = self.root / "custom-target/triple/release/proxpilot.exe"
        self.artifact.parent.mkdir(parents=True)
        self.artifact.write_bytes(b"new cargo artifact")
        old = self.root / "target/release/proxpilot.exe"
        old.parent.mkdir(parents=True)
        old.write_bytes(b"old stale artifact")
        tools = self.root / "tools"
        tools.mkdir()
        fake = tools / "fake.py"
        fake.write_text(FAKE)
        import sys
        for name in ("cargo", "git", "gh", "rustc"):
            (tools / f"{name}.cmd").write_text(f'@echo off\n"{sys.executable}" "{fake}" {name} %*\nexit /b %errorlevel%\n')
        self.env = dict(os.environ, FIXTURE_ROOT=str(self.root), PATH=str(tools) + os.pathsep + os.environ["PATH"])

    def tearDown(self): self.temp.cleanup()

    def run_script(self, name="build", case="", *args):
        env = dict(self.env, FAIL_CASE=case)
        return subprocess.run([POWERSHELL, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                               str(self.root / "scripts" / f"{name}.ps1"), *args],
                              cwd=self.root, env=env, capture_output=True, text=True, timeout=45)

    def calls(self):
        path = self.root / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def test_build_uses_reported_custom_target_and_locked_flag(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.root / "bin/proxpilot.exe").read_bytes(), self.artifact.read_bytes())
        self.assertIn("--locked", next(c for c in self.calls() if c[:2] == ["cargo", "build"]))

    def test_failed_build_and_missing_artifact_never_copy_stale_output(self):
        for case in ("build", "stale"):
            with self.subTest(case=case):
                result = self.run_script(case=case)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "bin/proxpilot.exe").exists())

    def test_copy_failure_is_nonzero(self):
        (self.root / "blocked").write_text("not a directory")
        self.assertNotEqual(self.run_script("build", "", "-OutputDirectory", str(self.root / "blocked")).returncode, 0)

    def test_local_package_has_hashes_metadata_licenses_and_no_publish(self):
        result = self.run_script("release")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(list((self.root / "dist").rglob("SHA256SUMS.txt")))
        self.assertTrue(list((self.root / "dist").rglob("build-metadata.json")))
        self.assertTrue(list((self.root / "dist").rglob("LICENSE-BORINGSSL")))
        self.assertFalse(any(c[0] == "gh" or c[:2] in (["git", "push"], ["git", "tag"]) for c in self.calls()))

    def test_test_or_build_failure_prevents_push_and_upload(self):
        for case in ("test", "build"):
            with self.subTest(case=case):
                result = self.run_script("release", case, "-PublishDraft")
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(c[:2] == ["git", "push"] or c[:3] == ["gh", "release", "create"] for c in self.calls()))

    def test_dirty_tree_and_existing_tag_prevent_release(self):
        for case in ("dirty", "existing-tag"):
            with self.subTest(case=case):
                result = self.run_script("release", case, "-PublishDraft")
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(c[:2] == ["git", "push"] for c in self.calls()))

    def test_push_failure_stops_upload(self):
        result = self.run_script("release", "push", "-PublishDraft")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(any(c[:2] == ["git", "push"] for c in self.calls()))
        self.assertFalse(any(c[:3] == ["gh", "release", "create"] for c in self.calls()))

    def test_upload_failure_returns_nonzero_and_only_draft_is_requested(self):
        result = self.run_script("release", "upload", "-PublishDraft")
        self.assertNotEqual(result.returncode, 0)
        release = next(c for c in self.calls() if c[:3] == ["gh", "release", "create"])
        self.assertIn("--draft", release)
        self.assertIn("--verify-tag", release)
        self.assertFalse(any(c[:2] in (["git", "add"], ["git", "commit"]) for c in self.calls()))


if __name__ == "__main__": unittest.main()
