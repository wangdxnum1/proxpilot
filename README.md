# ProxPilot · 代理领航员

> 节点检测 · 优选切换 · 守护运行
> 支持 Clash / mihomo 系客户端（CuteCloud、VVCloud、FlClash、Clash Verge、Clash for Windows 等）

ProxPilot 是一个 Windows 命令行工具，用于解决 Clash 系代理客户端的常见痛点：

- **节点列表是绿的，ChatGPT 却打不开** —— 测速只证明隧道通，不代表节点 IP 没被 OpenAI/Cloudflare 风控；
- **节点中途挂掉**，策略组是固定选择，不会自动切换；
- **多个代理客户端同时存在**，系统代理经常被别的软件退出时关掉。

ProxPilot 直接与本机运行的内核 API 通信：并发探测所有节点 → 逐个真实访问验证 → 自动切换到实测最快的节点，全程彩色输出、无需打开浏览器。

运行过程有**实时进度指示**（旋转符 + 当前步骤 + 已耗时 + 完成计数，如 `⠹ 并发探测节点... 已测 17/53 (8s)`）；输出被重定向到文件/管道时自动降级为静态日志，设 `PROXPILOT_SPINNER=1` 可强制开启。

---

## 编译

构建需要 Rust 1.98+、MSVC C++ 工具链、CMake 3.22+、NASM 和 libclang（Visual Studio 的 LLVM 组件或独立 LLVM）。`build.bat` 会通过 `build-env.bat` 查找 libclang，也支持 `target/build-tools/nasm-2.16.03/nasm.exe` 中的便携 NASM。独立 LLVM 可通过 `LIBCLANG_PATH` 指定 DLL 所在目录。这些工具只用于构建；BoringSSL、SQLite 和 CRT 静态链接进 exe，使用者无需安装。

```
build.bat
```

脚本会执行 `cargo build --release`，并把产物拷贝到 **`bin\proxpilot.exe`**（兼容自定义 target triple 的产物路径）。 exe 为静态链接 CRT 的单文件，拷到任何 Windows 10/11 机器可直接运行，无需 VC++ 运行库。

手动编译：`cargo build --release`（项目级 `.cargo/config.toml` 已配置 crt-static）。

## 快速开始

```
proxpilot check     :: 体检：内核通不通、系统代理是否被关、ChatGPT 能否访问
proxpilot fix       :: 一键优选：探测全部节点 → 真实验证 → 切到实测最快的
proxpilot watch     :: 守护模式：保持可用节点，坏了自动修
```

**典型场景**：ChatGPT 突然打不开 → 双击 `bin\proxpilot.exe` 或运行 `proxpilot fix`，约 1 分钟后自动切到可用节点。

---

## 客户端选择与配置

内置适配 `cutecloud`、`clash-verge`、`vvcloud`，`auto` 是自动选择模式。Clash Verge 优先读取运行配置中的命名管道，支持带 secret 的控制接口。VVCloud 没有明文运行配置时，校验进程身份后探测常见控制端口；自定义端口可用 `--api` 指定。托管订阅可能加密，节点详情仅使用可读的明文配置和内核信息，缺失字段保持未知。其他兼容内核可通过 `--api` 指定 HTTP 接口。

```powershell
proxpilot clients --supported
proxpilot clients
proxpilot check --client clash-verge --group VVCloud --dry-run
proxpilot scan --client clash-verge --group VVCloud
proxpilot check --client vvcloud --dry-run
proxpilot config set default-client vvcloud
proxpilot config set default-client clash-verge
proxpilot config show
proxpilot config set default-client auto
proxpilot config unset default-client
```

选择顺序：`--api` → `--client` → `--detect` → 保存的默认客户端 → `cutecloud`。`--detect` 等同于自动选择；优先匹配已开启的系统代理，否则只在唯一内核可用时选择，多个可用内核必须明确指定。显式参数可以绕过损坏的默认配置。配置保存在 `%APPDATA%\ProxPilot\config.json`，不保存 secret。

`--secret` 覆盖目标客户端运行配置中的凭据；`--proxy` 覆盖目标内核提供的 mixed/HTTP/SOCKS 端口。修复系统代理时同时校正目标出口并保留绕过规则，SOCKS 出口不能作为 Windows HTTP 系统代理。策略组不设固定默认值，也不保存默认组。未传 `--group` 时实时读取 `/proxies`，忽略合成的 `GLOBAL` 总览，选择唯一未被其他业务组引用的顶层组；多个顶层组或循环引用时列出可选组，要求 `--group` 显式指定。仅有 `GLOBAL` 时使用它。`watch` 每轮重新读取，显式指定的组始终固定。

Clash Verge 的节点切换作用于当前内核；不修改由 GUI 缓存管理的 `profiles.yaml`，重启后的持久化由 Clash Verge 管理。CuteCloud 和 VVCloud 会按客户端隔离，同步已记录该策略组的 SQLite 选择记忆；客户端重启后的行为仍由其自身管理。`watch` 固定启动时选定的客户端，并在每个周期重新读取该客户端运行配置，以适应重启后命名管道变化。

`clients`、`scan`、`config show` 及 `check --dry-run` 不改变系统代理或当前节点。`use/fix/watch --dry-run` 也不切换节点。

## 子命令详解

### `check` — 体检

依次检查并彩色输出：

1. 内核代理端口是否可用；
2. Windows 系统代理是否开启（被其他代理软件关闭时自动重新打开，可用 `--dry-run` 跳过）；
3. 当前策略组选中的节点；
4. 测试网址的实际可访问性（`--samples` 次全部 HTTP 200 才算通过）。

退出码：`0` 一切正常；`1` 环境问题（内核未运行等）；`2` 测试网址不可访问。

### `nodes` / `info <节点名>` — 节点信息

```powershell
proxpilot nodes --client auto
proxpilot nodes --client auto --max-rate 1
proxpilot info "D美国2-网页-视频浏览-1倍率" --client auto
```

`nodes` 列出实时策略组中的真实节点、内核协议、名称倍率与名称标签，不测速、不切换。`info` 可按准确名称查看全局真实节点，不要求自动选出唯一策略组；显式 `--group` 时验证归属。

详情从目标客户端运行配置及其目录内的已下载订阅缓存读取入口服务器、端口、显式传输方式、TLS、UDP、加密算法和 TLS 服务名；显示配置文件来源。密码、UUID、secret、订阅 URL 不显示。显式 `--api` 或未知客户端只使用内核提供的信息；缺失字段保持未知，入口地址不等于最终出口 IP。专线/住宅/用途/倍率标签只来自名称，不能证明实际线路或网站可用性。

### `--max-rate <N>` — 限制计费倍率

```powershell
proxpilot scan --client auto --max-rate 1
proxpilot fix --client auto --max-rate 1
proxpilot watch --client auto --max-rate 1
proxpilot watch --client auto --max-rate 1 --optimize
```

支持有限正数，例如 `1`、`0.5`。只考虑名称中明确标注且不超过上限的倍率；未知、冲突倍率排除并计数，不当作一倍率。筛选在探测和切换之前进行，`scan/fix/watch/nodes/use` 共用同一规则，候选不足时不放宽限制。失败后恢复原节点属于回滚，原节点可能不符合新限制。

默认 `watch` 仍保持可用节点；当前节点超限或倍率未知时提示，下次故障修复或开启 `--optimize` 的优选才选择合规候选。`use` 显式指定超限/未知节点也会拒绝。占位条目（续费网址、订阅地址、剩余流量等）不作为真实节点参与探测或优选。

### `scan` — 节点探测

对组内全部真实节点做**可达性探测**（并发数为系统可用逻辑 CPU 核数的两倍，最多不超过节点数；让内核拨号每个节点请求测试网址），列出可达清单及延迟。

注意：探测阶段**任何 HTTP 响应都算可达**（包括 403），所以"可达"≠"能打开网页"。探测快只是初筛，IP 是否被风控必须用 `fix` 实测验证。

结果按探测延迟排序，最多显示 30 个节点。探测延迟 ≤ 200ms 的节点用青色加粗并标注 `[低延迟]`，不受排名或 `--top` 数量限制；当前选中的节点另加黄色 `[当前]` 标签。名称明确标注 `1倍率`、`1.0倍率` 或 `一倍率` 的节点另加绿色 `[1倍率·省流量]` 标签，并在独立清单中完整列出，不受前 30 条限制。倍率依据名称识别。低延迟仅代表初筛速度，实际可访问性仍需 `fix` 验证。重定向输出时保留文字标签。

退出码：`0` 有可达节点；`2` 全部不可达；`1` 环境问题（API 读取失败等）。

### `fix` — 优选并切换（核心）

完整流程：

1. 检查内核与系统代理（异常自动修复）；
2. 读取组内节点，过滤策略组嵌套项与机场假节点（"剩余流量"、"套餐到期"等占位条目）；
3. 按最近测速延迟排序，以系统可用逻辑 CPU 核数的两倍并发探测可达性；
4. 取探测最快的 `--top` 个，**逐个切换并实测**：每个节点发起 `--samples` 次真实 HTTPS 访问，全部 200 才算通过；chatgpt.com 遇 403 时用 `api.openai.com` 交叉验证 IP 并复测隧道稳定性（防 UDP 抖动节点误判）；最多取 5 个通过者，**真实 200 的节点优先于仅 IP 检查通过的**，各自按实测延迟排序；
5. 停在实测最快的节点上，并做最终确认；
6. 若全部候选失败，恢复切换前的原节点并明确提示（多半是整批 IP 被风控）。

退出码：`0` 已切换或确认最优；`1` 环境问题；`2` 没有节点通过验证。

### `use <节点名>` — 手动切换

切换到指定节点并验证。节点名含空格或 emoji 时加引号：

```
proxpilot use "🇭🇰 香港 A2 | 中转"
```

退出码：`0` 切换且验证通过；`2` 切换成功但验证未通过；`1` 切换失败。

### `watch` — 守护模式

循环运行（Ctrl+C 退出），每个 `--interval` 周期：

- 内核未运行 → 跳过等待；系统代理被关 → 自动打开；
- 测试网址异常 → 5 秒后复测确认（避免 Cloudflare 偶发误报），确认失败后自动执行完整 `fix` 流程；
- 默认保持当前可用节点，不在启动时或定时主动优选；即使设置 `--reopt` 也不会开启主动优选。
- 加 `--optimize` 后，启动时执行优选，之后距上次优选结束达到 `--reopt` 秒时重新优选（默认 7200 秒）。优选会逐个切换候选实测，最后停在本轮候选中验证最优的节点；不设置相对原节点的提速门槛。

适合开机自启动：

```
proxpilot watch --interval 180
```

需要启动时及定时主动选择最优节点：

```
proxpilot watch --optimize --interval 180 --reopt 7200
```

---

## 全部参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--max-rate <N>` | 不限制 | 节点名称中的倍率上限，未知倍率排除；支持小数 |
| `--group <名称>` | 实时识别 | 显式指定目标策略组；省略时选择唯一顶层业务组，歧义时列出可选组 |
| `--url <地址>` | `https://chatgpt.com/` | 测试网址。优选流媒体可配 `--url https://www.youtube.com/`；只测连通性可用 `https://www.gstatic.com/generate_204` |
| `--client <类型>` | 保存值或 cutecloud | 明确指定 cutecloud、clash-verge、vvcloud 或 auto |
| `--api <地址>` | 目标运行配置 | 内核 external-controller 地址，如 `http://127.0.0.1:9097`。多客户端在线时用它指定目标 |
| `--secret <值>` | 无 | 内核 API 的鉴权密钥（客户端设置了 `secret` 时必填） |
| `--proxy <地址>` | 目标内核端口 | 实测流量走的代理出口，按 mixed / HTTP / SOCKS 优先级选择 |
| `--top <N>` | `12` | `fix` 中参与真实验证的候选数量（从探测最快的开始取） |
| `--samples <N>` | `2` | 每个节点实测次数。**全部 200 才算通过**，取平均延迟排序；加大可降低偶发误判 |
| `--interval <秒>` | `300` | `watch` 的检查周期 |
| `--optimize` | 关 | `watch` 启动时及定时主动优选；默认仅在当前节点故障确认后优选 |
| `--reopt <秒>` | `7200` | `watch --optimize` 的定时重新优选间隔；单独设置不启用主动优选 |
| `--dry-run` | 关 | 只探测报告、不切换不修复（`fix` 下等价于 `scan`；`check` 下不自动开系统代理） |
| `--detect` | 关 | 自动选择本机客户端，与 `--client auto` 等价；覆盖保存的默认值，但不覆盖显式 `--client` |

通用说明：所有涉及节点名/组名的参数都支持中文与 emoji，含空格时请加引号。

## 客户端识别

启动时输出客户端、运行配置来源、API、版本及代理出口。`clients` 读取两个适配客户端的运行配置，再扫描常见控制端口；只把 API 验证通过的内核作为可用候选，错误候选保留诊断。Clash Verge 的命名管道优先于陈旧的 TCP 配置。

自动选择优先匹配已开启的系统代理，否则要求唯一可用内核，不固定偏向 CuteCloud。通过进程信息校验本机接口身份，具体 `--client` 与显式 `--api` 身份不符时拒绝操作。

## 工作原理

```
机场订阅 → 客户端（CuteCloud 等）→ mihomo 内核 ← ProxPilot
                                      │
            GET /proxies              读节点与策略组
            GET /proxies/{节点}/delay  让内核经该节点请求测试网址（初筛）
            PUT /proxies/{组}          切换组内选中节点
```

- **初筛与实测是两回事**：延迟探测只要收到任何 HTTP 响应就算成功（403 也算）；真实验证使用 `wreq + BoringSSL` 经指定代理发起 HTTPS 请求，读取完整响应体并计时，两次 200 才算"能访问"。内核 API 保留 `reqwest + rustls`。
- **浏览器指纹模拟**：`wreq-util` 默认使用 Windows Chrome 149 配置，统一生成 UA、Client Hints、TLS ClientHello 和 HTTP/2 设置。仅当收到 HTTP 403 且 `cf-mitigated: challenge` 时，尝试 macOS Safari 26 配置；候补返回 200 后，同一次节点验证的后续采样优先使用该配置。两个配置的 Cookie 独立，每个采样最多尝试两个配置，整体超时 12 秒，计时包含候补请求。支持 gzip/Brotli/deflate/zstd 解压和最多 10 次重定向。每次请求建立新连接，避免切换节点后复用旧隧道。内核 API 请求明确直连，不受系统或环境代理影响。
- **双信号判定**：`chatgpt.com` 返回 403 时追加检查 `api.openai.com/v1/models`。401 被作为 IP 检查通过的备选信号，并复测隧道稳定性；403 或连接/传输失败判失败。仅 IP 通过的节点排在实测 200 节点之后。API 检查通过不能保证浏览器可访问，也不能证明网页 403 一定由指纹造成。
- **模拟范围**：可模拟 Chrome 的网络协议特征，但不会执行 JavaScript 或人机验证，不能保证消除 Cloudflare 403。网站实测验证 HTTPS 证书，使用内置 WebPKI 根证书；内核 API 的 rustls 使用系统证书验证。网站实测不会自动信任企业自行添加的根证书。
- **运行时依赖**：不调用 `curl.exe`、`netstat`、`tasklist` 或其他外部命令。HTTP/TLS、端口/进程识别、注册表和选择记录操作由 Rust 库完成；Windows API 与系统证书库仍由操作系统提供，代理内核仍需运行。SQLite 与 CRT 静态链接进 exe。构建需要 Rust/Cargo 与 MSVC 工具链；发版脚本还使用 Git 和 GitHub CLI（gh），这些不属于用户运行 exe 的依赖。
- 节点来源完全是你机场订阅在内核中的实时状态，ProxPilot 不保存、不解析订阅。

## FAQ

**Q：节点测速是绿的，为什么 ChatGPT 还是打不开？**
测速链接只证明隧道连通。ChatGPT 靠 IP 信誉拦截，机场共享 IP 常被 OpenAI/Cloudflare 风控（返回 403 或人机验证），此时测速照样绿。用 `fix` 换到实测 200 的节点即可。

**Q：check 显示 403，但浏览器明明能打开？**
HTTP 客户端和浏览器的 TLS 指纹、Cookie、JavaScript 执行能力不同，403 可能来自 Cloudflare 验证。ProxPilot 会用 `api.openai.com` 交叉检查，401 作为 IP 检查通过的备选信号，但浏览器访问仍需实际确认。

**Q：fix 提示"全部候选无法打开网页"？**
说明这批 IP 整体被风控了。换个 `--group`/地区再试、过几小时重跑，或问机场哪些节点支持 AI。

**Q：浏览器突然全部打不开，但 check 显示内核正常？**
多半是另一个代理软件退出时关闭了系统代理。`check`/`fix`/`watch` 都会自动重新打开。

**Q：切换后过一会儿又变回原节点？**
部分客户端（如 FlClash 系）会按自己的选择记录重新应用节点，外部切换可能被覆盖。重跑 fix 即可；`watch` 模式则会自动纠正。

**Q：想开机自动守护？**
把 `bin\proxpilot.exe watch --interval 180` 的快捷方式放入启动文件夹（`shell:startup`）。

## 目录结构

```
proxpilot/
├── build.bat            编译并输出到 bin\
├── bin\proxpilot.exe    编译产物（单文件、静态 CRT）
├── .cargo/config.toml   项目级构建配置（crt-static）
└── src/
    ├── main.rs          CLI 入口与子命令
    ├── detect.rs        客户端/后端自动探测
    ├── procinfo.rs      进程识别（端口 → PID → 客户端名）
    ├── mihomo.rs        Clash/mihomo API 客户端
    ├── http.rs          reqwest 内核客户端与 wreq 浏览器指纹实测
    ├── checker.rs       节点探测、实测验证、优选切换
    ├── appstate.rs      同步 CuteCloud / VVCloud 选择记录
    ├── sysproxy.rs      系统代理（注册表 + WinINET）
    └── ui.rs            彩色输出
```
