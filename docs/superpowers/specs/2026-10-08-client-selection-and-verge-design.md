# 客户端选择、默认配置与 Clash Verge 适配

日期：2026-10-08
状态：待用户审阅；产品代码尚未修改。

## 目标

让 ProxPilot 支持三种客户端选择入口：命令行指定目标、通过命令行保存默认目标、自动扫描并选择运行中的客户端。CuteCloud 与 Clash Verge 同时运行时，控制 API、网站实测代理及选择记录必须属于同一个目标客户端。

用户已确认增加参数指定客户端，并要求默认客户端可通过命令行配置，同时支持自动探测和扫描。本设计扩展已有 check、scan、fix、use、watch 流程，保留默认 watch 的故障切换行为及 --optimize 主动优选行为。

## 已核实的本机情况

- Clash Verge 安装于 C:\Program Files\Clash Verge，版本 2.5.7；内核为 verge-mihomo.exe。
- 运行配置为 %APPDATA%\io.github.clash-verge-rev.clash-verge-rev\clash-verge.yaml。
- 内核通过 Windows 命名管道提供 HTTP API；本机 TCP 外部控制器未启用。
- 管道 /version、/configs 已返回 HTTP 200，内核版本 v1.19.32，mixed-port 为 7897。
- 管道 /proxies 返回 HTTP 200；探查脚本未处理分块响应，不能将其解析失败误判为内核不可用。
- CuteCloud 同时运行，API 9090、代理端口 7890。
- 现有 API 基于 reqwest HTTP；默认目标写死 CuteCloud；代理出口取系统注册表，可能与目标内核不一致。
- 现有选择同步不判断目标客户端，会始终尝试写 CuteCloud 数据库，必须修正。

## 用户命令

客户端标识第一期支持 cutecloud、verge、auto；输入不区分大小写，保存与输出使用规范标识。未知标识报错，不静默回退。

```powershell
# 本次明确指定
proxpilot check --client verge
proxpilot scan --client verge --group "策略组名称"
proxpilot watch --client verge --group "策略组名称"

# 保存、查看和清除默认值
proxpilot config set default-client verge
proxpilot config show
proxpilot config unset default-client

# 将自动模式设为默认
proxpilot config set default-client auto

# 本次自动选择；--detect 保留为兼容入口
proxpilot check --client auto
proxpilot check --detect

# 列出客户端，不切换节点、不调整系统代理
proxpilot clients
```

config 命令只管理 ProxPilot 自身配置，不连接内核、不改变 CuteCloud/Verge 的设置。clients 是客户端扫描命令；scan 继续表示目标策略组内的节点扫描。

## 选择优先级与兼容

普通客户端选择优先级：显式 --client > --detect 自动模式 > 保存的 default-client > CuteCloud 兼容默认值。

- --client verge 与 --detect 同时给出时，显式 verge 生效，不扩大成自动选择。
- 默认值 cutecloud/verge 是严格选择；目标不运行或不可连接时，给出明确错误，不换用另一客户端。
- --api 保留为直接指定 HTTP API 的入口，优先于默认目标和自动模式。若同时明确 --client，必须核实其与 API 监听进程相符；不符则报错。无法识别的远程 API 配合具体本机客户端标识时也报错，避免错误读取本机配置和选择记录。
- --client auto 与 --api 同时给出时以显式 API 为连接目标，并标明识别结果。
- --secret 优先于目标客户端本地运行配置中的鉴权信息。
- --proxy 优先于目标内核运行配置中的代理端口，输出明确标识“参数指定”。
- 未指定 --group 时仍使用现有 AI服务。组缺失时报告错误并列出可选组名，不擅自选择或创建策略组。

## 配置存储

使用 %APPDATA%\ProxPilot\config.json：

```json
{
  "schema_version": 1,
  "default_client": "verge"
}
```

- 没有配置文件时按兼容默认值运行。
- config show 显示文件路径、保存值和实际默认值；未保存时明确说明使用 CuteCloud 兼容默认值。
- 保存采用同目录临时文件加替换，保留未知 JSON 字段，避免将来扩展时丢配置。
- unset 仅移除 default_client 字段。
- 无效 JSON、无效客户端值、不支持的版本及读写失败给出清晰错误，不覆盖损坏文件。
- 显式 --client/--api/--detect 可绕过默认配置解析；config show 仍报告文件错误。默认配置损坏不应阻止显式连接已知目标。
- 不保存订阅、节点凭据或 API secret；客户端 secret 只在请求时读取，不出现在日志中。

## 自动扫描与选择

1. 从已知配置位置读取候选 API 地址、命名管道地址及进程身份，查询配置时不输出密钥或订阅。
2. 用 /version 验证候选是否确为可连接内核；读取 /configs 获取真实代理端口。
3. 保留现有常见 TCP 端口扫描，补充 Verge 运行配置的管道入口，按实际监听进程去重。配置中遗留的 TCP 9097 不等于实际可用接口。
4. clients 列出客户端名称、运行状态、API 类型/地址、代理出口和是否匹配系统代理；不可用候选标出鉴权失败、连接失败或配置不可读，不阻止显示其他候选。
5. auto 模式优先选择唯一匹配已启用系统代理地址和端口的候选；否则，若仅一个候选可用则选它。
6. 多个候选可用且不能唯一匹配时，列出候选并要求 --client 明确选择；不悄悄固定偏好某个客户端。
7. 都不可用时报告各候选原因，不启动客户端，不改客户端配置。

第一期具体适配 cutecloud/verge。其他 mihomo 客户端仍可通过 --api、--proxy 手动连接；clients 对常见端口的其他内核显示识别结果，但不将其保存为未实现的具体客户端标识。

## API 与命名管道传输

引入统一内核 API 客户端，连接目标支持 Http URL 和 Windows NamedPipe。mihomo 业务方法只依赖这个客户端；网站实测继续使用现有 BrowserProbe。

- HTTP 路径沿用 reqwest 的直连、鉴权、状态检查与禁止重定向行为。
- 管道路径使用 Rust/Tokio Windows named-pipe 能力，并通过成熟 HTTP/1 库处理请求和响应，不自行假设一次 read 就得到完整 JSON。
- 支持 GET /version、GET /configs、GET /proxies、GET /proxies/{node}/delay、PUT /proxies/{group}。
- 正确处理 Content-Length、chunked、204 空响应、完整响应体读取以及错误状态；节点和组名继续支持中文及 emoji。
- 为管道连接、写入、读取和整体操作设置超时，断开/截断响应必须返回错误。
- 延迟探测可并发发起独立管道请求；单次请求失败不阻塞其他工作线程。
- API 错误信息不携带 Authorization 或 secret。
- 每次 API 操作建立连接，读取最新运行配置可恢复变化后的连接目标；watch 的目标客户端固定，不因另一软件启动而自动改目标。若 Verge 重启导致管道地址变化，下一轮重读该客户端配置并重连同一客户端。

## 代理出口和系统代理

- 优先使用用户 --proxy；否则从已验证目标内核 /configs 取得 mixed-port，其次 HTTP port，再次 SOCKS port。
- 没有可用端口时明确失败，不借用另一个客户端的注册表端口做实测。
- 本地代理出口连接到 loopback；混合/HTTP 端口使用 http://，仅 SOCKS 端口时使用 socks5h://。
- scan、clients 和 check --dry-run 不调整系统代理。
- check、fix、watch 如需修复系统代理，必须将 ProxyServer 与目标出口保持一致，并保留原 ProxyOverride；不能只打开开关却留下另一个客户端的地址。
- 显式 SOCKS-only 出口可用于程序实测，但不写入 Windows HTTP 系统代理；输出系统代理修复不支持该出口的说明。
- 不启停 CuteCloud/Verge，不修改 TUN 或规则模式。

## 客户端选择记录

- 将同步入口改为接收目标客户端身份，CuteCloud 数据库仅在目标确为 CuteCloud 时更新。
- Verge 经管道切换内核并读取 /proxies 确认结果；GUI 显示与重启持久化分别验证，不能将内核成功等同于客户端记录成功。
- 已查到 Verge 的 profiles.yaml/current/items/selected 及 GUI 内存配置机制。外部直接改 profiles.yaml 不能保证 GUI 内存刷新或避免其覆盖磁盘，因此本期不对运行中的该文件执行直接写入。
- 第一阶段明确报告 Verge 节点切换已在内核生效，客户端重启后的恢复由其自身记录决定，不宣称已同步永久选择记忆。
- 如果后续验证发现稳定且可外部调用的 GUI 同步接口，再将该能力作为独立增量适配；本期验收不包含未经证明的重启持久化承诺。

## 模块划分

- client_config.rs：ProxPilot 默认配置读取和 config 子命令。
- clients.rs（或现有 detect.rs 内清晰分层）：客户端标识、配置发现、候选扫描、选择策略。
- core_api.rs：统一 HTTP/命名管道传输、鉴权、超时。
- mihomo.rs：节点/组业务 API；新增读取 /configs。
- procinfo.rs：现有 Windows 进程识别，补齐 Verge 识别用例。
- appstate.rs：按客户端选择同步能力，避免跨客户端写入。
- sysproxy.rs：修复时写入正确出口并保留例外。
- main.rs：客户端参数、config/clients 分发、清晰输出。

## 验收与测试

1. 配置测试：set/show/unset、默认回退、无效配置、不丢未知字段；测试目录不得使用真实 APPDATA 配置。
2. 选择测试：参数/--detect/保存值优先级、显式目标失败不回退、两客户端同时运行的唯一匹配与歧义处理。
3. API 测试：HTTP 和真实临时 Windows 管道的鉴权、完整/分块/截断响应、超时、空响应、非成功状态，以及中文节点切换请求。
4. 端口测试：目标 Verge 7897 与系统 CuteCloud 7890 不一致时，实测仍选择 7897；覆盖 --proxy 覆盖、HTTP/SOCKS 回退。
5. 状态隔离测试：目标 Verge 时不访问或写入 CuteCloud 数据库。
6. 系统代理修复逻辑用隔离注册表位置或抽象替身测试，不改测试机真实设置。
7. 保留并通过现有 scan 并发、访问指纹和 watch 测试。
8. 本机真实验证先运行 clients、check --dry-run 与用户选定组的 scan，核对管道、端口、节点读取和实测；验证 use/fix/watch 时明确记录测试前后目标节点，并可恢复原节点。
9. 更新 README、帮助及 CHANGELOG，构建并更新 bin/proxpilot.exe。

## 来源

- 本项目 src/main.rs、detect.rs、mihomo.rs、procinfo.rs、appstate.rs、sysproxy.rs。
- 本机运行进程、Clash Verge 运行配置，以及已执行的只读管道 API 查询。
- Clash Verge Rev v2.5.7 profiles 实现：https://github.com/clash-verge-rev/clash-verge-rev/blob/v2.5.7/src-tauri/src/config/profiles.rs
- Clash Verge 配置结构：https://github.com/clash-verge-rev/clash-verge-rev/blob/main/src-tauri/src/config/verge.rs

## 审阅结果

自检：三种入口及其优先级、自动选择歧义、控制/出口绑定、密钥处理、Verge 状态持久化边界、watch 重连与测试隔离均已定义。需要用户审阅本设计后再编写实施计划并选择执行方式。
