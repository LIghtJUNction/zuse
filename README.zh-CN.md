# zuse

**为 agent 设计的电脑操作语言、Rust 库、命令行程序与 MCP 服务。**

zuse 执行一小段可检查的代码，完成观察窗口、移动鼠标、输入文字、读取无障碍树、控制后台浏览器、Android 显示目标与手机 Chrome、iOS，以及串口和 USB 设备。遇到密码、安全密钥或账号登录时，程序可以暂停，请人类处理，再从剩余步骤继续。

[English](README.md) · [语法](docs/language.md) · [平台能力](docs/platforms.md)

## 安装与使用

~~~sh
cargo install zuse-computer --locked
zuse doctor
zuse schema
zuse eval 'mouse.position(); window.list()'
zuse shell
~~~

Rust 要求 1.100 或更高版本。Cargo 包名为 `zuse-computer`，项目、库导入名和可执行文件都叫 `zuse`。crates.io 的短包名已被其他项目注册。

~~~text
mouse.move(x=400, y=240, duration=350, curve="bezier", jitter=1, seed=42)
mouse.click(button="left")
keyboard.type(text="你好")
wait(min=80, max=160, seed=42)
screen.capture(path="desktop.png")
~~~

`zuse check 文件.zuse` 或 `--dry-run` 返回执行计划；`--read-only` 拒绝输入、程序启动、JavaScript 执行和设置修改。运行时先检查整个程序，再执行第一步；执行失败时返回已完成步骤并停止。

## Project Z 结构

语法解析器和鼠标轨迹核心支持 `no_std`，不分配堆内存、零依赖。桌面运行时使用标准库，Rust 依赖树只有 Project Z 自有的 zmcpio、zcall、zconv、zclap、z64、zsys。系统动态库加载复用 zsys 0.1.1；CLI 和 MCP 共用执行器。

鼠标路径包含线性、平滑、缓入缓出和贝塞尔轨迹，支持种子固定的抖动与随机等待。轨迹首尾坐标准确，使用绝对时间安排采样。

## MCP

~~~sh
zuse mcp --stdio
zuse mcp --host 127.0.0.1 --port 8377
zuse mcp --socket /run/user/1000/zuse.sock
~~~

socket 路径请换成自己的运行目录。HTTP 入口为 `/mcp`，默认仅本机访问；绑定其他网卡需要配置至少 32 字节的 `ZUSE_TOKEN`。stdio/socket 支持传统初始化流程和当前协议，HTTP 使用 MCP 2026-07-28。

工具包括执行、预检查、能力发现、语法查询、观察、直接返回 PNG 图片、查看人工请求、恢复执行。

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

工作流支持版本 bump、原子提交/标签推送、五个平台制品及 Cargo 发布。详见 [开发说明](docs/development.md) 与 [验证记录](docs/validation-0.2.0.md)。

MIT 开源。
