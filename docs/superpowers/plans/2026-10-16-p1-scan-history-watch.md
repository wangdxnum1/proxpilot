# P1 Scan, History and Watch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans inline. User delegated recommended choices; continue without another approval gate.

**Goal:** Make probing controllable and selection stable, retaining current verification, billing limits and P0 ownership.

**Architecture:** A bounded scheduler produces measurements; an independent history store supplies evidence and a pure score. Watch uses a clock-driven policy and the existing switching transaction for temporary and committed changes.

**Tech Stack:** Rust 1.99.0 / floor1.98.0, Windows x64, existing HTTP/TLS, SQLite, SHA256 fingerprints.

**Spec:** `docs/superpowers/specs/2026-10-15-p1-scan-history-watch-design.md`

## Global Constraints

- Preserve P0 transaction, cancellation, typed errors and per-user lock. No long-lived watch lock. Billing filter precedes requests.
- Fixed concurrency1–256; probe/verify timeout100–120000ms (defaults4000/12000); retries0–3 (default1).
- Auto limit min(CPU*2,32,n), start min(8,limit); half-window transient failures halve; two clean complete windows add2. Partial final window cannot adapt.
- Only transient initial-probe transport errors retry, at cancellable200/400/800ms. Website samples never retry away a failure.
- History30days/10000 measurements; no raw URL/userinfo/query/secret/Cookie; corruption degrades without replacing DB.
- Current verification grade outranks scores; scan remains ordered by this round's delay.
- Watch defaults: minimum switch120s, failed-node600s, manual hold900s; default only confirmed-failure repair, explicit --optimize for proactive work.

## Review Focus

- Partial windows and retries cannot inflate unique completions (Task1).
- Controller loss cannot masquerade as all-node failure (Task1).
- Names may embed credentials: persist fingerprints/safe aliases (Task2).
- Cold/ip_only/nonfinite inputs cannot fabricate latency or NaN scores (Task3).
- External change or insufficient benefit must retain/restore the correct choice (Task4).

### Task 1: Bounded scheduler and configurable timing

**Files:** Add `src/probe_scheduler.rs`; modify checker/http/main and CLI fixtures.

**Interfaces:** `ProbeOptions { concurrency:Option<usize>, timeout_ms:u32, retries:u8 }`; scheduler returns unique measurements, retries, peak/window concurrency, cancellation and optional controller error. `checker::scan_report(...,&ProbeOptions)` consumes it. `verify_access_report_with_timeout(...,timeout_ms)` shares one browser sample budget across profiles. Preserve default wrappers for tests.

- [ ] Add parser/boundary and scheduler tests: fixed cap, automatic8→4→2, two clean windows+2, final partial unchanged, unique count, cancellation, auth/identity/bad-input no retry. Add fake-core peak/query-timeout/retry and website timeout cases.
- [ ] Run `cargo test --locked --quiet`; expected RED before interfaces exist.
- [ ] Implement and wire scan/fix/check/use/watch controls; expose unique completions/actual concurrency/retries. Report global API errors distinctly.
- [ ] Run full Rust and actual-exe offline suites; expected green. Commit `feat: bound probing and expose timing controls`.

### Task 2: Isolated history storage and commands

**Files:** Add history.rs; modify Cargo.toml/lock, main, checker and workflow.

**Interfaces:** `HistoryStore::open(path)`, `record(scope,node,measurement,now)`, `recent(scope,node,now)`, `list(now)`, `clear()`. Scope hashes canonical client/source/profile/group and target separately. Confirmed config identity permits pipe rotation; anonymous endpoint stays isolated. Store stage/availability/error/timing and fingerprint IDs. History failures warn without failing network work.

- [ ] Temporary-DB tests: client/source/profile/group/node/target isolation, credential absence, pipe rotation, schema version,30days/10000 pruning, locked/corrupt/readonly degradation. Run targeted tests; expected RED.
- [ ] Implement versioned SQLite and bounded transactions without replacing damaged/unknown schemas. Record probe and webpage evidence separately.
- [ ] Add `history` / `history clear` before backend setup; offline records remain accessible. Clear owns modification lock and touches only own DB.
- [ ] Run full suite, seeded-secret DB byte inspection and CLI fixtures; expected green. Commit `feat: retain isolated measurement history`.

### Task 3: Transparent scores and candidate ranking

**Files:** Add scoring.rs; modify workflow/switching/main.

**Interfaces:** `ScoreBreakdown { total,success,latency,stability,cost,samples }`; `score(history,current,rate)` returns finite components. Candidate comparator orders current grade, score descending, current latency, name. Selection reports carry evidence for P2.

- [ ] Test weights50/25/15/10, Laplace(success+1)/(total+2), <3 neutral0.5, median/MAD, unknown-rate0, ip_only neutral latency0.5 and nonfinite inputs. Expected RED.
- [ ] Implement latency1/(1+ms/500), stability1/(1+MAD/max(median,1)), cost1/(1+rate). Probe reachability never counts as webpage success.
- [ ] Rank only currently verified candidates; test excellent-history/current-failure rejection and deterministic ties. Full suite green; commit `feat: rank verified candidates with history evidence`.

### Task 4: Watch protection, cooldown and benefit gates

**Files:** Add watch_policy.rs; modify main/workflow/switching, CLI fixtures, README/CHANGELOG.

**Interfaces:** `WatchPolicy` consumes monotonic elapsed times and scope/selection, producing wait/repair/optimize decisions and cooldown eligibility. Flags `--min-switch-interval`, `--failure-cooldown`, `--manual-hold` default120/600/900. Optimization result may commit or report no benefit after explicit rollback; record failed candidates/next eligible time.

- [ ] Injected-time tests: default no optimization, opt-in startup/periodic, two failures5s apart, manual900s hold, min120s/failure override, scope reset, cooldown exhaustion, API neutrality/cancel. Expected RED.
- [ ] Test same-grade benefit requires score+5 AND delay-20% AND-100ms; higher grade may win; ip_only cannot fabricate a latency gain.
- [ ] Implement policy and identical current/candidate sampling inside P0 transaction. Insufficient benefit explicitly restores original; GUI conflict preserves external selection. Only actual commit updates last-switch time.
- [ ] Run Rust/actual-exe/clean CI; update help/docs/roadmap with evidence, expected green. Commit `feat: stabilize watch with manual-choice protection`.
