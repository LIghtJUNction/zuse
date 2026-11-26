# zuse

**为 agent 设计的电脑操作语言、Rust 库、命令行程序与 MCP 服务。**

zuse 把小段程序变成电脑观察与操作，支持桌面、浏览器、移动设备、串口和 USB。程序先计算坐标、展开有界控制流，再执行输入；常驻会话保留状态，供外部控制器逐步观察和决策。

[English](README.md) · [语法](docs/language.md) · [数值程序](docs/scalar-programs.md) · [常驻会话](docs/sessions.md) · [协议 v1](docs/session-protocol.md) · [平台能力](docs/platforms.md)

[书写与机器控制](docs/handwriting.md)介绍单线字体、基准距离、逐字偏移、鼠标与模拟触控书写、写字机 G-code 和 X/Z 车床。[Zuse Writer 桌面软件](apps/writer/README.md)使用 zgpui，在同一 Zuse 会话内供人和 AI 一起控制写字机。

## 安装与使用

~~~sh
cargo install --git https://github.com/LIghtJUNction/zuse --locked
zuse doctor
zuse schema
zuse eval 'window.list()'
zuse script examples/scoped-mice.zuse --dry-run
zuse shell
~~~

Rust 要求 1.100 或更高版本。Cargo 包名为 `zuse-computer`，项目、库导入名和可执行文件都叫 `zuse`。crates.io 的短包名已被其他项目注册。

### 更新命令

~~~sh
zuse update --check       # 查询最新稳定版，不安装
zuse update --dry-run     # 返回具体更新命令，不安装
zuse update              # 更新有 Cargo 所属记录的用户安装
~~~

更新器通过 `curl` 与 Cargo 核对安装来源，保留安装目录、features、profile 和 target，选择精确的稳定版本或官方 Git 标签；较新的开发版本保留。系统/AUR 安装使用包管理器，便携、自定义及固定 revision 的安装返回手动说明。stdout 返回 JSON，stderr 显示进度与错误；详见[更新行为](docs/update.md)。

下面的普通命令示例明确选择用户的系统鼠标：

~~~text
mouse.use(kind="user", appearance="system")
mouse.move(x=400, y=240, duration=350, curve="smooth", jitter=0.6, seed=42)
mouse.click(button="left")
keyboard.type(text="你好")
wait(min=80, max=160, seed=42)
screen.capture(path="desktop.png")
mouse.release()
~~~

`zuse check 文件.zuse` 或 `--dry-run` 返回执行计划；`--read-only` 拒绝输入、程序启动、JavaScript 执行和设置修改。运行时先检查整个程序，再执行第一步；执行失败时返回已完成步骤并停止。

目标点移动及 `mouse.click_at` 默认使用 350 ms、`curve="smooth"` 和 0.6 px 抖动，接近终点时抖动归零。常规移动保持平滑，提速优先合并调用；`duration=0` 留给明确选择的瞬移操作。

## 数值程序与绘图

短程序直接交给 `zuse -c '源码'`，多行源码使用 `zuse script -`：

~~~sh
zuse script - --dry-run <<'ZUSE'
let size = 80
for i in 0..3 {
    let x = 40 + i*100
    draw.triangle(x1=x,y1=100,x2=x+size,y2=100,x3=x+size/2,y3=20)
}
ZUSE
~~~

不可变 `let`、算术、比较、`if`/`else`、整数半开 `for`、纯数学函数和递归均在准备阶段求值。每个循环最多 4096 次，调用深度最多 32，展开工作最多 16384 次；非法计算在输入前失败。见[数值语法](docs/scalar-programs.md)与[递归分形示例](examples/fractal-sierpinski.zuse)。

`draw.triangle` 将三个数值顶点连成一笔闭合描边。执行前选好应用画笔，并按真实窗口绑定 `anchor.set`；`mouse.click_at`、`mouse.stroke` 和 `draw.*` 校验窗口身份、焦点、几何与缩放。同窗标签页共用 ID；看到用户切页时停止后续输入，重新观察内容。见[绘图与锚点](docs/drawing.md)。

## 常驻会话与外部决策

Unix 上的命名会话可跨 CLI 调用保留函数、设备和锚点：

~~~sh
zuse session start board --idle-timeout-ms 900000
zuse session observe board - <<'ZUSE'
window.list()
ZUSE
zuse session result board
~~~

取得观察结果后，由外部控制器决定下一步，把短程序交给 `zuse session script board -`，按返回的 `job_id` 查询结果，再观察。`accepted` 表示已接收，完成情况以结果回执为准。同一会话一次执行一个作业；`status`、`result`、`cancel`、`close` 可继续控制当前工作，取消后需查询清理结果。

公开的 [JSON-lines 协议 v1](docs/session-protocol.md) 使用 `start` 返回的私有 Unix socket。稳定请求 ID 用于有界重试，`--expect-revision` 检查 Zuse 会话状态；应用内容仍需重新观察。该通道执行 flat Runtime 及展开后的数值程序；scoped mouse 仅支持 dry-run 预检。默认空闲 15 分钟释放会话，运行时间不计入空闲；单一 stdin 流仍可使用 `zuse shell`。支持原生信号处理的构建中，SIGINT 或 SIGTERM 会取消当前工作、清理 shell 持有的运行时与子进程，并丢弃排队命令；空闲等待和未输入完整一行时也会清理退出。详见[会话说明](docs/sessions.md)与[shell 取消](docs/local-programs.md#timing-and-cancellation)。

随附的 [Python 控制器示例](examples/session_client.py) 从 stdin 提交一次源码，并在同一连接上等到作业终态。它只使用 Python 标准库，接收实际观察到的 socket 路径、执行预算及可选修订号。

## 可复用函数与本地事件

在同一个 `zuse shell` 会话中导入一次，后续命令直接调用：

~~~text
import(path="skills/zuse/assets/wechat.zuse", as="wechat")
eval(source="fn delay(ms){wait(ms=ms)}", as="local")
local.delay(ms=10)
~~~

在函数前写连续的 `///` 描述，再用 `functions(namespace="wechat")` 查询限定名称、参数顺序、说明和源码位置。查询只读取已登记的元信息，不调用函数、不重新读取文件。MCP 提供同一个 `zuse.functions` 目录，Rust 提供类型化的 `function_metadata()`。可运行 `zuse run examples/function-catalog.zuse --read-only` 查看完整例子。

微信示例只登记读取、准备文本和发送按钮三个函数；调用前必须从实际应用确认窗口、会话和节点，创建函数不会自动发送消息。也可用 `import(source="...", as="模块名")` 导入源码。显式 `eval` 可同时定义函数并执行动作。整段展开结果共用预检查、只读限制和累计预算；预检失败或 dry-run 不提交新定义。文件中的相对导入以该文件目录为基准。

Git 包通过 `pkg init/add/install/update/remove/list/publish` 管理，`zuse.lock.json` 固定完整提交。`pkg:名称` 与 `pkg:名称/导出` 从已安装的本地源码导入声明。用 `pkg install --locked` 恢复依赖；见[包管理](docs/packages.md)和[标准库](https://github.com/LIghtJUNction/zuse-std)。

`observe`、`watch`、`wait_until` 默认返回结构化文本，通过局部 DOM 或 AT-SPI 事件等待变化。明确调用 `watch_until_click` 才把出现事件连接到点击，限定原标签页、域名、容器、次数和超时。`timer.start/reset/stop/status` 提供每条命令及累计耗时。可选 `--features vision` 增加多尺度图标匹配与按需渲染的视觉时间切片。完整用法见[本地程序说明](docs/local-programs.md)、[Zuse 技能](skills/zuse/SKILL.md)和[实测记录](docs/validation-20261121.md)。

`keyboard.state()` 将当前 Wayland 键盘与其它设备分开；文本输入保持锁状态。明确设置 NumLock 使用 `keyboard.num_lock(state="on")`、`off` 或 `toggle` 并读取实际回读。Wayland 的彩色标记跟随共享用户鼠标、点击穿透；独立多鼠标仍依赖 X11 MPX。原生 pinch 不可用时明确返回不支持。

`keyboard.hold(key="Up",duration=400)` 与 `browser.hold(key="ArrowRight",duration=300)` 提供有时限的方向键按住与释放，绑定已观察目标并响应取消，见[按键保持](docs/key-holds.md)。[ChatGPT 桌面模块](docs/chatgpt-desktop.md)提供本地观察、带进程和空编辑器检查的草稿准备，以及需要明确授权的发送函数；目标需暴露原生无障碍接口。

## 独立鼠标、变量生命周期与并行轨迹

`zuse script` 让每个鼠标变量拥有自己的位置、按键状态和可见光标，默认创建独立鼠标。Linux 原生 X11 的 XInput2/MPX 提供独立设备身份，因此移动这些鼠标不会移动用户的鼠标。颜色、形状、大小、发光强度和自定义图像都可以设置。

~~~text
let cyan = mouse(color="#2FE2FF", shape="arrow");
let violet = mouse(color="#B58CFF", shape="ring");
parallel {
    cyan.move(x=320, y=180, duration=600);
    violet.move(x=520, y=320, duration=600);
}
~~~

用 `zuse script examples/scoped-mice.zuse --dry-run` 预检，在支持的 X11 会话里执行。块和函数拥有局部鼠标，别名与参数借用已有鼠标。`sequence` 串行执行，`parallel` 的直接子语句各为一个分支；连续操作放入同一分支块。同一鼠标的并行冲突在输入前拒绝。

在 Hyprland 上，`mouse(kind="semantic")` 可以创建多只独立显示的穿透光标。`activate` 按无障碍节点定向触发控件，光标位置独立保存。用法和 GTK 演示见[作用域鼠标说明](docs/scoped-mice.md#wayland-semantic-mice)。

普通 `mouse.*` 动作现在也默认使用独立鼠标。要控制用户鼠标，普通命令先调用 `mouse.use(kind="user", appearance="system")`，作用域脚本使用 `user_mouse(appearance="system")`。这一显式系统模式适用于已有的 Wayland、Windows 和 macOS 后端；独立鼠标或自定义原生光标不受支持时直接报错。普通运行时的 `mouse.position()` 只观察已经选定的鼠标，需要先调用 `mouse.use()` 或执行一个鼠标动作。

作用域退出、运行错误和协作取消都会触发清理，包括释放程序按住的鼠标键。支持的 Linux 构建中，`zuse script` 的 SIGINT/SIGTERM 通过取消标记走正常清理路径；SIGKILL、进程崩溃等无法运行析构函数的终止不保证清理。完整语法、Rust 接口、目标定位、外观格式和平台边界见[作用域鼠标说明](docs/scoped-mice.md)。

## 移动到文本或按钮

`mouse.move_to` 一次完成定位和移动：先读取目标的无障碍信息，再获取当前鼠标位置，沿指定轨迹移动到目标中心加偏移的位置。默认耗时 350 毫秒，只移动鼠标。

~~~sh
zuse eval 'mouse.move_to(text="保存",role="button",duration=650,curve="bezier",jitter=1.2,speed_variation=0.25,seed=42,offset_x=4,offset_y=-3)' --dry-run
zuse eval 'mouse.move_to(text="继续",role="button",speed=600,via="320,180;480,220")' --dry-run
cargo run --example target_motion
~~~

`duration` 与 `speed` 二选一；`via` 指定绝对坐标，`jitter`、`speed_variation` 和 `seed` 控制可复现轨迹。定位依赖原生无障碍文本，同名目标用 `scope` 或 `index` 选择。`--dry-run` 只校验参数；Rust 示例需 `--execute` 才输入。路径规划与 `Runtime::move_to_with` 回调见[轨迹说明](docs/target-motion.md)。

## Project Z 结构

基础命令解析器和鼠标轨迹核心支持 `no_std`，不分配堆内存、零依赖。桌面运行时及作用域脚本的编译器、调度器使用标准库，Rust 依赖树只有 Project Z 自有的 zmcpio、zcall、zconv、zclap、z64、zsys。系统动态库加载复用 zsys 0.1.1；CLI 和 MCP 共用执行器。

鼠标路径包含线性、平滑、缓入缓出和贝塞尔轨迹，支持种子固定的抖动与随机等待。轨迹首尾坐标准确，使用绝对时间安排采样。

`motion::FunctionalMotion` 在 `no_std` 核心中接收自定义路径函数；`mouse.circle` 将完整圆周轨迹安排在一次调用中。定位与桌面事件执行属于启用 `std` 的运行时。

## MCP

~~~sh
zuse mcp serve --stdio
zuse mcp --host 127.0.0.1 --port 8377
zuse mcp --socket /run/user/1000/zuse.sock
~~~

使用 `zuse mcp install codex` 或 `zuse mcp install claude`，以当前可执行文件的绝对路径在用户配置中登记名为 `zuse` 的 stdio 服务，新会话会加载它。`zuse mcp install ssmcp` 通过 SSMCP 的 Zuse 管理器安装匹配的正式版本并返回可执行文件路径。加 `--dry-run` 可预览命令。`zuse mcp --stdio` 也可直接启动服务。

socket 路径请换成自己的运行目录。HTTP 入口为 `/mcp`，默认仅本机访问；绑定其他网卡需要配置至少 32 字节的 `ZUSE_TOKEN`。stdio/socket 支持传统初始化流程和当前协议，HTTP 使用 MCP 2026-07-28。

工具包括执行、预检查、能力发现、语法查询、观察、直接返回 PNG 图片、查看人工请求、恢复执行。`zuse.script` 接收完整作用域脚本及可选的 `dry_run`、`read_only`，每次调用返回前清理该次创建的鼠标；`zuse.execute` 执行普通命令批次。

## 后台浏览器与无障碍

~~~text
browser.launch(headless=true)
browser.viewport(width=1280, height=800)
browser.navigate(url="data:text/html,<input id=message><button id=save>Save</button>")
browser.fill(selector="#message", text="你好")
browser.snapshot()
browser.capture(path="browser.png")
browser.close()
~~~

浏览器使用原生 CDP 连接和独立临时配置，不依赖 Playwright。截图输出当前视口的 PNG；`browser.viewport` 按 CSS 像素设置视口大小。`browser.fill` 遇到密码输入会返回错误，可使用可见浏览器和 `human.request` 交给本机操作人完成。

Linux 无障碍通过 AT-SPI 读取角色、文本、边界、动作，并支持聚焦、按钮动作和编辑文本。`screen.text()` 直接读取文本字段。Windows 使用 UI Automation，macOS 使用 Quartz 与 System Events。

Wayland 的 `window.list()` 和 `window.focus()` 支持 Hyprland、Sway 和 niri。目标 ID 来自窗口列表。Sway 可截取可见窗口的屏幕区域，niri 可使用全屏截图。后端条件和验证范围见[平台支持](docs/platforms.md)。

## Android 两种模式

~~~text
android.devices()
android.connect(serial="YOUR_DEVICE_SERIAL", mode="foreground")
android.status()
~~~

~~~text
android.connect(serial="YOUR_DEVICE_SERIAL", mode="background", display=4)
android.tap(x=240, y=160)
~~~

原生 ADB 客户端要求已授权的 ADB server；省略 serial 时需恰有一台就绪设备。前台操作主显示，后台明确选择已有的非零显示，并绑定输入、启动与截图；映射不明确时拒绝。文本支持最多 4096 字节可打印 ASCII，排除 `%s`，其余输入在预检时拒绝。Android 16 前台、副显示隔离与像素验证见[测试流程](docs/native-testing.md)和[设备证据](docs/evidence/android-physical-2026-11-20.json)。

## 手机浏览器、iOS 与设备 I/O

~~~text
android.browser.sockets(serial="YOUR_DEVICE_SERIAL")
browser.connect_android(serial="YOUR_DEVICE_SERIAL")
browser.navigate(url="https://example.com")
browser.snapshot()
browser.capture(path="phone-browser.png")
browser.close()
~~~

手机 Chrome 通过原生 ADB 隧道连接可响应的调试 socket，在已有配置中创建并控制一个自有标签页，见[手机浏览器](docs/mobile-browser.md)。iOS 使用明确配置的本地 WebDriverAgent，支持触摸、Unicode 输入、观察、截图和应用启动；当前验证为协议夹具，物理 iPhone 验证待完成。准备方式见[iOS 说明](docs/ios.md)。

~~~text
serial.ports()
usb.devices()
~~~

串口通过原生 termios / Win32 COM 接口收发；USB 通过 zsys 加载系统 libusb，
支持 bulk、interrupt 和 control 传输。打开 USB 必须明确指定设备地址、
VID/PID 和接口，不自动分离内核驱动。部分写入会报告已经发送的字节数并停止，
不会自动重发。串口已用 Linux 内核 PTY 实测，USB 传输使用隔离 C ABI 夹具验证，
没有向未知实体设备发送测试命令。详见[设备 I/O](docs/device-io.md)。

MCP 图片工具支持 `source=desktop/browser/android/ios`，可以直接把已连接设备的
PNG 返回给 agent。需要跨多条命令保持连接时，使用 `zuse shell` 或 MCP 服务。

## 人类协助

~~~text
human.request(kind="yubikey", message="请触碰安全密钥，完成后确认。", timeout=300)
window.list()
~~~

人工请求会产生系统通知及请求 ID。操作人执行 `zuse human assist ID`；完成后通过 MCP 重试或 `zuse resume ID` 继续。密码请求要求指定窗口，由本机提示框取得并输入该窗口，不进入 MCP 返回值或执行记录。

需要保留浏览器、harness 状态的流程，请运行在持久的 `zuse shell` 或 MCP 服务中。单次 CLI 退出时会结束自己拥有的浏览器和进程。

## 开发与发布

~~~sh
python3 scripts/check.py
cargo check --no-default-features --lib
cargo bench --bench core
python3 scripts/bump.py patch --commit --tag --push
~~~

工作流支持版本 bump、原子提交/标签推送和五个平台的原生制品。Cargo 发布使用单独验证的源码暂存目录，内部依赖必须与 Git 锁定来源对应的 registry 精确版本及完整发布内容一致。详见 [开发说明](docs/development.md) 与 [验证记录](docs/validation-0.2.0.md)。

MIT 开源。
