# P2 Output, Diagnose and Browse Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans inline. User delegated recommended choices; keep one final fresh-context whole-branch review.

**Goal:** Consistent machine output, share-safe diagnosis and a confirmed terminal workflow from existing business services.

**Architecture:** Finite commands return shared reports to text/JSON presenters; watch emits typed events. Diagnose uses a whitelist and report-local aliases. Browse resolves immutable identities and invokes verified selection.

**Tech Stack:** Existing Rust/serde/SQLite/Windows stack; lightweight terminal input, no GUI/server.

**Spec:** `docs/superpowers/specs/2026-10-15-p2-output-and-interaction-design.md`

## Global Constraints

- Preserve Windows single exe, P0 ownership/cancel/transaction and P1 controls/history/grade-first scoring/billing limits.
- Finite --json stdout: exactly one UTF-8 document including argument errors, with schema_version1,command,success,data,warnings,error,cancelled. Unknown numbers are null; node results complete despite text top30.
- watch --json: NDJSON with run_id, increasing sequence,time,type,payload and final stop/cancel event; no ANSI/banner/spinner/plain stdout. Broken pipe terminates without retry/panic loops.
- diagnose readonly/partial; never output raw config/rows/responses/secret/password/UUID/Cookie/Authorization or arbitrary names/errors. Endpoint/group/node/profile use report-local aliases; redact user paths.
- --output refuses overwrite unless --overwrite. Single exe reports build-time dependency evidence; source-file verification is separate. No uploads or automatic API enablement.
- browse requires stdin/stdout terminals; rejects --json. Confirmation defaults cancel; fresh identity/member check before switch; --dry-run never writes.

## Review Focus

- Parser/setup errors and nested helpers cannot contaminate JSON stdout (Task1).
- Early pipe closure must finish cleanly (Task1).
- Names/errors can hide secrets outside usual sensitive keys (Task2).
- Runtime users lack vendor files; do not mislabel build-time evidence (Task2).
- Refresh cannot reinterpret a stale ordinal as another target (Task3).

### Task 1: Shared reports and machine presenters

**Files:** Add report.rs,commands.rs,tests/cli_json.py; modify main/ui/helpers/watch.

**Interfaces:** `CommandReport<T>` owns envelope/exit semantics; command data feeds text and JSON. `OutputContext` routes progress to stderr or disables it throughout helpers. `WatchEvent` adds run/sequence/time. Emitter handles BrokenPipe as output termination.

- [x] Real-exe tests for every finite command success, invalid input, core-offline, validation failure and cancellation: parse entire stdout as one object, no ANSI, nullable unknowns. Watch sequence/final-cancel and closed-pipe tests. Run; expected RED.
- [x] Extract reports without duplicating selection or billing logic; raw argv selects error format before parse. Exclude secret-bearing raw parser text from unsafe messages.
- [x] Structured scan contains full ranks/measurements/current, rates/protocol/tag sources. Text retains widths/colors/current and complete1x list. ip_only stays distinct.
- [x] Full Rust, JSON and transaction CLI suites green; commit `feat: render shared reports as JSON and watch events`.

### Task 2: Readonly redacted diagnostic artifact

**Files:** Add diagnose.rs,root build.rs/provenance; modify help/packaging and JSON fixtures.

**Interfaces:** `DiagnosticReport` whitelist types and report-scoped alias table. Embedded version/revision/native-manifest fingerprint/verified-at-build status; optional source integrity separately. `write_report(path,overwrite)` uses create_new by default.

- [x] Seed known secrets/password/UUID/Cookie/path/URL and malicious names/errors; assert zero secret bytes and only readonly requests. Offline core yields partial report. Vendor-absent single-exe test and existing-output/overwrite/error tests. Run; expected RED.
- [x] Implement aliases/redaction/partial-module errors, own recovery/history summaries and optional real source integrity. Drop unrepresentable fields with typed warnings, no raw fallback.
- [x] Full Rust/CLI/secret byte-scan/readonly suites green; update docs; commit `feat: generate share-safe diagnostic reports`.

### Task 3: Filterable confirmed terminal browse

**Files:** Add browse.rs; modify main/help and terminal tests.

**Interfaces:** `BrowseModel` owns immutable selected identity, scope revision, filters/page/details. Actions refresh/scan/filter/page/details/select/confirm/back/quit. Confirmation shows client/group/original/target/rate/verification. Session guard restores terminal; apply calls workflow::use_flow.

- [x] Test keyword/protocol/rate/line filters, pagination/full Unicode, refresh invalidation/stale ordinal/scope/member change. Inject terminal input for default cancel/yes/max-rate/dry-run/cancel cleanup and nonterminal/JSON rejection. Run; expected RED.
- [x] Implement lightweight menu/input and terminal guard; reread before confirmation and revalidate through P0 at apply. Require unique group or explicit choice of legal group.
- [x] All suites and actual terminal smoke green; update docs/roadmap; commit `feat: add confirmed terminal browsing`.

### Task 4: Whole-goal verification and delivery

**Files:** Roadmap/help/README/CHANGELOG/evidence records.

- [x] Audit P0/P1/P2 acceptance against real Rust/script/CLI/terminal output and clean CI; release build; check original checkout for unrelated changes.
- [x] Create review packages; dispatch one fresh reviewer on most-capable model with all three plans/specs/rulings. Grade by effect. Fix Important/Critical with RED→GREEN regressions/full suite; ledger deferred minors.
- [x] Integrate reviewed branch into original development checkout, preserve user files, rebuild original bin and verify hashes; commit/push authorized original branch and inspect final CI.
- [x] Complete goal only after full implementation, original-bin delivery and push. Link exhaustive rulings/deferred findings in an auditable document; state material unverified external behavior.
