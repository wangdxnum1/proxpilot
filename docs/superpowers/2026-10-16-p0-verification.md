# P0 verification evidence

P0 baseline: `ff78eb0493e801cac1ac31fe50baf11665d93f66`. P1/P2 remain in development; original-checkout bin delivery follows final whole-goal review.

[Clean Windows CI run](https://github.com/wangdxnum1/proxpilot/actions/runs/38769828918) passed both Rust1.98.0 and1.99.0 on `windows-2025-vs2026`. Actual compiler19.51.36260.0/linker14.51.36260.0, toolset14.51.36231, SDK10.0.26100.0 were logged and checked against bundled library requirements.

Both jobs verified native hashes, passed99 Rust tests (2 live-network tests intentionally ignored),8 packaging failure tests and11 actual release-exe offline tests. The producer-tool blockers remained installed during build/test; consumption requires no CMake/NASM/Clang/bindgen execution. The1.99 job also ran default local packaging and uploaded its artifact; no Release was published.

Downloaded artifact was checked using its external archive SHA256 and every internal SHA256SUMS entry:

- Archive SHA256: `511b9957c6acb939e21fda286378fcc565619956922088c6e23bef4d6f7fab34`
- Rust1.99 CI exe: `23bc95e8c29f750771bb5985c62cdaaeba106cb95ce53aa2bb5b940c6a9a19c2`
- Rust1.98 CI exe: `2e9cc8cc15485276db5b8433249cc29e9fe48ef5be0632a958de6e2873dab6c3`
- Metadata revision matches baseline; dirty=false. Included native manifest, build metadata, declared dependency licenses and bundled BoringSSL/MIT license files all match checksums.

Local fresh-target Rust1.99/MSVC14.52/SDK10.0.28000 build and default local package passed. Local exe hash `49d302d5456279b01cb41aed11eec479f78a12e8ee7756fc181c276d38c6a080` matched its Cargo artifact and packaged copy. Different compiler toolchains are not expected to produce identical exe bytes.

Coverage includes authenticated HTTP/named-pipe control, malformed/truncated/timeout responses, real proxy/TLS failures, exact OpenAI host boundaries, DPAPI tamper rejection, exclusive lock release after process death, captured-profile DB isolation, registry half-write/external-update preservation, verified commit and conditional rollback. Subprocesses verify final verification failure, Ctrl+C/Ctrl+Break130, crash/recover, corrupt-record discard, dry-run/no eligible candidates and two-process mutation exclusion with concurrent readonly scan.

Audit supplements test an actually locked SQLite writer, committed journal deletion failure plus memory failure, and readonly scan pending-recovery notice. These additions will run in final CI together with P1/P2; the baseline CI above is not claimed to cover later commits.

Limits: no live third-party website or running user client was changed by these tests. External GUI races cannot be made atomic by mihomo's PUT API; detected conflicts are preserved. Forced kill cannot guarantee immediate rollback; verified recovery is deferred to the next modifying run or explicit recover. Original checkout/bin and final development-branch integration are still pending the remaining stages.
