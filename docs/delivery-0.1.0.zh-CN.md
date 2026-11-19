# zuse v0.1.0 首发交付说明

交付日期：2026-11-20（Asia/Taipei）  
许可证：MIT  
首版源码提交：`dd57e38a7b0eafb11f438a539c7dbbac98b349a0`

## 发布结果

**zuse 已完成首版开发并正式开源发布。** 它同时提供 Rust 库、命令行可执行程序和 MCP 服务，面向 agent 的电脑观测、动作执行及人类协助流程。

| 入口 | 位置 |
| --- | --- |
| 源码 | [LIghtJUNction/zuse](https://github.com/LIghtJUNction/zuse) |
| 正式版本与五个平台下载包 | [GitHub Release v0.1.0](https://github.com/LIghtJUNction/zuse/releases/tag/v0.1.0) |
| Rust 包 | [zuse-computer 0.1.0](https://crates.io/crates/zuse-computer/0.1.0) |
| 中文说明 | [README.zh-CN.md](https://github.com/LIghtJUNction/zuse/blob/main/README.zh-CN.md) |
| 首次发布工作流 | [成功的发布运行](https://github.com/LIghtJUNction/zuse/actions/runs/44607816271) |
| 验证记录 | [docs/validation.md](https://github.com/LIghtJUNction/zuse/blob/v0.1.0/docs/validation.md) |
| 性能测量 | [docs/performance.md](https://github.com/LIghtJUNction/zuse/blob/main/docs/performance.md) |

crates.io 中的 `zuse` 名称已被其他项目使用，因此本项目发布名为 **zuse-computer**；安装后的命令仍是 **zuse**，Rust 库名也仍是 **zuse**。注册表已确认版本 0.1.0 可用且未被撤回。

GitHub Release 已包含以下构建，发布工作流全部成功：

| 系统 | 架构 | 文件 |
| --- | --- | --- |
| Linux | x86_64 | `zuse-v0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| Linux | ARM64 | `zuse-v0.1.0-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | Intel | `zuse-v0.1.0-x86_64-apple-darwin.tar.gz` |
| macOS | Apple Silicon | `zuse-v0.1.0-aarch64-apple-darwin.tar.gz` |
| Windows | x86_64 | `zuse-v0.1.0-x86_64-pc-windows-msvc.zip` |

## 工作方式与 Project Z 结构

agent 生成一段由具名操作组成的小程序。zuse 先解析并校验整批程序，再调用对应系统接口执行动作，最后返回结构化结果。校验错误在动作发生前返回；执行时遇到动态错误，会报告已经完成的步骤。

项目把解析和轨迹计算放在可以关闭标准库的核心中，把桌面、浏览器、Android、进程和人工协助放在主机运行时中。CLI、Rust API 与 MCP 共享执行器，避免三套入口产生不同语义。

- 关闭默认特性后，核心支持 `no_std`，不依赖第三方 crate，不进行堆分配。
- 默认主机运行时使用标准库，直接复用 Project Z 的 `zmcpio`、`zclap`、`z64`；传递依赖包含 `zcall`、`zconv`。
- 本轮没有出现必须拆成新公共库的独立模块，因此没有新建额外仓库或空子模块。
- 首版语法是有界操作序列，支持分号、换行、注释和类型化参数；没有引入循环、变量或任意系统 shell 语法。

源码与结构说明：[architecture.md](https://github.com/LIghtJUNction/zuse/blob/v0.1.0/docs/architecture.md)、[language.md](https://github.com/LIghtJUNction/zuse/blob/v0.1.0/docs/language.md)。

## 已实现的能力

| 范围 | 首版实现 |
| --- | --- |
| 鼠标与键盘 | 坐标读取、移动、点击、滚动、按键、输入；线性/缓动/贝塞尔轨迹、带种子的抖动与随机等待 |
| Linux 桌面 | X11/XTest 原生输入、窗口枚举、窗口/全屏 PNG；Hyprland/Wayland 接入及对应系统工具适配 |
| Windows / macOS | Windows user32、GDI 和 UIA 路径；macOS Quartz、ImageIO 和系统无障碍路径 |
| 结构化观测 | 窗口信息、无障碍树、文本、边界与动作；Linux AT-SPI；可选 OCR 工具接入 |
| 后台桌面操作 | 向明确指定的窗口发送定向事件；能否接收取决于平台和应用 |
| 浏览器 | 原生 WebSocket/CDP；独立 Chromium 配置、可见/无头运行、页面结构、文本、输入、点击、截图和清理 |
| Android | 原生 Rust ADB smart socket / shell-v2；设备列表、连接、状态、显示目标、输入、应用启动、截图和观测 |
| Wine | 在 Linux X11/XWayland 路径识别和控制 Wine 窗口 |
| IDE / harness / Omarchy | IDE 打开/定位/差异/扩展入口；受管理子进程和有界输出；Omarchy 命令与 Hyprland IPC |
| 人类协助 | 审批、密码、安全密钥触碰、登录及权限请求；通知、持久待办、超时、拒绝和一次性恢复 |
| MCP | stdio、Streamable HTTP、Unix socket；结构化工具定义、内联 PNG、协助请求及恢复 |
| 运行约束 | 只读、试运行、步骤/等待上限、程序预校验、持久会话及拥有对象的退出清理 |

平台细节及依赖见 [platforms.md](https://github.com/LIghtJUNction/zuse/blob/v0.1.0/docs/platforms.md)。

## 安装与使用

源码安装需要 Rust 1.100 或更新版本：

```sh
cargo install zuse-computer --version 0.1.0 --locked
zuse --version
zuse doctor
zuse schema
```

也可以从 Release 下载对应平台的预编译包，将 `zuse` 或 `zuse.exe` 放入 PATH；这种方式无需安装 Rust。

常用入口：

```sh
zuse eval 'mouse.position(); window.list()'
zuse check examples/desktop.zuse
zuse run examples/desktop.zuse
zuse shell
```

`check` 只检查程序。`shell` 保留会话状态，每一行须为一个完整程序；跨行程序可保存为文件后用 `run` 执行。

轨迹与随机等待示例，参数中的时间单位为毫秒：

```text
mouse.move(x=400, y=240, duration=350, curve="bezier", jitter=1, seed=42)
wait(min=80, max=160, seed=42)
mouse.position()
```

作为 Rust 库使用：

```toml
[dependencies]
zuse = { package = "zuse-computer", version = "0.1.0" }
```

只需要解析与轨迹核心时增加 `default-features = false`。

### MCP

```sh
zuse mcp --stdio
zuse mcp --host 127.0.0.1 --port 8377
zuse mcp --socket "$XDG_RUNTIME_DIR/zuse.sock"
```

HTTP 地址为 `http://127.0.0.1:8377/mcp`。Unix socket 的父目录须存在；Windows 使用 stdio 或 HTTP。对非回环地址监听时，要求配置至少 32 字节的 `ZUSE_TOKEN` 并使用 Bearer 认证；HTTP 本身不提供 TLS。

### Android 两种控制方式

需要本机已有并获设备授权的 ADB server，默认地址为 `127.0.0.1:5037`。zuse 直接实现 ADB 通信协议，不会自动启动 server 或确认手机的调试授权。

```sh
adb start-server
zuse eval 'android.devices()'
```

抢占式操作主显示，将序列号替换为实际设备：

```text
android.connect(serial="YOUR_SERIAL", mode="preempt")
android.status()
android.snapshot()
android.tap(x=240, y=160)
android.capture(path="android-main.png")
```

`preempt` 与 `foreground` 等价，指对 display 0 的前台操作，不包含优先级调度或独占锁。

后台操作绑定设备已存在的非零显示。下面假定实际逻辑显示 ID 为 4：

```text
android.connect(serial="YOUR_SERIAL", mode="background", display=4)
android.status()
android.snapshot()
android.tap(x=240, y=160)
android.capture(path="android-background.png")
```

目标显示消失时返回错误，不会自动切换到主显示。截图另行解析 SurfaceFlinger 显示 ID；映射缺失或不明确时返回错误。

### 人类协助

例如先请求审批，再继续后续动作：

```text
human.request(kind="approval", message="Please confirm you are ready to continue.", timeout=300)
wait(ms=1)
```

本地操作人可以通过 `zuse human list` 查看请求，再使用 `assist`、`approve`、`deny` 和 `resume` 处理。密码输入留在本机人工交互流程中，不通过 MCP 请求参数或执行日志传递。安全密钥和登录请求用于通知人类完成相应动作，完成后恢复剩余程序。

涉及浏览器或子进程时，应使用持久 MCP 或 `zuse shell` 会话；单次 CLI 退出会清理它拥有的浏览器和子进程，持久待办本身不会保存这些活对象。

## 验证结果

| 检查 | 结果 |
| --- | --- |
| Rust 测试 | 53 项通过 |
| MCP 真实进程传输测试 | 12 项通过，零跳过 |
| no_std | 原生目标及 wasm32 检查通过 |
| 静态检查与打包 | rustfmt、Clippy warnings-as-errors、Cargo package 验证通过 |
| Linux X11 | 隔离 Xvfb 中实测前台/定向后台输入、坐标、点击、滚动及 PNG 内容 |
| Linux 无障碍 | 私有 D-Bus/Xvfb/GTK 中实测树读取、文字编辑、聚焦、按钮动作与密码脱敏 |
| Chromium | 隔离且保留 sandbox 的浏览器中实测导航、Unicode 输入、点击、结构/文本、截图及清理 |
| Android | 仿真 ADB 服务端验证真实协议帧和显示定向；真机完成只读连接/状态检查 |
| 五个平台正式构建 | 全部成功并上传 Release |

发布后重新下载了 Linux x86_64 包，校验其 SHA-256，并运行 `--version` 与轨迹示例的 `check`，结果通过：

- 压缩包大小：566,327 字节。
- SHA-256：`b215bfa23958a7272c2bd246c3cb2940aabeb5e1f4aff9b8dcfa6cb1ab4cc9db`。
- 版本输出：`zuse 0.1.0`。

验证方法和复现命令见 [验证记录](https://github.com/LIghtJUNction/zuse/blob/v0.1.0/docs/validation.md)。

## 性能基线

在 Linux x86_64、Rust 1.100.0 上，对首版源码的本地 release 构建进行了单次测量：

| 项目 | 结果 |
| --- | --- |
| 可执行文件 | 1,146,072 字节，约 1.09 MiB |
| MCP 初始化并列出工具后的空闲 RSS | 3,428 KiB，约 3.35 MiB |
| 空闲线程 | 1 |
| 300,000 条调用解析 | 约 229 ns/条 |
| 650,000 个轨迹采样 | 约 15.75 ns/点 |

这些是单次基线，不代表完整电脑操作延迟，也不是与其他框架的比较。浏览器与目标应用属于独立进程，内存另计；没有报告未测量的冷启动指标。完整测量方法见 [性能记录](https://github.com/LIghtJUNction/zuse/blob/main/docs/performance.md)。

## 首版边界

1. Windows/macOS 原生实现已完成，正式二进制已构建；当前没有在真实 Windows/macOS 桌面完成输入、截图和权限对话框的端到端验收。
2. Android 后台模式要求已有第二显示，不负责创建虚拟显示。后台快照返回对应显示的窗口元数据；不会把主显示的 UIAutomator 树当作后台树。
3. Android 文字输入目前限制为最多 4096 字节可打印 ASCII，并拒绝字面 `%s`；中文、emoji 需要后续 IME 路径。前台 XML 观测依赖设备是否提供可用的 `uiautomator dump`。
4. 浏览器后端面向 Chromium 系 CDP。需要系统中安装兼容浏览器；Firefox/Safari 专用协议不在首版范围。
5. Wayland 能力依赖合成器、相应工具和系统权限。后台窗口事件是否生效取决于应用；Wine 复用宿主路径，尚未做独立 Wine 应用兼容性实测。
6. IDE、Omarchy 和 harness 复用其现有命令接口，不自动安装目标程序；harness 使用管道，尚未提供通用 PTY 终端模拟。
7. 语法和执行器提供有界批处理；复杂循环和任务规划由调用它的 agent 负责。

## 后续版本发布

已配置 PR/手动 CI、正式发布和版本递增工作流。版本递增会更新版本与锁文件、提交、创建标签、推送，再调用五平台正式发布流程。

下一次需要发布补丁版本时可运行：

```sh
gh workflow run bump.yml --repo LIghtJUNction/zuse -f version=patch
```

也支持 `minor`、`major` 或明确的语义版本号。该命令会产生新的正式发布，应在下一次准备好发布时执行。

工作流和本地开发要求见 [development.md](https://github.com/LIghtJUNction/zuse/blob/main/docs/development.md)。
