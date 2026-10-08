# 更新日志

## 未发布

- watch 默认保持当前可用节点，仅在故障复测确认后优选；新增 `--optimize`，开启后才在启动时及每隔 `--reopt` 主动优选。
- scan 输出用青色加粗和 `[低延迟]` 标签标识探测延迟 ≤ 200ms 的节点，不受排名或 `--top` 数量限制；用黄色标签标识当前节点，并附上标识说明。重定向输出保留文字标签。
- scan/fix 的节点探测并发数改为系统可用逻辑 CPU 核数的两倍（不超过节点数），使用固定工作线程动态领取节点任务；修正原先按每组 8 个节点分配线程导致并发数不固定的问题。

## v0.2.2（2026-10-07）

- 网站实测使用 `wreq + BoringSSL`，默认模拟 Windows Chrome 149 的 TLS、HTTP/2 和请求头指纹；遇到 Cloudflare 明确标记的 403 Challenge 时尝试 macOS Safari 26，成功后在同一次节点验证中优先使用该配置。两个配置的 Cookie 独立，每个采样整体超时 12 秒。内核 API 使用 `reqwest + rustls`。移除 curl 与 ureq，支持压缩、重定向和 Cookie，每次请求使用新连接并读取完整响应体计时。
- 端口与进程识别改用 `windows-sys`，移除 netstat/tasklist，运行时不再启动外部程序。
- 内核 API 明确直连，并为版本读取补齐 secret 鉴权；IP 检查提示不再保证浏览器可访问。
- 增加本地代理、响应传输、连接隔离、TLS ClientHello GREASE/ALPN 与 Windows IPv4/IPv6 监听识别测试。
- 构建要求 Rust 1.98+ 和 BoringSSL 构建工具（CMake、NASM、libclang）；工具仅在编译时使用。

## v0.2.1（2026-10-07）

### 新增
- **一键发版脚本 `release.bat`**：自动读取版本号、编译、提交、打标签、推送并创建 GitHub Release（附上带版本号的 exe），发版从一系列手工步骤变成一条命令。

## v0.2.0（2026-10-07）

### 新增
- **客户端自动识别**：启动时探测本机运行的 Clash/mihomo 系客户端（CuteCloud / FlClash / Clash Verge / Clash for Windows / mihomo），输出标明目标客户端；多客户端同时在线默认优先 CuteCloud，可用 `--api` 指定。
- **实时进度指示**：客户端探测、节点并发探测、真实访问验证全程显示旋转符 + 当前步骤 + 已耗时 + 完成计数；输出重定向时自动降级为静态日志（`PROXPILOT_SPINNER=1` 强制开启）。
- **切换后同步客户端选择记忆**：切换内核节点的同时更新 CuteCloud 的 `database.sqlite` 选择记录，界面显示与内核实际状态不再脱节，也不会再被旧选择反向覆盖。
- **一键修复脚本 build.bat**：编译并输出到 `bin\proxpilot.exe`。

### 修复 / 改进
- **双信号访问判定**：`chatgpt.com` 返回 403 时用 `api.openai.com` 交叉验证 IP 信誉（401 = IP 正常，浏览器可访问），消除"实际能访问却报 403"的误判。
- **防 UDP 抖动节点误判**：IP 检查通过后复测隧道稳定性，任何一次断连即判失败；"仅 IP 通过"的节点在优选排序中永远排在实测 200 的节点之后。
- 结果输出整行着色（可访问绿 / 失败红 / 警告黄）。
- 系统代理被其他软件关闭时自动修复（check / fix / watch 均支持）。

### 其他
- exe 静态链接 CRT 并内置 SQLite，单文件免安装，Windows 10/11 直接运行。
