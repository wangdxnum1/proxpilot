# Client Selection and Clash Verge Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 支持 `--client clash-verge`、命令行保存默认客户端、自动扫描选择，以及 Clash Verge 的 Windows 命名管道内核 API。

**Architecture:** 客户端选择负责发现并验证唯一目标；CoreApi 统一 HTTP/命名管道传输；现有检测、节点扫描、优选和 watch 共用验证后的 Backend。系统代理和客户端选择记录操作以该 Backend 为边界。

**Tech Stack:** Rust 1.98+、Windows 10/11、reqwest、Tokio、hyper 1 / hyper-util、serde_json、serde_yaml_ng 0.10、现有 wreq BrowserProbe。

**Spec:** `docs/superpowers/specs/2026-10-08-client-selection-and-verge-design.md`

## Global Constraints

- 客户端类型为 `cutecloud`、`clash-verge`；`auto` 是选择模式。
- 优先级：显式 --api > 本次 --client > --detect > 保存 default-client > CuteCloud 兼容默认值；显式 --api/--client 的身份冲突必须报错。
- 默认配置 `%APPDATA%\ProxPilot\config.json`，schema_version=1，不存 secret。
- scan、clients、clients --supported、config、check --dry-run 不修改系统代理或客户端选择。
- 自动选择歧义必须报错；显式目标不可连接不回退到其他客户端。
- 保留 ≤200ms scan 颜色及默认 watch 故障切换 / --optimize 主动优选行为。
- 第一阶段 Verge 切换验证以内核为准，不写运行中的 profiles.yaml、不宣称客户端重启持久化已同步。
- 所有自动测试使用临时文件、临时命名管道、模拟 API/代理及隔离注册表路径。

## Review Focus

1. 默认配置损坏时，显式目标仍可连接；Task 1/4 测试。
2. Verge 管道路径在客户端重启后改变，watch 重连同一客户端而非切到另一家；Task 2/4 测试。
3. 并发扫描管道响应可能分块、截断或停滞，每个任务有整体超时；Task 2 测试。
4. 系统代理指向 CuteCloud，但目标为 Verge，控制和实测必须都指向 Verge；Task 3/5 测试。
5. 明确连接远程 API 时，本机选择记录不得被误写；Task 3/5 测试。

## 文件结构

- `src/client_config.rs`：客户端枚举、ProxPilot 配置读写。
- `src/core_api.rs`：HTTP/命名管道 API 请求与响应。
- `src/clients.rs`：本地配置发现、扫描、自动/显式选择。
- `src/detect.rs`：Backend 模型与现有 setup 兼容入口。
- `src/mihomo.rs`：通过 CoreApi 访问内核及读取运行端口。
- `src/main.rs`：新参数和 config/clients 命令、watch 目标重连。
- `src/sysproxy.rs`、`src/appstate.rs`：目标出口修复与客户端记录隔离。
- `src/checker.rs`：将传入 API 客户端类型改为 CoreApi，保留选择算法。
- Cargo.toml/lock、README、CHANGELOG、bin：依赖、文档与产物。

### Task 1: 客户端类型与默认配置

**Files:** Create `src/client_config.rs`; Modify `src/main.rs` module declarations.

**Interfaces:**
- `ClientKind::{CuteCloud, ClashVerge}`；`ClientSelection::{Explicit(ClientKind), Auto}`。
- `ClientSelection::parse(value: &str) -> Result<Self, String>`；规范名称输出 `cutecloud`、`clash-verge`、`auto`。
- `config_path() -> Result<PathBuf, String>`。
- `load_default(path: &Path) -> Result<Option<ClientSelection>, String>`。
- `save_default(path: &Path, client: Option<ClientSelection>) -> Result<(), String>`；None 删除 default_client。
- `show_config(path: &Path) -> Result<String, String>`；显示保存值、兼容默认值及路径。

- [ ] 写测试 `config_round_trip_preserves_unknown_fields`：写入 clash-verge 后载入相同值；未知字段保留；unset 后 None。
- [ ] 写测试 `config_rejects_invalid_data_without_overwrite`：无效 JSON、schema_version=2、default_client=unknown 均报错，原文件字节不变。
- [ ] 写测试 `client_names_are_strict_and_case_insensitive`：CLASH-VERGE 有效，verge/unknown 报错，auto 与具体类型区分。
- [ ] 注册模块，运行 `cargo test client_config --offline`，确认未实现接口导致测试失败。
- [ ] 实现上述接口，JSON 文档保留未知字段；同目录唯一临时文件，Windows 文件替换使用原子替换机制，失败清理自己的临时文件并保留原文件。
- [ ] 运行 `cargo test client_config --offline`，要求全部通过；提交本任务。

### Task 2: 统一内核 API 与管道传输

**Files:** Create `src/core_api.rs`; Modify `Cargo.toml`, `Cargo.lock`, `src/mihomo.rs`, `src/detect.rs`, `src/checker.rs`, `src/main.rs`。

**Interfaces:**
- `ApiEndpoint::{Http(String), NamedPipe(PathBuf)}`，实现 Display 与从 HTTP String 的转换。
- `CoreResponse { status: u16, body: Vec<u8> }`。
- `CoreApi::new() -> Result<CoreApi, String>`。
- `CoreApi::request(&self, be: &Backend, method: reqwest::Method, path: &str, body: Option<Vec<u8>>, timeout: Duration) -> Result<CoreResponse, String>`。
- Backend.api 类型改为 ApiEndpoint；新增 Backend.kind: Option<ClientKind>；现有测试构造 Backend 同步迁移。
- mihomo 现有方法的 agent 参数改为 `&CoreApi`；新增 `get_runtime_proxy(be: &Backend, api: &CoreApi) -> Result<String, String>`。

- [ ] 写临时管道测试 `named_pipe_handles_chunked_authenticated_response`：读取 Bearer 请求，返回分块 JSON，结果体完整且无块长度残留。
- [ ] 写测试 `named_pipe_reports_truncation_timeout_and_http_error`：Content-Length 截断、停滞、403 不成功；204 空响应成功；错误消息不含测试 secret。
- [ ] 写测试 `named_pipe_requests_remain_concurrent`：多个并发请求在固定线程队列下均完成，没有单个共享 runtime 锁串行化。
- [ ] 运行 `cargo test core_api --offline`，确认测试先失败。
- [ ] 添加直接依赖 hyper 1（client/http1）、hyper-util（tokio）、bytes 1、serde_yaml_ng 0.10；Tokio 增加 io-util 测试支持。若 YAML 新依赖未缓存，执行一次正常 Cargo 获取后再 locked/offline 验证。
- [ ] HTTP 分支沿用 reqwest.no_proxy() 和禁止重定向；管道使用 Tokio ClientOptions + TokioIo + hyper HTTP/1 handshake，连接/请求/完整响应体读取包含在整体超时内。对管道忙进行有界等待；不输出请求头或响应中的私密数据。
- [ ] `get_runtime_proxy` 从 /configs 选择 mixed-port、port、socks-port，分别生成 http://127.0.0.1 或 socks5h://127.0.0.1；均为 0 则报错。
- [ ] 迁移现有 API 调用，运行 `cargo test --locked`，保留现有鉴权、重定向、并发等测试；提交本任务。

### Task 3: 本地发现、扫描与选择策略

**Files:** Create `src/clients.rs`; Modify `src/detect.rs`, `src/procinfo.rs`。

**Interfaces:**
- `DiscoveryPaths { cutecloud: PathBuf, clash_verge: PathBuf }`；生产环境以 APPDATA 初始化，测试显式传入临时目录。
- `DiscoveryRecord { name: String, backend: Option<Backend>, error: Option<String> }`。
- `discover(api: &CoreApi, paths: &DiscoveryPaths) -> Vec<DiscoveryRecord>`。
- `select_auto(records: &[DiscoveryRecord], enabled_system_proxy: Option<&str>) -> Result<Backend, String>`。
- `resolve_backend(args: &Args, api: &CoreApi, paths: &DiscoveryPaths) -> Result<Backend, String>`。

- [ ] 写测试 `verge_runtime_pipe_and_proxy_override_stale_tcp`：运行 YAML pipe 有效、配置 TCP 9097 未监听；选择管道及真实 /configs 7897，忽略陈旧 config.yaml TCP 值。
- [ ] 写测试 `auto_selects_unique_active_proxy_or_reports_ambiguity`：两候选中系统出口7897选择 ClashVerge；禁用系统代理且两候选则报错；只有一个可用则选它。
- [ ] 写测试 `explicit_client_and_api_never_cross_clients`：ClashVerge 失败不能选 CuteCloud；本机身份冲突、远程 API 配合具体本机 --client 报错；未知远端 kind=None。
- [ ] 写测试 `explicit_target_ignores_broken_default_config`：明确 --client/--api/--detect 时，不读取坏默认配置。
- [ ] 写测试 `discovery_deduplicates_instances_and_reports_unreadable_config`：同一内核的管道/TCP 去重；单个配置权限或解析错误不吞掉其他候选。
- [ ] 运行 `cargo test clients --offline --locked`，确认先失败。
- [ ] 用 serde_yaml_ng 定向反序列化必要控制字段，优先 Verge 运行配置，运行配置不可读时再报告可用后备；不得打印 YAML 片段、secret 或订阅。保留常见端口扫描和进程核实。
- [ ] 使用 /version、/configs 验证候选，填充 kind、版本、endpoint、proxy、来源；执行已定义选择优先级，--secret/--proxy 覆盖目标值。
- [ ] 运行 `cargo test clients --offline --locked` 与现有 procinfo 测试，全部通过；提交本任务。

### Task 4: 命令入口、支持类型列表与 watch 重连

**Files:** Modify `src/main.rs`, `src/detect.rs`; Test CLI through isolated mock API/proxy fixtures。

**Interfaces:**
- Args.client: Option<ClientSelection>。
- `ConfigCommand::{Show, SetDefault(ClientSelection), UnsetDefault}`。
- `Cmd::Clients { supported: bool }`、`Cmd::Config(ConfigCommand)`。
- `parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Args, String>`；主入口包装 std::env::args().skip(1)。
- `print_supported_clients()`；静态显示 cutecloud/clash-verge 的传输、节点操作及记录同步能力，auto 单独说明。

- [ ] 写参数测试：`check --client clash-verge`、`config set default-client auto`、`config unset default-client`、`clients --supported` 分发正确；缺值和未知值报错。
- [ ] 写 `supported_list_works_without_appdata_or_kernel`：静态列表不调用发现或读取配置；config 同样不 setup 内核。
- [ ] 写 `watch_reconnects_changed_pipe_of_same_client`：模拟同客户端运行配置从管道A切到B；下一轮重连B；另一客户端运行也不改目标。
- [ ] 运行相关测试确认失败，然后接入 Task 1/3 接口。
- [ ] 修改 check：缺少策略组时明确报错并显示可选组；watch 在重连时固定已选 kind 或显式 API，刷新该目标配置/secret/运行端口，不跨客户端回退。
- [ ] 显示客户端、选择来源、传输地址、代理出口及版本；不打印 secret。clients 列出错误候选，退出0代表至少有可用候选、1代表扫描无可用目标；supported/config 成功退出0。
- [ ] 运行完整测试及模拟 CLI 验证；确保原 watch 三种行为仍通过；提交本任务。

### Task 5: 系统代理绑定、记录隔离、文档与本机验收

**Files:** Modify `src/sysproxy.rs`, `src/appstate.rs`, `src/main.rs`, `src/checker.rs`, `README.md`, `CHANGELOG.md`。

**Interfaces:**
- `sysproxy::matches(proxy: &str) -> bool`：启用且出口一致。
- `sysproxy::enable_for(proxy: &str) -> Result<(), String>`；底层读写帮助函数接收测试注册表路径。
- `appstate::sync_selection(be: &Backend, group: &str, node: &str)`：只有 kind=CuteCloud 才更新 CuteCloud 数据库；ClashVerge 输出内核已生效和持久化能力说明；未知目标不写客户端记录。

- [ ] 写 `proxy_repair_targets_selected_client_and_preserves_bypass`：隔离注册表原7890改7897，ProxyOverride完全保留；dry-run无变化；SOCKS-only报不支持系统HTTP出口且不写入。
- [ ] 写 `selection_sync_isolated_by_client_kind`：Verge/未知目标不打开 CuteCloud 数据库，CuteCloud更新正确组；测试使用临时数据库路径。
- [ ] 运行上述测试确认失败，再实现新接口并替换 check/fix/watch 和最终选择同步调用。
- [ ] 更新 README/帮助/CHANGELOG，覆盖 clients --supported、持久默认值、自动歧义、--group、--api覆盖及 Verge 持久化边界。
- [ ] `cargo test --offline --locked` 全部通过；`git diff --check` 无错误。
- [ ] 本机只读执行 clients、clients --supported、check --client clash-verge --dry-run；读取真实 /proxies 策略组后执行 scan --client clash-verge --group 实际组，确认代理出口7897和候选列表。只读验证不改变当前节点或系统设置。
- [ ] use/fix/watch 的切换路径通过模拟管道/代理验证；本机真实切换需在用户明确要求时记录原节点并验证恢复。当前“代理正常工作”不作为随意切换真实节点的要求。
- [ ] 执行 `build.bat` 更新 bin/proxpilot.exe；用最终 exe 验证帮助和支持类型列表；提交本任务。推送按用户指令执行。

## 自检与执行交接

已自检：设计的三种入口、优先级、原参数兼容、管道传输、端口绑定、watch、状态隔离、静态支持列表及错误处理均有对应任务；五项 Review Focus 均有测试归属。

当前本机只运行 Clash Verge，系统代理已启用并指向127.0.0.1:7897，可以进行上述只读验收。工作区当前是普通 main checkout；执行前选择是否使用隔离工作区，保留已修订设计文档。

推荐 Native：五个任务共享 Backend/CoreApi 接口，在本会话依次实现更易保持接口一致；完成后进行一次独立整体审查。用户审阅计划并选择执行方式后开始产品代码修改。

## 技术资料

- https://docs.rs/serde_yaml_ng/0.10.0/serde_yaml_ng/
- https://docs.rs/hyper/1.12.0/hyper/client/conn/http1/
- https://docs.rs/tokio/1.53.2/tokio/net/windows/named_pipe/
