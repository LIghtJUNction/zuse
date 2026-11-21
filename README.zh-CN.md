# zuse

**为 agent 设计的电脑操作语言、Rust 库、命令行程序与 MCP 服务。**

zuse 执行一小段可检查的代码，完成观察窗口、移动鼠标、输入文字、读取无障碍树、控制后台浏览器、Android 显示目标与手机 Chrome、iOS，以及串口和 USB 设备。遇到密码、安全密钥或账号登录时，程序可以暂停，请人类处理，再从剩余步骤继续。

[English](README.md) · [语法](docs/language.md) · [函数与本地观察](docs/local-programs.md) · [作用域鼠标](docs/scoped-mice.md) · [文本定位与轨迹](docs/target-motion.md) · [平台能力](docs/platforms.md)

## 安装与使用

~~~sh
cargo install zuse-computer --locked
zuse doctor
zuse schema
zuse eval 'window.list()'
zuse script examples/scoped-mice.zuse --dry-run
zuse shell
~~~

Rust 要求 1.100 或更高版本。Cargo 包名为 `zuse-computer`，项目、库导入名和可执行文件都叫 `zuse`。crates.io 的短包名已被其他项目注册。

下面的普通命令示例明确选择用户的系统鼠标：

~~~text
mouse.use(kind="user", appearance="system")
mouse.move(x=400, y=240, duration=350, curve="bezier", jitter=1, seed=42)
mouse.click(button="left")
keyboard.type(text="你好")
wait(min=80, max=160, seed=42)
screen.capture(path="desktop.png")
mouse.release()
~~~

`zuse check 文件.zuse` 或 `--dry-run` 返回执行计划；`--read-only` 拒绝输入、程序启动、JavaScript 执行和设置修改。运行时先检查整个程序，再执行第一步；执行失败时返回已完成步骤并停止。

## 可复用函数与本地事件

在同一个 `zuse shell` 会话中导入一次，后续命令直接调用：

~~~text
import(path="skills/zuse/assets/wechat.zuse", as="wechat")
eval(source="fn pause(ms){wait(ms=ms)}", as="local")
local.pause(ms=10)
~~~

微信示例只登记读取、准备文本和发送按钮三个函数；调用前必须从实际应用确认窗口、会话和节点，创建函数不会自动发送消息。也可用 `import(source="...", as="模块名")` 导入源码。显式 `eval` 可同时定义函数并执行动作。整段展开结果共用预检查、只读限制和累计预算；预检失败或 dry-run 不提交新定义。文件中的相对导入以该文件目录为基准。

`observe`、`watch`、`wait_until` 默认返回结构化文本，通过局部 DOM 或 AT-SPI 事件等待变化。明确调用 `watch_until_click` 才把出现事件连接到点击，限定原标签页、域名、容器、次数和超时。`timer.start/reset/stop/status` 提供每条命令及累计耗时。可选 `--features vision` 增加多尺度图标匹配与按需渲染的视觉时间切片。完整用法见[本地程序说明](docs/local-programs.md)、[Zuse 技能](skills/zuse/SKILL.md)和[实测记录](docs/validation-20261121.md)。

`keyboard.state()` 将当前 Wayland 键盘与其它设备分开；文本输入保持锁状态。明确设置 NumLock 使用 `keyboard.num_lock(state="on")`、`off` 或 `toggle` 并读取实际回读。Wayland 的彩色标记跟随共享用户鼠标、点击穿透；独立多鼠标仍依赖 X11 MPX。原生 pinch 不可用时明确返回不支持。

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

用 `zuse script examples/scoped-mice.zuse --dry-run` 检查完整示例，在支持的 X11 会话里去掉 `--dry-run` 执行。两条轨迹按时间交错推进；脚本结束时销毁两个鼠标。块和函数拥有各自创建的局部鼠标，别名与函数参数借用已有鼠标。`sequence` 串行执行；`parallel` 的每条直接子语句是一个分支，需要连续操作同一鼠标时，将这些操作放进同一个分支块。对同一鼠标的并行冲突会在输入前被拒绝。

普通 `mouse.*` 动作现在也默认使用独立鼠标。要控制用户鼠标，普通命令先调用 `mouse.use(kind="user", appearance="system")`，作用域脚本使用 `user_mouse(appearance="system")`。这一显式系统模式适用于已有的 Wayland、Windows 和 macOS 后端；独立鼠标或自定义原生光标不受支持时直接报错。普通运行时的 `mouse.position()` 只观察已经选定的鼠标，需要先调用 `mouse.use()` 或执行一个鼠标动作。

作用域退出、运行错误和协作取消都会触发清理，包括释放程序按住的鼠标键。支持的 Linux 构建中，`zuse script` 的 SIGINT/SIGTERM 通过取消标记走正常清理路径；SIGKILL、进程崩溃等无法运行析构函数的终止不保证清理。完整语法、Rust 接口、目标定位、外观格式和平台边界见[作用域鼠标说明](docs/scoped-mice.md)。

## 移动到文本或按钮

`mouse.move_to` 一次完成定位和移动：先读取目标的无障碍信息，再获取当前鼠标位置，沿指定轨迹移动到目标中心加偏移的位置。默认耗时 350 毫秒，只移动鼠标。

~~~sh
zuse eval 'mouse.move_to(text="保存",role="button",duration=650,curve="bezier",jitter=1.2,speed_variation=0.25,seed=42,offset_x=4,offset_y=-3)' --dry-run
zuse eval 'mouse.move_to(text="继续",role="button",speed=600,via="320,180;480,220")' --dry-run
cargo run --example target_motion
~~~

`duration` 指定移动时长，`speed` 按完整采样轨迹的平均速度计算时长，两者互斥。`via` 使用绝对屏幕坐标；`jitter` 与 `speed_variation` 调整途中扰动和进度变化；相同 `seed` 可复现路径。默认精确、区分大小写地匹配文本，同名目标需用 `scope` 或 `index` 明确选择。

定位使用系统原生无障碍树，无需 OCR 或视觉模型。应用需要提供文本及其屏幕区域，纯图片或未暴露无障碍信息的画布文本不在定位范围内。`--dry-run` 只校验参数；Rust 示例默认只生成轨迹，明确传入 `--execute` 才移动真实鼠标。程序可用 `Runtime::move_to_with` 提供接收进度、实际起点和最终落点的 Rust 回调，也可用 `MovePlan` 先取得所有采样点。详见[参数、坐标与 Rust 接口](docs/target-motion.md)。

## Project Z 结构

基础命令解析器和鼠标轨迹核心支持 `no_std`，不分配堆内存、零依赖。桌面运行时及作用域脚本的编译器、调度器使用标准库，Rust 依赖树只有 Project Z 自有的 zmcpio、zcall、zconv、zclap、z64、zsys。系统动态库加载复用 zsys 0.1.1；CLI 和 MCP 共用执行器。

鼠标路径包含线性、平滑、缓入缓出和贝塞尔轨迹，支持种子固定的抖动与随机等待。轨迹首尾坐标准确，使用绝对时间安排采样。

`motion::FunctionalMotion` 在 `no_std` 核心中接收自定义路径函数；`mouse.circle` 将完整圆周轨迹安排在一次调用中。定位与桌面事件执行属于启用 `std` 的运行时。

## MCP

~~~sh
zuse mcp --stdio
zuse mcp --host 127.0.0.1 --port 8377
zuse mcp --socket /run/user/1000/zuse.sock
~~~

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

Linux 无障碍通过 AT-SPI 读取角色、文本、边界、动作，并支持聚焦、按钮动作和编辑文本。Windows 使用 UI Automation，macOS 使用 Quartz 与 System Events；它们的实机验证范围在平台表中单独列出。

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

抢占式模式操作主显示；后台模式必须指定已有的非零显示 ID。输入、应用启动和截图都会检查目标，目标缺失或截图映射不明确时直接报错。此版本支持选择已有的第二显示，不自动创建虚拟显示。

Android 适配器直接实现 ADB 协议。电脑上需运行已获设备授权的 ADB server；建议始终显式指定 serial。省略 serial 时，完整设备列表必须只有一个已就绪设备，不能在多设备中自动挑选。

文字输入支持可打印 ASCII，排除 ADB 保留序列 %s，长度不超过 4096 字节。Unicode、控制字符、非法按键和组件参数在整段程序的预检查阶段拒绝。Android 16 实机已验证前台输入、全部 95 个可打印 ASCII 字符、截图像素，以及自有副显示输入与主显示隔离。详见[实机测试流程](docs/native-testing.md)及[设备证据](docs/evidence/android-physical-2026-11-20.json)。

## 手机浏览器、iOS 与设备 I/O

~~~text
android.browser.sockets(serial="YOUR_DEVICE_SERIAL")
browser.connect_android(serial="YOUR_DEVICE_SERIAL")
browser.navigate(url="https://example.com")
browser.snapshot()
browser.capture(path="phone-browser.png")
browser.close()
~~~

手机 Chrome 使用原生 ADB 隧道，不创建本机端口转发。zuse 在手机现有 Chrome
配置中创建一个自有标签页；移动端列表只返回该页，也不能选择其他标签页。
Chrome 调试 socket 必须能正常响应，仅发现 socket 不等于浏览器已经就绪。
后台行为及测试条件见[手机浏览器说明](docs/mobile-browser.md)。

iOS 通过明确指定的本机 WebDriverAgent 端点执行触摸、Unicode 输入、结构化读屏、
截图、启动应用和打开网址。运行前需要由使用者准备签名并运行的 WDA。
本轮没有苹果手机，验证范围是协议夹具；这不代表已经通过 iPhone 实机验收。
详见[iOS 使用说明](docs/ios.md)。

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
