# ProxPilot Git 长连接检测（一期）设计

日期：2026-11-11。状态：**已实施（v0.6.0）**。
范围决策（用户已确认）：**一期最小可用** / 默认探测目标用**可控字节数端点** / 长连接结果**只在同验证等级内参与排序**。
追加决策（用户确认）：**尽量并行计算，不增加过多时间** —— 落实为单节点内并发采样，见 §3.3。

---

## 0. 实施后回填

| 项 | 落实结果 |
|---|---|
| 并发策略 | 单节点内 N 次采样在 `new_current_thread` runtime 上用 `JoinSet` 并发；候选之间串行（内核选择是全局状态）。未启用 `rt-multi-thread`，不引额外 OS 线程、不改依赖。 |
| 默认字节数 | 从 8 MiB 下调为 **2 MiB**（失败发生在开始传 body 的瞬间，2 MiB 足以触发；墙钟显著缩短）。 |
| 默认采样次数 | 5 次；对约 44% 失败率的检出概率约 94%。 |
| 默认超时 | 整体 15 s、首字节 5 s；首字节超时用于区分「掐连接」与「卡死」，且不大于整体超时。 |
| 归因实现 | `checker::classify_stream()` 纯函数，6 项单测覆盖 healthy / cut / degraded / unknown / 空 / 不泄露 URL。 |
| 排序实现 | `scoring::stream_rank()` 插在 `grade` 之后、`score.total` 之前；未启用时全为 `None`，排序逐项退化。 |
| 测试基线 | 改动前 128 passed + 6 failed（环境性）+ 2 ignored；改动后 142 passed + 同为 6 failed + 2 ignored → **净增 14 项通过、零新增失败**。`tests/cli_json.py` 在基线与改动后均为 `failures=3, errors=13`（沙箱 `%APPDATA%` 受限，与本次改动无关）。 |
| 实机验证 | 当前节点：ChatGPT 网页 1.71 s 通过（✔），同一节点长连接 5 次采样中 **3 次早期中断、仅 2 次完整传输**，中位吞吐 196 KB/s → 判 `cut`。与外部项目实测的 44–55% 失败率吻合，且 `check` 退出码仍为 0。 |

### 实测耗时（本机，默认参数）

| 命令 | 未启用 | 启用后 | 说明 |
|---|---|---|---|
| `check --git-check` | 3–6 s | 约 8–12 s | 单节点 5 次并发采样，墙钟 ≈ 最慢一次采样 |
| `fix --git-check` | 40–60 s | 约 55–75 s | 每个网页通过的候选增加约 2–5 s，另加最终确认 1 次采样 |
| `scan` / `watch` | 不变 | 不变 | 一期不集成；`scan --git-check` 会明确提示该命令不做长连接检测 |

> 并发带来的关键收益：5 次采样从「5 × T₁」压缩到「≈ max(T₁)」。最坏情形（节点卡死）由 `--git-timeout-ms` 单次预算封顶，不再线性累加 5 倍。

### 输出与噪音控制

- **默认只给结论**：一行判定（如「✘ 不稳定（同时开 5 条连接各下载一个文件，4 条成功、1 条被掐断）」）+ 一条可操作建议。
- **`--verbose` / `-v`** 才展开：失败占比、吞吐数值、为何会表现为「时好时坏」、探测范围说明。
- 历史库读写异常同样降级为静默（历史只是评分辅助，检测本身已成功），仅 `--verbose` 提示；原因始终保留在 JSON `warnings`，脚本仍可感知。
- **流量护栏**：显式同时设置 `--git-bytes` 与 `--git-samples` 时，乘积超过 256 MiB 直接拒绝，避免误配置拉取 GiB 级流量。

### 已知限制（一期）

- `--git-check` 对 `scan` 无效（快速可达性排序不承担大流量探测），已给出明确提示而非静默忽略。
- `watch` 未集成：节点退化（好节点变差）不会被守护自动发现，需手动 `check --git-check`。二期处理。
- 不计入历史分层与参考分；stream 只在同验证等级内打破平手。
- 单次最终确认也只跑 1 条采样，不阻断网页验证结果（硬门槛 `--require-git` 列三期）。

---

## 1. 目标与依据

### 1.1 要解决的问题

ProxPilot 优选后的节点「ChatGPT 能开、网页正常，但 git 时好时坏」。根因已由另一个项目的实测确认，不是配置、不是证书：

- `../gitpilot/doc/git-proxy-diagnosis.md` §3 把「代理出口节点在掐长连接」列为第一主因；§5 判别法则：「网页正常、只有 git 挂 → 长连接被掐，几乎必然不是证书」。
- §2.3 / §2.4 的对照实验：换节点前 `clone ×3` 为 2/3 含 1 次 6.3s 硬失败，换节点后同套测试 3/3 成功、`gstatic` 5/5。
- `../gitpilot/doc/long-connection-probe-20261111.md`（2026-11-11 14:00–14:08）：当前节点 `clone --depth 1 git/git` **9 次 5 成功 4 失败**，失败全部为 3.1–3.2s 的 `curl 56 / early EOF`；成功样本吞吐 2.6–3.5 MB/s（带宽充足，是不稳定不是慢）；`CONNECT` 200、TLS 握手 OK、HTTP/2 响应头全部收完，断在「开始传 pack 数据流」那一刻。

### 1.2 为什么现有检测测不出

现有验证链路全部是**短请求**，且只有一个注入点：

| 环节 | 代码 | 性质 |
|---|---|---|
| 初筛 | `probe_scheduler.rs` → 内核 `/proxies/{node}/delay` | 单次短请求，任何 HTTP 响应即算可达 |
| 真实验证 | `checker.rs::verify_access_report_with_timeout` → `http.rs::BrowserProbe::request` | `--samples` 次**互相独立**的短请求；每次重建 Client、`forbid_recycle()`、`chatgpt.com` 首页仅几十 KB |
| 评分 | `scoring.rs::score_at` | 成功率 50 / 延迟 25 / 稳定性 15 / 成本 10；「稳定性」是历史**网页耗时**的 MAD，不是长连接存活能力 |
| 排序 | `scoring.rs::compare_candidates` | `grade` → `score.total` → `mean_ms` → `node` |

因此「短请求 100% 通过、44% 概率掐断长连接」的节点必然被评为 `Webpage` + 低延迟 + 高分，被 `fix` / `watch --optimize` 选中并保持。

**关键有利条件**：`workflow.rs::measure_node()` 是 `fix` / `use` / `watch` 健康检查 / `optimize_flow` 共用的唯一验证函数，且已在 `SwitchTransaction` 内完成「切换 → 验证 → `validate_current()` → 记历史」。扩展它即可让所有流程受益。

### 1.3 一期目标

一条命令能回答「**当前/候选节点对 git 大流量长连接友不友好**」，并让 `fix` 在同等级候选中优先选健康的节点。

### 1.4 明确不做（一期）

- 不改 `Availability` 三档（`Webpage > IpOnly > Failed`）与 `grade()` 语义。
- 不改 50/25/15/10 评分公式，stream 结果不混进 `success`/`latency`/`stability`。
- 不集成 `watch`（低频健康检查、冷却、单调时钟属二期）。
- 不写 `history.sqlite3`、不加 `diagnose` 聚合项。
- `scan` 不跑大流量探测，保持「默认排名只看本轮初筛延迟」的既有约定。
- 不自研 git 客户端行为模拟，不调用 `git.exe` / `curl.exe`。
- 不提供 `--require-git` 硬门槛（三期可选）。
- 不改退出码语义，不改 `~/.gitconfig`，不改系统代理以外的任何既有行为。

---

## 2. 权威基线

| 项 | 实测值 |
|---|---|
| 源码 | `main` @ `ae2cd00`，工作区干净，`origin/main` 同步 |
| 版本 | 0.5.0 |
| 工具链 | Rust 1.99.0 / `x86_64-pc-windows-msvc` / 静态 CRT |
| 测试基线 | Rust 内联单测 136 项；`tests/` 6 个 Python 验收脚本约 1270 行 |
| 输出边界 | `report.rs` 单一边界，文本/JSON/NDJSON 复用同一批业务证据 |
| 公共约束 | `docs/superpowers/specs/2026-10-15-reliability-roadmap.md` §公共约束 1–9 |

---

## 3. 检测项一期清单

| ID | 检测项 | 判据 | 一期 |
|---|---|---|---|
| G1 | 流式大对象传输完整性 | 逐帧读 body，记录 `first_byte_ms`、`received/expected`、实收比、吞吐；不缓冲整块 | ✅ |
| G2 | 多次采样成功率 | 单次采样测不出 5/9 这类概率性故障，必须 N 次独立采样统计 | ✅ |
| G3 | 早期中断 vs 限速 vs 环境归因 | 沿用 gitpilot §5 判别法则 | ✅ |
| G4 | Git 关键域名可达性 | `codeload.github.com` 等作为 `--git-url` 可配推荐值，不默认跑 | 部分 |
| G5 | 最终复验 | `final_candidate` 内用 1 次采样复验，写入报告 | ✅（不阻断） |
| G6 | watch 低频健康检查 + 冷却 | 节点会退化（11-10 好节点 → 11-11 变 5/9） | ❌ 二期 |
| G7 | 空闲保活检测 | 成本高、收益不明 | ❌ 不做 |

### 3.1 G1 实现要点

失败样本的特征是「**响应头全部正常收完 → 3.1s、0 KB 断开**」，中断发生在开始传 body 的瞬间。因此不必每次传满 8 MB：设计成「流式读取 + 记录中断位置」，`--git-bytes` 作为**期望字节数与读取上限**，失败时立即下结论。

每次采样记录 `received_bytes` 与 `elapsed_ms` —— 这两个值正是区分「掐连接 / 限速」的证据，直接对应判别法则。

新增独立 `StreamProbe`（`http.rs`），**不修改现有 `BrowserProbe`**：两者超时预算不同（网页 12s / 长连接 30s），`BrowserProbe::builder()` 的 `.timeout(self.timeout)` 是构造时固定的。独立结构可避免回归现有网页验证。复用 `wreq + BoringSSL + Chrome149 指纹`，流量走 `be.proxy`，每次采样重建 Client 且不复用连接。

### 3.2 G3 归因表（写入 `StreamReport.health`）

| 观测 | 判定 | 依据 |
|---|---|---|
| `Tls` / `Dns` / `Connect` / `ProxyConnect` 立即失败 | `Unknown` + 保留 `AppError`，**不算节点质量失败** | 项目既有原则：控制接口/环境错误不惩罚节点；对应 gitpilot 的「探测环境假阳性」 |
| 实收 < 期望、`< 10s` 硬失败、错误为 `ResponseTransfer`/`Timeout` | `Cut`（早期中断） | gitpilot §5「几秒内失败 + curl 56 / early EOF」 |
| 完整收齐但吞吐 < `--min-throughput-kbps` | `Degraded`（限速） | gitpilot §5「长时间缓慢、速度恒定很低」/ C12 |
| 全部采样完整收齐且吞吐达标 | `Healthy` | — |

**多次采样聚合**：`Healthy` 要求 `samples_completed == samples_requested` 且 `slow_samples == 0`；`early_cuts > 0` 直接判 `Cut`（一次被掐即足以说明不可靠）；其余为 `Degraded`。默认 5 次采样对 44% 失败率的检出概率约 94%，9 次约 99.9%，故 `--git-samples` 上限放宽到 10。

### 3.3 并发策略（用户要求：并行以压低耗时）

**能并行的只有「单个节点内的多次采样」**：

- 候选之间**不能**并行——所有流量走同一个代理出口，内核一次只选中一个节点，切换是全局状态；并发测试多个节点会得到互相污染的结论。
- 单节点内 N 次采样**可以**并行：它们经由同一出口、各自独立 TCP 连接。而目标故障模式恰恰是「每条新连接在开始传 body 时以一定概率被掐」，所以 N 条并发连接就是 N 次独立的伯努利试验，**检出率与串行完全等价，墙钟从 `N × T₁` 降到 `≈ max(T₁)`**。
- 实现：`tokio::runtime::Builder::new_current_thread()` + `JoinSet`。采样是 IO 密集型，单线程 reactor 足以重叠；不需要 `rt-multi-thread`，也不新增 OS 线程或依赖。
- 取消：`tokio::select!` 中 `biased` 优先响应 `Cancellation`，命中后 `abort_all()`。
- 资源上限：`--git-bytes` 同时是**读取上限**，并在累计字节达到期望值后立即停止读取，不会因为服务端发送更多而超出预算。

> 说明：早期版本的估算是串行的「C × S × T₁」（最坏 780 s）。改为并发 + 2 MiB 后，最坏情形被单次超时封顶，不再乘以采样次数。

---

## 4. 数据模型

`diagnostics.rs` 新增（均为 `Serialize` + `Deserialize`，新字段带 `#[serde(default)]` 保证兼容）：

```rust
pub enum StreamHealth { Healthy, Degraded, Cut, Unknown }

pub struct StreamSample {
    pub completed: bool,
    pub received_bytes: u64,
    pub expected_bytes: u64,
    pub first_byte_ms: Option<f64>,
    pub elapsed_ms: f64,
    pub throughput_kbps: f64,
    pub error: Option<AppError>,
}

pub struct StreamReport {
    pub samples_requested: usize,
    pub samples_completed: usize,
    pub early_cuts: usize,
    pub slow_samples: usize,
    pub min_received_ratio: f64,
    pub median_throughput_kbps: f64,
    pub health: StreamHealth,
    pub samples: Vec<StreamSample>,   // JSON 可选裁剪
    pub represents: String,           // 固定 "proxy_link"，见 §7
}
```

- `VerificationReport` 增加 `#[serde(default, skip_serializing_if = "Option::is_none")] pub stream: Option<StreamReport>`。
- `CurrentVerification` 增加 `pub stream: Option<StreamReport>`。
- 新增 `Stage::Stream`；**不新增 `ErrorCode`**，归因由 `health` 表达，传输错误复用 `ResponseTransfer` / `Timeout` / `Tls` 等既有类别。
- `Measurement::valid()` 的 `match self.stage` 已有 `_ => false` 兜底，加枚举值不破坏编译；一期**不写历史**，故不放宽校验。
- `CommitReport` **无需改动**：其 `verification: VerificationReport` 会自动带出 stream，JSON 由 `report.rs` 统一序列化。
- `Candidate` **无需改动**：`compare_candidates` 直接读 `a.verification.stream`。

---

## 5. 命令与参数

### 5.1 新增参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `--git-check` | 关 | 启用长连接检测。**默认关闭，保证现有行为逐字节不变** |
| `--git-url <URL>` | `https://speed.cloudflare.com/__down?bytes=<git-bytes>` | 自定义目标；codeload 作为推荐备选写入 README |
| `--git-bytes <N>` | `8388608`（8 MiB） | 期望字节数与读取上限，范围 1 MiB–64 MiB |
| `--git-samples <N>` | `5` | 独立采样次数，范围 1–10 |
| `--git-timeout-ms <N>` | `30000` | 单次采样整体预算，范围 100–120000 |
| `--min-throughput-kbps <N>` | `200` | 限速告警阈值，对应 gitpilot C12 |

`--git-url` 与 `--git-bytes` 的交互：自定义 URL 时 `--git-bytes` **只作为读取上限**，不改写 URL query；期望字节数取响应的 `Content-Length`（缺失则取上限，并标记 `expected_from_header: false`）。

> **待验证项**（实施第一步，不写代码前先跑一次）：确认默认端点在当前代理链路下可用、支持 `bytes` 参数、返回体可流式读取且 `Content-Length` 可信。若不可用，改为内置候选列表 + 首个可用者，并把失败明确报为 `Unknown` 而非 `Cut`；**绝不因端点问题误判节点质量**。

### 5.2 适用命令（一期）

| 命令 | 行为 |
|---|---|
| `check --git-check` | 对**当前节点**跑采样，报告健康度；**不改退出码**，只输出 warning + JSON 字段 |
| `fix --git-check` | 对**通过网页验证、进入 ranking 的候选（≤5）**跑采样；同等级内参与排序；最终确认用 1 次采样复验 |
| `use --git-check` | 手选节点验证后跑采样，**只报告不阻断**（`use` 无排序，且不应因 git 维度否决用户明确指定的节点） |
| `scan` | **不跑**，保持初筛延迟语义 |
| `watch` | 不集成；`args.git_check` 默认 false，`VerificationReport.stream = None`，行为不变 |

`--dry-run` 下不执行任何大流量探测（`fix --dry-run` 本就等价于 `scan`）。

### 5.3 成本前置过滤

顺序固定为：`scan 初筛` → `网页验证` → `stream 采样` → `final_candidate 复验`。**只对已通过网页验证的候选做 stream**，不对 `top 12` 全做。最坏 5 候选 × 5 采样 × 8 MiB ≈ 200 MiB + 1 次最终复验，且全部可配。

---

## 6. 优选策略（用户已确认：同等级内参与排序）

`scoring.rs::compare_candidates` 插入一个排序键，**位置在 `grade` 之后、`score.total` 之前**：

```rust
fn stream_rank(s: Option<&StreamReport>) -> (u8, u32) {
    match s {
        Some(r) if r.health == StreamHealth::Healthy  => (0, 0),
        Some(r) if r.health == StreamHealth::Degraded => (1, 0),
        None | Some(_ /* Unknown */)                  => (2, 0), // 未检测 = 中性
        Some(r) /* Cut */                             => (3, r.early_cuts as u32),
    }
}
```

**必须保证的回归约束**：`--git-check` 未启用时所有候选均为 `None`，排序键相同，`compare_candidates` 退化为现有 `grade → score.total → mean_ms → node`，逐项一致。

- 不改变 `grade()`；`Webpage > IpOnly > Failed` 仍是第一优先级，「参考分不能跨越本轮验证等级」仍成立。
- 不改变 `score()` 公式与 `ScoreBreakdown` 字段。
- `watch_policy::has_benefit()` 一期**不改**（其字段依赖未变，编译无影响）；二期处理 watch 收益门槛时再纳入 stream。

**G5 最终复验**：`final_candidate()` 内对选中节点跑 **1 次** stream 采样，结果写入 `CommitReport.verification.stream`。一期**不因复验为 `Cut` 而回滚或阻断**（避免把「网页可用但 git 不佳」误映射为验证失败、改变退出码语义），仅在文本与 JSON 中明确提示「最终确认时长连接检测为 X」。硬阻断留到三期 `--require-git`。

---

## 7. 与 GitPilot 的边界及标注

GitPilot 设计文档明确把「不切换节点」列为非目标，其 `probe --deep` 只给出「换节点」建议；ProxPilot 补上「换到对 git 也友好的节点」，两者不重叠。

但存在**必须显式标注的分歧**：GitPilot 强调探测要走真实 `git.exe`/curl 以保证与 git 行为一致；ProxPilot 用 `wreq + BoringSSL`。我们测的是**链路/节点能力（TCP 是否被掐）**，不是 git 客户端行为，用 wreq 反而能避开 GitPilot 遇到的 `SEC_E_NO_CREDENTIALS` 沙箱假阳性。

因此 `StreamReport.represents` 固定为 `"proxy_link"`，文本输出须注明「**不代表真实 git.exe 行为**」；可选 `--git-via git` 对照模式列三期。

---

## 8. 逐文件改动面

| 文件 | 改动 |
|---|---|
| `src/http.rs` | 新增 `StreamProbe` + 单次 `test_stream()`（逐帧计数、字节/耗时/中断位置、取消检查）；**不动 `BrowserProbe`** |
| `src/diagnostics.rs` | `Stage::Stream`、`StreamHealth`、`StreamSample`、`StreamReport`；`VerificationReport` 加可选 `stream` |
| `src/checker.rs` | 新增 `check_stream_report()` 聚合与归因；挂到 `verify_access_report_with_timeout`、`verify_current_report`（后者加 `stream` 字段及归属判定，见下） |
| `src/workflow.rs` | `measure_node()` 挂接 + stream 后追加 `validate_current()`；`final_candidate()` 1 次复验 |
| `src/scoring.rs` | 仅 `compare_candidates` 插入 `stream_rank` 键 |
| `src/main.rs` | `Args` 新字段、`parse_args_from` 参数解析（含范围校验与 URL 校验）、`command_name()` flag 跳过表、help、`cmd_check` 输出 |
| `src/switching.rs` | 无需改动 |
| `src/report.rs` / `src/diagnose.rs` | 一期不改（JSON 自动带出；诊断白名单不含 `VerificationReport`，不泄露 URL/节点名） |
| `src/history.rs` / `src/watch.rs` / `src/watch_policy.rs` | 一期不改 |
| `README.md` / `CHANGELOG.md` | 参数表、工作原理、FAQ 增补；注明 `--git-url` 的 codeload 推荐值与 `represents` 限制 |
| `tests/cli_json.py` | 新增 `--git-check` 的 JSON 结构与「未启用时逐字节不变」回归用例 |

### 8.1 易漏的两个回归点（写进实施 checklist）

1. **`main.rs::command_name()` 的 flag 跳过表必须同步加入全部新参数**。P2 Task4 曾修过 `--secret watch nodes --json` 被误判成两个 watch 事件的 bug（`docs/superpowers/2026-10-16-delivery-audit.md` §P2 Task4）。新增 `--git-url`/`--git-bytes`/`--git-samples`/`--git-timeout-ms`/`--min-throughput-kbps` 后若遗漏，`watch --json` 的事件归属会再次出错。
2. **结果归属**：`check` 的 stream 检测位于 `verify_current_report` 内「网页验证后、`control.state()` 复查前」，会拉长前后校验窗口。只有复查后 `stable == true` 才认为 stream 归属有效；否则把 `stream` 置为 `Unknown` 并记 `state_conflict`，**不把结果归属到可能已变化的节点**。这与现有「验证中选择或订阅改变时不把结果归属到旧候选」一致。

---

## 9. 公共约束核对

| 约束 | 遵守方式 |
|---|---|
| §3 `scan/nodes/info/...` 与所有 `--dry-run` 不改代理/节点 | `scan` 不跑；`--dry-run` 直接跳过 stream |
| §4 未知/冲突倍率不当 1 倍率 | 不改 `BillingPolicy`，stream 在过滤之后执行 |
| §5 仅 IP 通过保持独立状态 | 不改 `Availability`；`IpOnly` 候选同样可做 stream 且排序仍低于 `Webpage` |
| §6 不禁用证书验证、不打印/持久化凭据 | `StreamProbe` 沿用 `WebPKI` 根证书；`StreamReport` 不存 URL 明文（`--git-url` 可含敏感 query），只存 `represents` 与聚合数值 |
| §7 默认文本输出保持既有颜色/列布局 | 未启用时零输出变化；启用时追加独立段落，不动 scan 表格 |
| §8 退出码 0/1/2/130 | stream 不改变退出码 |
| §9 不承诺断电回滚 | stream 是只读探测，不进入 `SwitchTransaction` 的写入路径 |

---

## 10. 测试计划

**Rust 单测**（新增约 12 项，`http.rs` 的本地假服务器 fixture 已有 `test_support::listener()` 可复用）：

- 正常完整传输 → `Healthy`，`received == expected`，吞吐计算正确。
- 响应头后提前断流（0 字节 / 部分字节）→ `Cut`，`early_cuts` 计数，`min_received_ratio` 正确。
- 慢速但完整 → `Degraded`，非 `Cut`。
- TLS/Connect 立即失败 → `Unknown`，不判节点质量。
- 5 次中 2 次被掐 → 聚合为 `Cut`，成功率如实记录（**不许被单次成功掩盖**）。
- 取消在采样循环内生效 → `Cancelled`，不继续后续采样。
- `compare_candidates`：全部 `None` 时顺序与改动前逐项一致（**关键回归**）。
- `compare_candidates`：`grade` 仍优先于 stream（`Webpage + Cut` 仍胜过 `IpOnly + Healthy`）。
- 同等级内 `Healthy` > `Degraded` > `Unknown` > `Cut`；`Cut` 多者更靠后。
- `VerificationReport` 旧 JSON（无 `stream` 字段）可反序列化。
- `StreamReport` 序列化不含 URL 字段。
- 参数范围校验（0 / 越界 / 非法 URL）与 `command_name()` 跳过表。

**Python 验收**（`tests/cli_json.py`）：`--json` 下 `check --git-check` 的字段结构；未启用 `--git-check` 时输出与基线一致。

**人工验收**：本地假内核/`--api` 下跑 `fix --git-check`，确认只对网页通过的候选发大流量请求；`--dry-run` 零大流量；Ctrl+C 在采样中途立即退出。

---

## 11. 验收标准

1. `cargo test --locked --quiet`：136 项基线全绿 + 新增项全绿。
2. `cargo fmt --check` 通过；`scripts/build.ps1` 产出 exe 成功。
3. `tests/cli_json.py` / `cli_transactions.py` / `cli_diagnose.py` / `cli_browse.py` 全绿。
4. 不带 `--git-check` 时，`check` / `fix` / `use` / `watch --json` 的输出与基线**逐字节一致**。
5. 带 `--git-check` 时，能稳定区分「掐长连接」「限速」「环境问题」三类，且**不会因探测目标不可用而误判节点**。
6. 文本输出显式标注 `represents: proxy_link`、不代表真实 git.exe。
7. README 参数表、原理与 FAQ 完整；CHANGELOG 有 v0.6.0 条目。

---

## 12. 二期 / 三期展望

- **二期**：`watch` 低频长连接健康检查（每 N 周期或仅故障确认/主动优选时跑）、`watch_policy::cooldown_failure` 纳入 `Stage::Stream`、`has_benefit` 收益门槛纳入 stream、`history` 分层记录 `Stage::Stream`、`diagnose` 白名单加聚合项、节点退化自动修复。
- **三期**：`--require-git` 硬门槛、可选 `--git-via git` 对照模式（明确标注不代表日常配置）、`--git-deep` 空闲保活检测。
