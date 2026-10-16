# 0.4.0 P0/P1/P2 交付审计

范围：Windows x64 单文件 CLI；完整三个阶段与 scan 延迟列对齐。

## 当前验证

- 本机：130项 Rust 通过，2项真实外部环境测试忽略；31项事务、13项输出/JSON、3项诊断、4项真实ConPTY和9项构建脚本验收。审查修复后的实际 exe 与 Rust 全套已重新运行通过。
- P1 双版本干净CI：[38789612038](https://github.com/wangdxnum1/proxpilot/actions/runs/38789612038)，Rust1.98/1.99均成功；P0首次打包产物已逐文件核验，见P0验证记录。
- 最终代码 `a59f85c` 的 [Windows Rust1.98/1.99 干净CI](https://github.com/wangdxnum1/proxpilot/actions/runs/38858687818) 全部成功，含51项真实release exe验收、9项脚本故障注入和静态依赖校验。
- 原开发目录已快进整合代码并重新构建；原目录再跑130项Rust、31项事务、13项输出/JSON、3项诊断及4项真实ConPTY，全部通过。
- 交付 `bin/proxpilot.exe` 版本0.4.0，SHA256：`33fe4dd4db53414ad57fc0e31a125cc9e8d7a7b30abae487181f9d8a8fee9129`，与原目录本次Cargo release相等。
- CI ZIP SHA256：`c958e7a66d66322e908ba42ef66aef9f623f0ad939cbfa03666af6e697a56685`；包内6项文件校验和全部核验，版本0.4.0、revision a59f85c、dirty=false、native_integrity_verified=true。CI exe SHA256：`c7bd2e362e145d21c80ef340e7c577a6cd1e24972d3e6b32dec591383f1d0eba`。本机/CI编译环境不同，不承诺两者逐字节相同。
- 最后交付记录提交仅修改文档与完成清单；exe内嵌构建代码 revision 保持 `a59f85c`，对应上述已验证源代码。[最终开发分支CI记录](https://github.com/wangdxnum1/proxpilot/actions?query=branch%3Acodex%2Fclient-selection-clash-verge) 与Git提交记录为最终推送依据。

## 代理代为作出的完整实施裁定

以下逐条保留历史执行账本；历史阶段的“待完成”措辞不代表当前状态。每条附裁定的潜在代价。

### 2026-10-15-p0-reliable-execution

- Ruling: use the native inline execution recommendation — tightly coupled control/error boundaries benefit from one implementer; retain final independent review.
  - 潜在代价：中途没有逐任务独立复核，由最终独立审查和回归补足。
- Ruling: approved design and implementation plan review gates are delegated by the user's updated goal; proceed without additional questions.
  - 潜在代价：推荐选择由代理代办，执行方式可能不同于用户逐项人工选择。
- Ruling: delay cells now contain numbers only under 延迟(ms), with shared column widths; this supersedes the earlier per-row ms presentation.
  - 潜在代价：旧文本解析脚本需适配表头单位和纯数字单元格。
- Ruling: create the skill workspace with its shipped script; on Windows store commands/results directly if task wrapper Bash cannot invoke cargo reliably. Never substitute a plan tick for actual test output.
  - 潜在代价：临时执行账本清理后以已提交报告、Git和CI为记录。
- Ruling: explicitly depend on the already locked btls 0.5.6 with no default features — wreq exposes a narrow is_tls flag but connector errors wrap btls::ssl::Error; structural downcast avoids string guessing and does not change native ABI/features.
  - 潜在代价：升级原生ABI须同时更新静态库、绑定及清单。
- Ruling: keep temporary typed-to-string wrappers only in discovery compatibility APIs; CoreApi requests, browser verification reports and scan measurements retain structured errors for later P2. A report, not the display message, remains authoritative.
  - 潜在代价：旧发现兼容入口部分提示仍是本地文本。
- Ruling: use an exclusive share-mode Windows file handle under per-user APPDATA instead of a named mutex — naturally coordinates sessions of the same account and releases on process death without trusting a stale file.
  - 潜在代价：互斥范围按Windows账户，不承诺跨账户协调。
- Ruling: Windows DPAPI with UI forbidden and purpose entropy protects journal payloads; MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH) publishes only a fully synced ciphertext file. Only this call's newly created temp file may be cleaned up.
  - 潜在代价：日志不能直接跨用户或机器迁移。
- Ruling: persist a committed phase to the encrypted journal BEFORE memory sync/removal — if deletion fails, a later invocation must not undo a successfully committed choice. Follow-up recovery may clear a committed marker without touching the core.
  - 潜在代价：提交后记忆同步失败不会撤销已经验证的内核选择。
- Ruling: capture direct selection, sorted members, controller endpoint/source/process and readable profile ID; re-check before/after each PUT. Ambiguous response allows known previous/pending selections, not a third external choice.
  - 潜在代价：内核无CAS，不能识别瞬间ABA；已检测改动会保留。
- Ruling: proxy mutations are journaled before writing, conditionally restored only if fields still match previous/owned states; failure and external update leave recovery evidence. Repeated repair cannot replace the original snapshot.
  - 潜在代价：外部改动可能阻止恢复，需要用户检查保留的日志。
- Ruling: retain existing discovery/string wrappers for compatibility; the transaction runtime adapter uses detailed CoreApi and captured-profile sync. CLI integration is Task 5, not yet delivered.
  - 潜在代价：保持发现接口兼容，新输出路径仍须保留类型信息。
- Task 5 Ruling: independent check/watch proxy repair owns the mutation lock and conditionally restores failed writes, but a successful standalone repair persists across later website rejection; joint fix node/proxy mutation remains one journaled transaction, as specified. Half-write and external-change fixture RED function absent -> GREEN; legacy unsafe selection sync removed and database isolation tests migrated to captured-profile production writer.
  - 潜在代价：独立系统代理修复成功后保留，即使后续网站验证失败。
- Task 6 Ruling: push only the isolated development branch now to obtain the actual clean-runner evidence required by the approved spec; final original-branch integration remains after all P0/P1/P2 and review. User goal expressly authorizes commit/push and delegated recommended choices. No public Release is created.
  - 潜在代价：为取得真实CI证据保留一个额外开发分支与产物。

### 2026-10-16-p1-scan-history-watch

- Ruling: keep inline execution and one whole-goal final reviewer under user delegated recommended choices. P0 CI continues independently; no P0 completion claim until actual runner green.
  - 潜在代价：中途没有逐任务独立复核，由最终独立审查和回归补足。
- Task1 RED→GREEN: parser/exe rejected controls; scheduler missing; unissued retry counted1 instead0. All fixed.108 Rust pass/2 ignored;13 full CLI tests plus controller fixture pass. Real truncated response:3 requests/2 nodes,777ms query,peak1. Website100ms fails2/restores with no sample retry. Ruling: transient retries influence adaptive overload feedback even after success; fixed mode bounded batches may wait for slow peer. All-node failures check controller health before blaming nodes. Tests/logs preserve evidence.
  - 潜在代价：同批最慢请求会拖慢下一批，吞吐可能低于连续工作池。
- Task2 RED→GREEN: history APIs absent; exe produced no DB; external switch left1 misattributed verification row instead0. Added independent schema1 SQLite, SHA256 scope/target/node fingerprints, bounded100ms writes,30day/10000 physical retention, unknown/corrupt/readonly rejection and nonfatal recording. Current/transaction state is revalidated before assigning website evidence.112 Rust pass/2 ignored and18 full actual-exe cases pass; damaged bytes preserved, locked scan succeeds, history commands require no core requests. Fixture fixes: Python connection context did not close handle; corrupt assertion belonged in separate test. Ruling: display fingerprint aliases rather than persist arbitrary names that may embed credentials; verified config paths permit pipe rotation, anonymous endpoint/pipe instances remain isolated. Probe and website sessions stay distinct; default scan stays live-delay order.
  - 潜在代价：离线历史隐藏真实名称，定位需结合当前实时结果。
- Task3 RED→GREEN: scoring interfaces absent; deterministic real-exe fixture chose fast target with poor history instead of reliable original; history basis API absent. Added finite50/25/15/10 formula, Laplace/median/MAD/cost, cold priors, expiry and grade-first verified-only candidates.116 Rust pass/2 ignored;20 full actual-exe tests pass. Reports include score and verified candidates; offline history basis uses all retained rows, not100-row display cap. Ruling: samples count complete website verification attempts, success requires all requested200; final confirmation is a separate attempt and candidate scoring reads history before current write. This is more conservative than counting individual requests and excludes fallback/API request inflation. Probe history never counts as webpage success. Previous89ca7d8 CI38775627891 passed both Rust versions.
  - 潜在代价：按完整验证批次计样本更保守，最终确认另算一次。
- Task4 implementation: watch.rs owns typed events and monotonic policy; health snapshot rechecked before and after mutation ownership. Ruling: scope change resets prior cooldowns/optimization clock and starts manual hold; zero explicitly disables each protection. Proven node failures alone incur cooldown; controller/config/local-state errors do not count as node quality. Baseline/candidate have identical sample counts; final benefit gate can restore original. Runtime identity is SHA256 to exclude endpoint credentials and paths. Rust125 pass/2 ignored; nine script tests pass; CLI final run pending.
  - 潜在代价：保护和冷却可能推迟主动优化；明确故障仍可修复。
- Task4 final evidence:30 real CLI cases pass;125 Rust pass/2 ignored;9 script tests. Clean Windows CI38789612038 passed Rust1.98 and1.99 at895ce1a including release CLI and package. Ruling: all probe HTTP errors with healthy version endpoint remain controller/environment errors, not node failures; protects history/cooldown. P0 encoding follow-up42e958f verified9 script cases.
  - 潜在代价：若应用范围判断错误，需要调整对应边界并重跑相关验收。

### 2026-10-16-p2-output-diagnose-browse

- Pre-flight: Task1 node evidence and typed workflow reports feed Task2 whitelist and Task3 browse; report output boundary owns every text helper. Task3 must preserve expected identity snapshot through P0 transaction. Task4 audits all interfaces. No conflicts. Ruling: preserve existing text presenters while recording their same typed business evidence before rendering; do not capture or parse printf output. Output closure triggers cooperative cancellation only for global operations, cleanup remains uncancelled. Config JSON whitelists supported fields; dry-run skips writes.
  - 潜在代价：新增命令须继续接入报告边界，不能随意直接打印。
- Task2 RED→GREEN: diagnose rejected; now2 real-exe diagnostic tests pass including arbitrary secret-bearing names, offline copied/single exe, readonly request/state byte checks and existing-file refusal/explicit overwrite.127 Rust pass/2 ignored,7 JSON and30 transaction cases green. Ruling: diagnostic historical errors use read-only own SQLite categories, never raw messages/rows; no history initialization or schema migration. Node/group/profile/endpoint aliases are report-local; version accepts numeric dotted whitelist. Source integrity checks cwd only, separately from embedded build-time evidence. All ordinary Cargo builds now hash-check native manifest artifacts too.
  - 潜在代价：损坏/不可读模块只报告缺失原因，不输出原始数据。
- Task3 RED→GREEN: missing model APIs and real terminal rejected browse;130 Rust pass/2 ignored,4 real ConPTY acceptance cases,7 JSON,2 anonymous diagnose and30 transaction cases pass. Ruling: cooked-line menus preserve console modes; root captures/restores codepages/modes at every exit and input waits observe cancellation25ms. IDs bind name to normalized identity/profile/group/members/current choice; collisions are ambiguous and rejected. No stale ordinal interpretation. Only legal Selector/select groups offered; ambiguous group requires choice. use_flow_expected checks preview snapshot both before and after lock; explicit use remains ungated by automatic benefit thresholds. ConPTY host clears only its own std-handle table during CreateProcess to avoid redirected host handle inheritance, restores immediately; lifecycle follows Microsoft sample.
  - 潜在代价：采用键盘行命令，不提供桌面或鼠标界面。
- Task4 own-audit RED→GREEN: --secret watch nodes --json was incorrectly emitted as two watch events. Command detection now skips values of all declared flags; actual-exe regression passes. Ruling: bump package to0.4.0 for the three-stage feature set and preserve previous commands/default help; add all new real-exe suites to both Rust CI versions. Final delivery is fast-forward into original codex/client-selection-clash-verge and push under goal authorization; no public release/tag or PR requested.
  - 潜在代价：依赖旧版本文本/版本号的外部脚本需更新。
- Final: Ruling: use available gpt-6.1-sol high for the one independent review after requested most-capable gpt-6-astra hit usage limit before reviewing — preserves fresh review without stalling authorized work — cost: reviewer differs from initially selected model. Final: Ruling: preserve localized discovery text in non-serializable CommandFailure.detail while carrying AppError unchanged to JSON — no message parsing/last-discovery guessing — cost: legacy discovery inventory hints remain text-only, structured failed inventory entries use unavailable category.
  - 潜在代价：审查模型与最初选择不同；保留了独立新上下文审查。
- Final own-audit: authenticated --proxy credentials were echoed by textscan summary in actual6b4aa50 release. Actual-exe regression RED→GREEN after display-only URL redaction;13 output tests and130 Rust pass/2 ignored. Ruling: redact URL userinfo/query/fragment in summaries, website progress and HTTP alternatives; leave original request and identity inputs unchanged — protects logs without changing routing/auth — cost: text diagnostics intentionally omit those private URL components. Clean CI38857792649 passed both toolchains for reviewed6b4aa50; display follow-up needs its own cleanCI.
  - 潜在代价：日志隐藏私有URL组件，精确地址需在本地配置中检查。

## 外部环境与限制

- 没有修改本机真实代理客户端的当前选择或注册表系统代理作为验收。测试使用本地假内核/代理、隔离APPDATA及仅测试用注册表项。
- 强制杀进程/断电不会保证立即恢复；保留DPAPI恢复记录，后续显式恢复时按身份与现状校验。
- 固定MSVC兼容下限及Windows x64静态包；其他平台未承诺支持。
- 浏览器指纹、真实网站风控与长时间守护需在用户实际网络环境继续观察；两项真实外部测试保持忽略。

## 独立审查与延后小项

独立审查覆盖全部三阶段与各计划 Review Focus，结论“修复后可合入”。没有 Critical、Minor 或 Declined to judge；以下四项 Important 已修复：

1. 恢复 dry-run：retry/discard 均在访问 API 前转入本地只读预览。实际崩溃恢复夹具先复现节点被恢复/日志被删除，再验证节点、请求记录及 DPAPI 密文字节保持不变。
2. 输出写入失败：仅真正断管正常退出0，权限/存储等输出错误退出1。实际只读 stdout 句柄测试 RED→GREEN，同时保留 watch 早关管道退出0。
3. JSON 错误类别：API JSON、版本/端口、客户端身份、策略组、历史/恢复状态在最终边界保留 AppError；CommandFailure 把本地文本提示与可序列化错误分开。实际 exe 覆盖无效组/节点、空倍率范围、损坏响应/历史、身份不匹配，取消也不复用先前局部 HTTP 错误。
4. 诊断取消：模块之间、文件写入前及静态入口检查取消；阻塞假控制接口后发送真实控制台事件，验证退出130、cancelled=true 且文件未创建。

审查最初指定的 gpt-6-astra 在开始审查前触发账户限额，换用当时可用的 gpt-6.1-sol high 完成唯一一次独立全分支审查。两项真实外部环境测试没有执行；独立审查的额外执行是定向假内核验证，完整回归由实现者运行。

候选 CI 38793113593：Rust1.98 全部成功；Rust1.99 仅失败于旧测试把 exe 启动/发现/回滚一起计入0.9秒。该测试改为请求开始后的响应事件门：服务端不响应，要求客户端在默认12秒超时之前结束为 typed timeout、请求数1、恢复原节点。没有放宽生产超时或增加重试。


额外隐私验收：原文本扫描摘要回显显式代理用户名/密码；实际release复现后，显示层移除userinfo/query/fragment，原请求与身份输入保持原样。新输出回归及最终双版本CI均通过。
