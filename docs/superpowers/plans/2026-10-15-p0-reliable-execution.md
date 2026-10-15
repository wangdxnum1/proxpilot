# ProxPilot P0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. The user delegates recommendations and all routine confirmations; execute inline and continue through P1 and P2, then commit/push the verified integrated result. Steps use checkbox syntax for tracking.

**Goal:** Deliver reliable switching, actionable failures and a verified Windows build/release process, including the user's delay-column correction.

**Architecture:** Preserve the existing adapters and TLS implementation. Add typed diagnostic records, cancellation and an explicitly finished switching transaction, then route fix/use/watch through it. Keep legacy presentation wrappers only where an adapter still needs strings; do not discard typed evidence at the reporting boundary.

**Tech Stack:** Rust 1.99.0, windows-sys, serde/serde_json, rusqlite, wreq/BoringSSL, reqwest/rustls, PowerShell and GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-15-p0-reliable-execution-design.md`; shared constraints and the complete goal are in `docs/superpowers/specs/2026-10-15-reliability-roadmap.md`.

## Global Constraints

- Windows x64/MSVC, static CRT, BoringSSL browser emulation and existing client identity checks remain.
- auto never falls back across clients while system proxy is active; watch remains pinned to its initial client.
- Real verification grades are webpage pass, IP-only and failure. Missing HTTP status is null, never synthetic HTTP 0.
- `--max-rate` applies before probes; unknown/conflicting rates stay unknown; rollback restores the original direct selection even when it violates the new filter.
- Scan and dry-run do not change system proxy, node or third-party memory.
- Commit third-party selection memory only after final verification and confirmed selection, into the captured subscription.
- Success 0, environment/arguments 1, failed verification 2, cancellation 130. Errors retain machine-readable codes.
- No real account credentials, subscription URLs, cookies or raw third-party response bodies enter diagnostics.
- Continue the full P1/P2 objective after P0; do not claim total completion at this plan's end.

## Review Focus

- Ambiguous PUT result: the core may switch even when the response is lost; read-back and journal pending target must handle both known selections.
- User/profile change during testing: abort without overwriting a new choice or persisting into a different subscription.
- Cancellation while rollback is required: normal cancellation must not cancel cleanup or hide rollback errors.
- Empty/locked/corrupted state: refuse unsafe switching, retain recoverable state and keep read-only commands usable.
- Build output and shell failures: a stale exe, custom target directory or filtered command output must not turn a failed build/push into success.

---

### Task 1: Correct delay-column presentation

**Files:** Modify `src/scan_output.rs`, README and CHANGELOG.

**Interfaces:** Keep `header(width, economical) -> String` and `row(&Row, width, economical) -> String`; use one fixed display width for the delay column. Header `延迟(ms)`; table cells contain right-aligned integer milliseconds without repeated units; compact mode includes units explicitly.

- [ ] Change the wide table header and cells together; keep label fit/wrapping, current marker and colors.
- [ ] Verify the actual CLI with a local fake core, including rank 1/10/30 and delays 49/299/1691; confirm field boundaries by display cells, not UTF-8 bytes. This is reversible layout work: reuse the existing behavioral checks instead of introducing tests that duplicate formatting.
- [ ] Run `cargo test --locked --quiet`; commit the presentation correction and record the result.

### Task 2: Preserve typed request and verification evidence

**Files:** Create `src/diagnostics.rs`; modify `src/http.rs`, `src/core_api.rs`, `src/mihomo.rs`, `src/checker.rs`, `src/main.rs`.

**Interfaces:** `AppError { code: ErrorCode, stage: Stage, http_status: Option<u16>, retryable: bool, message: String }`, `type AppResult<T> = Result<T, AppError>`. Expose `CoreApi::request_detailed(...) -> AppResult<CoreResponse>`, `BrowserProbe::test_detailed(url) -> AppResult<(u16,f64)>` and `mihomo::probe_delay_detailed(...) -> AppResult<i64>`. Existing string APIs may call these methods while migration is incremental. Verification failures retain AppError instead of u16; build a serializable verification report for P2.

- [ ] Add failing tests for timeout/response truncation/HTTP-auth classifications, sanitized messages, and deceptive OpenAI URL hosts. Use existing local transport fixtures; no real network.
- [ ] Run those tests and capture the failing output.
- [ ] Implement classifications from structured library evidence, with transport_unknown when insufficient. Preserve status codes, remove `(0,99.0)` conversion and blanket IP reputation claims; parse URL host boundaries.
- [ ] Run targeted tests and the full suite. Update all existing callers so error wording/exit behavior is consistent; commit.

### Task 3: Cancellation, modification lock and recovery journal

**Files:** Create `src/operation.rs`; extend necessary Windows features in Cargo.toml; add integration tests under `tests/`.

**Interfaces:** `Cancellation::new/global/cancel/is_cancelled/check/wait`, per-user `MutationLock::acquire() -> AppResult<MutationLock>`, encrypted `RecoveryJournal::load/store/remove` for a serializable snapshot/pending target. Cancellation handler only sets an atomic flag; cleanup ignores normal cancellation. Use a held exclusive Windows file handle, not lock-file presence, to prevent simultaneous mutations across sessions.

- [ ] Add failing tests for cancellable waits, two live lock holders, lock release on process death, DPAPI journal round-trip, malformed/truncated journal and atomic replacement.
- [ ] Run them and observe failures before implementation.
- [ ] Implement control handler and bounded waits, lock and encrypted/atomic journal. Store no API secret; use current runtime config on recovery. Do not treat an old lock filename as a live owner.
- [ ] Run targeted subprocess/temporary-directory tests and the full suite; commit.

### Task 4: Switching transaction and conflict-aware rollback

**Files:** Create transaction code within `src/operation.rs` or focused `src/switching.rs`; modify `src/mihomo.rs`, `src/appstate.rs`, `src/sysproxy.rs`.

**Interfaces:** A mockable `SwitchControl` exposes identity/profile snapshot, read direct selection and switch; `SwitchTransaction::begin`, `select`, `commit`, `rollback` return typed reports. Explicit finalization reports restored/skipped-conflict/failed separately. `appstate` exposes captured-profile sync and returns a result instead of printing from inside the operation.

- [ ] Write failing simulated-core tests for final PUT/verification failure, restoration failure, nested original group, response-lost PUT, external third selection and changed profile. Assert final core/memory state and request sequence.
- [ ] Run targeted tests and record failures.
- [ ] Implement pending-target logging before PUT, pre/post read-back, original-selection validation, captured-profile checks and post-verification memory commit. Add proxy snapshot/conditional restore using a test registry key, preserving bypass and external updates.
- [ ] Run targeted tests and full suite; commit. No claim of compare-and-swap atomicity against external GUIs.

### Task 5: Integrate fix/use/watch and cancellable probes

**Files:** Modify `src/checker.rs`, `src/main.rs`, `src/http.rs`, `src/operation.rs`; add subprocess integration tests.

**Interfaces:** `fix_flow` and explicit use share the transaction service and return committed outcome plus warnings. Main registers cancellation once, maps final report to exit code and only then exits. Browser requests and scheduler waits observe Cancellation; active blocking API calls keep bounded timeouts.

- [ ] Write failing integration tests for `use` validation failure (restores original), `fix` final verification failure (nonzero), Ctrl+C during testing (130 plus recovery), and dry-run/no eligible candidates (zero mutation requests).
- [ ] Observe RED, then integrate the service without changing client selection or billing semantics.
- [ ] Bring final confirmation inside the transaction; remove premature appstate writes, ignored rollback results and misleading success messages. watch acquires locks per mutation, not while idle.
- [ ] Run all local transport and subprocess tests and commit.

### Task 6: Reproducible build and failure-safe packaging/release

**Files:** Create `rust-toolchain.toml`, `.github/workflows/ci.yml`, PowerShell packaging/build scripts and script fault-injection tests; modify bat entry points, vendor documentation and README.

**Interfaces:** Build entry returns nonzero for every failed phase, locates this invocation's Cargo artifact for configured target directory/triple, and verifies copy hash. Packaging defaults to local artifacts; explicit publish creates a draft release only after clean-tree/version/tag/identity gates, tests, integrity and build; it does not stage or commit arbitrary files.

- [ ] Write script subprocess tests with fake cargo/git/gh for build, copy, push and upload failures; stale artifact and custom target directory must fail rather than silently deliver another exe.
- [ ] Implement locked builds, Rust 1.99.0 pin with 1.98.0 CI coverage, SHA256 metadata/licenses and ASCII/UTF-8 output. Preserve original native provenance and verify hashes.
- [ ] Add clean `windows-2025-vs2026` CI: report tool versions; verify native files; test; build release; run local smoke/JSON-independent CLI fixtures; upload checksummed artifact. Ensure native consumer needs no CMake/NASM/libclang invocation.
- [ ] Run local tests, script fault injections and fresh target build. Push to the development branch when needed to execute CI, inspect the actual run, fix until green; do not create a public release as part of testing.
- [ ] Commit verified build changes and save actual CI evidence in the roadmap/ledger.

### Task 7: P0 verification and P1 handoff

**Files:** Update roadmap, user documentation, changelog and execution ledger.

- [ ] Check every P0 spec acceptance item against actual tests/CI results, run full suite and release build, verify bin hash.
- [ ] Record remaining P1/P2 as incomplete and write their implementation plans from approved specs; user has delegated the recommended choices, so continue execution without another confirmation gate.
- [ ] Keep new scan layout and no-mutation modes covered as shared regressions. A fresh whole-branch reviewer is required after all stages, before final integration and push.
