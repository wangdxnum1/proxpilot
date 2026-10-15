# 0.4.0 P0/P1/P2 交付审计

范围：Windows x64 单文件 CLI；完整三个阶段与 scan 延迟列对齐。

## 当前验证

- 本机：130项 Rust 通过，2项真实外部环境测试忽略；30项事务、8项JSON、2项诊断、4项真实ConPTY和9项构建脚本验收。最终审查后重新运行。
- P1 双版本干净CI：[38789612038](https://github.com/wangdxnum1/proxpilot/actions/runs/38789612038)，Rust1.98/1.99均成功；P0首次打包产物已逐文件核验，见P0验证记录。
- P2最终CI、审查结论、原目录bin哈希与推送证据：待完成后补充。

## 代理代为作出的完整实施裁定

逐条保留各阶段执行账本中的裁定，包含原因和取舍；未增加真实客户端在线修改验收。

### 2026-10-15-p0-reliable-execution

- Ruling: use the native inline execution recommendation — tightly coupled control/error boundaries benefit from one implementer; retain final independent review.
- Ruling: approved design and implementation plan review gates are delegated by the user's updated goal; proceed without additional questions.
- Ruling: delay cells now contain numbers only under 延迟(ms), with shared column widths; this supersedes the earlier per-row ms presentation.
- Ruling: create the skill workspace with its shipped script; on Windows store commands/results directly if task wrapper Bash cannot invoke cargo reliably. Never substitute a plan tick for actual test output.
- Ruling: explicitly depend on the already locked btls 0.5.6 with no default features — wreq exposes a narrow is_tls flag but connector errors wrap btls::ssl::Error; structural downcast avoids string guessing and does not change native ABI/features.
- Ruling: keep temporary typed-to-string wrappers only in discovery compatibility APIs; CoreApi requests, browser verification reports and scan measurements retain structured errors for later P2. A report, not the display message, remains authoritative.
- Ruling: use an exclusive share-mode Windows file handle under per-user APPDATA instead of a named mutex — naturally coordinates sessions of the same account and releases on process death without trusting a stale file.
- Ruling: Windows DPAPI with UI forbidden and purpose entropy protects journal payloads; MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH) publishes only a fully synced ciphertext file. Only this call's newly created temp file may be cleaned up.
- Ruling: persist a committed phase to the encrypted journal BEFORE memory sync/removal — if deletion fails, a later invocation must not undo a successfully committed choice. Follow-up recovery may clear a committed marker without touching the core.
- Ruling: capture direct selection, sorted members, controller endpoint/source/process and readable profile ID; re-check before/after each PUT. Ambiguous response allows known previous/pending selections, not a third external choice.
- Ruling: proxy mutations are journaled before writing, conditionally restored only if fields still match previous/owned states; failure and external update leave recovery evidence. Repeated repair cannot replace the original snapshot.
- Ruling: retain existing discovery/string wrappers for compatibility; the transaction runtime adapter uses detailed CoreApi and captured-profile sync. CLI integration is Task 5, not yet delivered.
- Task 5 Ruling: independent check/watch proxy repair owns the mutation lock and conditionally restores failed writes, but a successful standalone repair persists across later website rejection; joint fix node/proxy mutation remains one journaled transaction, as specified. Half-write and external-change fixture RED function absent -> GREEN; legacy unsafe selection sync removed and database isolation tests migrated to captured-profile production writer.
- Task 6 Ruling: push only the isolated development branch now to obtain the actual clean-runner evidence required by the approved spec; final original-branch integration remains after all P0/P1/P2 and review. User goal expressly authorizes commit/push and delegated recommended choices. No public Release is created.

### 2026-10-16-p1-scan-history-watch

- Ruling: keep inline execution and one whole-goal final reviewer under user delegated recommended choices. P0 CI continues independently; no P0 completion claim until actual runner green.
- Task1 RED→GREEN: parser/exe rejected controls; scheduler missing; unissued retry counted1 instead0. All fixed.108 Rust pass/2 ignored;13 full CLI tests plus controller fixture pass. Real truncated response:3 requests/2 nodes,777ms query,peak1. Website100ms fails2/restores with no sample retry. Ruling: transient retries influence adaptive overload feedback even after success; fixed mode bounded batches may wait for slow peer. All-node failures check controller health before blaming nodes. Tests/logs preserve evidence.
- Task2 RED→GREEN: history APIs absent; exe produced no DB; external switch left1 misattributed verification row instead0. Added independent schema1 SQLite, SHA256 scope/target/node fingerprints, bounded100ms writes,30day/10000 physical retention, unknown/corrupt/readonly rejection and nonfatal recording. Current/transaction state is revalidated before assigning website evidence.112 Rust pass/2 ignored and18 full actual-exe cases pass; damaged bytes preserved, locked scan succeeds, history commands require no core requests. Fixture fixes: Python connection context did not close handle; corrupt assertion belonged in separate test. Ruling: display fingerprint aliases rather than persist arbitrary names that may embed credentials; verified config paths permit pipe rotation, anonymous endpoint/pipe instances remain isolated. Probe and website sessions stay distinct; default scan stays live-delay order.
- Task3 RED→GREEN: scoring interfaces absent; deterministic real-exe fixture chose fast target with poor history instead of reliable original; history basis API absent. Added finite50/25/15/10 formula, Laplace/median/MAD/cost, cold priors, expiry and grade-first verified-only candidates.116 Rust pass/2 ignored;20 full actual-exe tests pass. Reports include score and verified candidates; offline history basis uses all retained rows, not100-row display cap. Ruling: samples count complete website verification attempts, success requires all requested200; final confirmation is a separate attempt and candidate scoring reads history before current write. This is more conservative than counting individual requests and excludes fallback/API request inflation. Probe history never counts as webpage success. Previous89ca7d8 CI38775627891 passed both Rust versions.
- Task4 implementation: watch.rs owns typed events and monotonic policy; health snapshot rechecked before and after mutation ownership. Ruling: scope change resets prior cooldowns/optimization clock and starts manual hold; zero explicitly disables each protection. Proven node failures alone incur cooldown; controller/config/local-state errors do not count as node quality. Baseline/candidate have identical sample counts; final benefit gate can restore original. Runtime identity is SHA256 to exclude endpoint credentials and paths. Rust125 pass/2 ignored; nine script tests pass; CLI final run pending.
- Task4 final evidence:30 real CLI cases pass;125 Rust pass/2 ignored;9 script tests. Clean Windows CI38789612038 passed Rust1.98 and1.99 at895ce1a including release CLI and package. Ruling: all probe HTTP errors with healthy version endpoint remain controller/environment errors, not node failures; protects history/cooldown. P0 encoding follow-up42e958f verified9 script cases.

### 2026-10-16-p2-output-diagnose-browse

- Pre-flight: Task1 node evidence and typed workflow reports feed Task2 whitelist and Task3 browse; report output boundary owns every text helper. Task3 must preserve expected identity snapshot through P0 transaction. Task4 audits all interfaces. No conflicts. Ruling: preserve existing text presenters while recording their same typed business evidence before rendering; do not capture or parse printf output. Output closure triggers cooperative cancellation only for global operations, cleanup remains uncancelled. Config JSON whitelists supported fields; dry-run skips writes.
- Task2 RED→GREEN: diagnose rejected; now2 real-exe diagnostic tests pass including arbitrary secret-bearing names, offline copied/single exe, readonly request/state byte checks and existing-file refusal/explicit overwrite.127 Rust pass/2 ignored,7 JSON and30 transaction cases green. Ruling: diagnostic historical errors use read-only own SQLite categories, never raw messages/rows; no history initialization or schema migration. Node/group/profile/endpoint aliases are report-local; version accepts numeric dotted whitelist. Source integrity checks cwd only, separately from embedded build-time evidence. All ordinary Cargo builds now hash-check native manifest artifacts too.
- Task3 RED→GREEN: missing model APIs and real terminal rejected browse;130 Rust pass/2 ignored,4 real ConPTY acceptance cases,7 JSON,2 anonymous diagnose and30 transaction cases pass. Ruling: cooked-line menus preserve console modes; root captures/restores codepages/modes at every exit and input waits observe cancellation25ms. IDs bind name to normalized identity/profile/group/members/current choice; collisions are ambiguous and rejected. No stale ordinal interpretation. Only legal Selector/select groups offered; ambiguous group requires choice. use_flow_expected checks preview snapshot both before and after lock; explicit use remains ungated by automatic benefit thresholds. ConPTY host clears only its own std-handle table during CreateProcess to avoid redirected host handle inheritance, restores immediately; lifecycle follows Microsoft sample.

## 外部环境与限制

- 没有修改本机真实代理客户端的当前选择或注册表系统代理作为验收。测试使用本地假内核/代理、隔离APPDATA及仅测试用注册表项。
- 强制杀进程/断电不会保证立即恢复；保留DPAPI恢复记录，后续显式恢复时按身份与现状校验。
- 固定MSVC兼容下限及Windows x64静态包；其他平台未承诺支持。
- 浏览器指纹、真实网站风控与长时间守护需在用户实际网络环境继续观察；两项真实外部测试保持忽略。

## 独立审查与延后小项

待独立审查后补充。
