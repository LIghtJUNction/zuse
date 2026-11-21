# Zuse 0.6.0 发布前验证快照

日期：2026-11-22（Asia/Taipei）。本轮从浏览器绘图、围棋和同机双人游戏的实际使用中发现问题，再修改语言、执行器与控制接口。

本文记录发布前各固定候选的实际验证范围。下述 `8b9d236a…` 候选已通过完整 Rust 门禁、隔离原生按键验收和公开会话客户端验收；各候选的结果按二进制摘要区分。最终 Git SHA、标签、发布资产与消费者回执在正式 Release 说明及 Project Z 传播记录中更新，准备状态见[交付记录](delivery-0.6.0.md)。

`8b9d236a…` 随后的真实主机双人输入在按键前触发 `cannot restore the same active keyboard after virtual input`，促使本轮增加多键盘路径修复。隔离验收和真实主机验收分别记录；用户离开后的修复只进行源码与隔离验证，实际对战留待用户返回。

## 固定候选与结果

验证的可执行文件为 `zuse 0.6.0`，开发机固定路径为
`/home/lightjunction/Documents/GITHUB/project_z/zuse/.ssmcp/bin/zuse-session-0.6.0-8b9d236a`，SHA-256：

```text
8b9d236a971cce998149ffc33eb61a09793c6f637e180a47e9e8ac5fdc560dbd
```

| 门禁 | 实际结果 |
| --- | --- |
| 完整 Rust 测试 | **473 passed、19 ignored、0 failed**；忽略项没有计为通过。 |
| 格式与静态检查 | fmt 通过；Clippy all-targets、`-D warnings` 通过。 |
| 无标准库核心 | 本机与 `wasm32-unknown-unknown` 检查均通过。 |
| 私有原生接收器 | `.ssmcp/zuse-game-holds-8b9d236a.json`，进程退出 0，`native_wayland=pass`；窗口元数据与有界按键检查均为 `ok=true`。 |

该摘要固定了本轮实际测试的二进制；最终发布源提交须在提交后另行记录，不能用二进制摘要代替 Git SHA。

## 后续候选：严格原生设备映射

发布前的后续候选为 `zuse-session-0.6.0-5028964e`，SHA-256：

```text
5028964e84222717bcede915e9f6e937e5d77e955921c589d74c6806a497c889
```

该候选为显式并发按键增加现有 evdev 设备路径。它按照 Hyprland v0.57.2 的实际名称规范化与全部 HID 设备去重规则检查唯一映射，保留原设备、XKB 映射和锁定状态；设备身份、权限、目标键能力或所有权不满足条件时拒绝。原始规则与传输边界见[按键保持](key-holds.md)。

| 实际命令 | 结果 |
| --- | --- |
| `cargo test --locked --offline --lib --test key_hold -j 2` | **192 passed、4 ignored、0 failed**。包括新增的名称碰撞、外来目标键保护、部分写入清理和完整设备清单检查。 |
| `cargo fmt --all -- --check` | 退出 0。 |
| `cargo clippy --locked --offline --all-targets -j 2 -- -D warnings` | 退出 0。 |
| `cargo build --locked --offline -j 2` | 退出 0，固定二进制报告 `zuse 0.6.0`。 |
| `cargo test --locked --offline --lib native_evdev_preflight_current_keyboard_without_input -j 2 -- --ignored --nocapture` | 退出 0，明确输出 `PASS: exact native identity, capability and same-FD verification; input_events=0`，本次没有跳过。 |

只读原生预检实际完成了当前设备的唯一映射、现有读写 FD、按键能力、身份和状态读取，随后关闭未提交事件的 FD。它没有发送 keydown/keyup、切换设备或修改用户设置。`native_evdev` 接收器时序及真实对战仍未验收；前述私有 Wayland 接收器结果属于 `8b9d236a…` 的虚拟设备路径。

增量记录保存在 `.ssmcp/validation/native-evdev-0.6.0/`。此前的 473 项完整测试与 host/wasm `no_std` 结果仍按原候选保留，没有把增量测试数量相加或改记成新候选的全量运行。

## 源码与验证对象

本轮保留已发布 0.5.0 的方向键保持和桌面模块，合并基线为
[`b5ebbd3679af215706cbd73b17045e014f5e0524`](https://github.com/LIghtJUNction/zuse/commit/b5ebbd3679af215706cbd73b17045e014f5e0524)。
发布候选包含绘图锚点、Git 源码包、有界数值程序、命名会话，以及实际双人输入所需的显式并发选项。

下表记录不同阶段实际运行的检查。测试集合存在重叠，不能将各行数量相加当作独立测试总数。

| 验证对象 | 实际结果 | 范围 |
| --- | --- | --- |
| 绘图、包管理、CLI、shell 与既有按键保持 | 221 passed，4 ignored | 首轮候选的相关单元与集成检查；忽略的原生环境用例没有计为通过。 |
| 既有函数与模块兼容 | 38 passed | function metadata、runtime modules、script entrypoints、script modules。 |
| 数值语言和三角形 | 87 passed | drawing 10、scalar programs 13、script compile 34、script modules 15、script syntax 15。 |
| 首个固定会话候选 | 184 passed，3 ignored | lib 173、sessions 11；包含非阻塞取消请求与真实 Unix socket 客户端。 |
| 合并数值程序、会话、默认移动与游戏并发输入 | 470 passed，19 ignored，0 failed | 修复保持截止边界之前的合并候选。 |
| 保持截止修复后的固定候选 | 473 passed，19 ignored，0 failed | 增加三项截止回归后重跑全量，并通过上表全部门禁。 |

首个会话候选为 `zuse 0.6.0`，SHA-256：

```text
3a4aba42e4c5cecab1463e07ea377155ca3777a7db9945792dc07b4c05ab5cf4
```

该候选完成了 start、数值脚本提交、结果读取、跨任务函数复用与 close 的实际检查。后续使用又暴露了嵌套观察结果的 JSON-lines 换行问题，因此这个摘要不代表最终发布候选。

## 公开标准库安装

[`std 0.1.0`](https://github.com/LIghtJUNction/zuse-std/tree/v0.1.0) 已发布为纯源码包。
`main` 与 `v0.1.0` 均指向
[`d79f43f8f3c2a1bff05962420f014b6c16403c5c`](https://github.com/LIghtJUNction/zuse-std/commit/d79f43f8f3c2a1bff05962420f014b6c16403c5c)。
仓库包含 22 个公共函数、5 个导出，明确要求 Zuse 0.6.0。

在全新项目中，通过公开 URL 实际执行 `pkg add`、`pkg install --locked` 与 `pkg list`，均退出 0；锁文件固定到上述完整提交，安装状态为 `locked: true`、`installed: true`。导入 `pkg:std` 返回 22 个函数。完整 API 示例预检为 43 步；导入 `pkg:std/fractal` 后，深度 5 的递归程序预检生成 243 个三角形操作。所有递归和坐标计算由 Zuse 源码完成，没有宿主语言预计算坐标。

这组公开安装检查使用前述 `3a4aba42…` 固定候选；它验证源码包的实际发布与安装，不代表 Zuse 0.6.0 本体已经发布。

## 公开会话客户端验收

[`examples/session_client.py`](../examples/session_client.py) 只使用 Python 标准库，读取 stdin 中的真实 Zuse 源码，通过显式指定的 Unix socket 提交一次，再在同一 JSON-lines 连接按 `job_id` 读取结果。它不生成绘图坐标，也不直接发送桌面输入。用法与超时、取消约定见[会话文档](sessions.md)。

本轮验收固定的客户端源码为 10,147 字节，SHA-256：

```text
137cf04bbdba851011b1e7785a2b1408836d1dbdfba13d3cd6a78894a032d395
```

使用 `8b9d236a…` 候选启动属于夹具的独立 `session serve`，清除其 `DISPLAY` 与 `WAYLAND_DISPLAY`。实际执行的源码只含 `wait`、`timer.status`、`functions` 和函数声明；结束时关闭自己的服务并清理临时目录。首次检查完成函数注册、调用与后续只读任务中的函数复用。随后九项边界检查全部通过，执行进程退出 0：

| 检查 | 已观察的结果 |
| --- | --- |
| 同连接提交与轮询 | 代理只接受一条连接；只收到一次源码提交，其后均为 `result`，各请求 ID 独立。 |
| 只读任务 | `--observe` 的 `functions()` 正常完成。 |
| 过期修订号 | 返回 `stale_revision`，未创建新任务。 |
| 执行预算 | 两步源码在 `--max-steps 1` 下预检失败，完成步骤为 0。 |
| 客户端超时 | 返回已知任务 ID，退出 124；未重放或取消，原任务随后正常完成。 |
| Ctrl+C | 返回退出 130 和清理尚未完成的提示；带修订号的取消成功，终态为 `cancelled`，下一任务正常完成。 |
| 源码上限 | 超过 256 KiB 的 stdin 在提交前被拒绝，未创建任务。 |
| stdin 等待 | 输入流未关闭时仍受总超时约束，超时前未提交源码。 |
| 响应帧上限 | 独立协议夹具发送超过 2 MiB 的帧时被拒绝，未发生第二次源码提交。 |

独立验收没有接入用户的游戏会话。随后实际控制会话 `live-play` 的任务 11 已通过此客户端完成 `keyboard.state`、`window.list` 与 `window.capture`，返回协议结果及图像；这确认了实际观察链路，键盘输入的主机阻塞仍见下节。

## 实际使用暴露的边界

绘图批次在真实 Chrome 画板中执行，窗口锚点按观察到的原生窗口位置、尺寸、缩放和焦点验证。画板自身的画笔状态不属于锚点状态；实际观察发现，滑块点击同一数值不会触发页面的值更新，因此操作脚本需要先改变数值再设置目标值。

19×19 同屏双人围棋页面已打开并实际落子。原生窗口 ID 在切换浏览器标签页后可能保持不变，因此窗口锚点不能代替应用内容检查。观察到用户切页后停止继续落子；未在棋盘确认的 Q4 没有记为成功。

首次实际会话观察含有原生窗口列表中的嵌套格式化 JSON。服务端直接嵌入这个结果会在一帧中产生硬换行，导致客户端提前结束读取。最终候选将协议响应保持为单行，同时保留结果对象、字符串内容与转义；真实 Unix socket 流回归已纳入上述完整测试。

同机双人游戏还暴露了输入的排他假设：用户按住 WASD 时，原方向键保持会拒绝或中断另一方操作。显式 `allow_other_keys=true` 只允许真实键盘映射中的普通外部按键共存，保留目标键所有权、修饰键、窗口、权限和取消检查。原默认排他行为不变。

### 8b9 候选的真实主机阻塞与修复目标

首次失败时，主机列出 10 台键盘；活动设备为 `at-translated-set-2-keyboard`，后面还有 ydotool、fcitx5 等设备，活动键盘并非列表末项。Num Lock 开启、Caps Lock 关闭、布局组为 0，用户正按住 WASD 操作。请求 `Left` 保持 400 ms 并显式设置 `allow_other_keys=true`，在约 57 ms、`failed_step=0` 返回上述活动键盘恢复错误；尚未创建临时输入设备或按下本次键。

后续只读检查发现，Hyprland `hl.get_devices()` 返回的全部 10 台键盘均无 `sysname`，包括实体 AT 键盘；按该 AT 设备的确切地址查询也返回 `-1`，该接口未提供可解析为 `eventN` 的原生设备映射。修复采用 Hyprland 0.57.2 的实际名称规范化与全部 HID 设备去重规则建立唯一映射，并核对内核身份、能力和原 FD；后续候选的纯测试、静态门禁和无输入原生预检结果见上节。

固定的 `8b9d236a…` 及其完整测试结果保持不变。没有通过猜测设备、改权限、关闭 Num Lock、更换活动键盘或修改用户设置绕过保护。该候选检查截点位于 `main` 与 `v0.6.0` 发布前；后续修复和隔离验证按新候选另列回执。

用户已转去推进其他项目，并授权继续后台修复、隔离测试与如实记录后的发布。后续工作不抢窗口、不向主机发送测试按键；真实对战验收留待用户回来。隔离验证、版本发布和真实游戏效果分别记录。

## 原生输入验收方式

`tests/native_key_hold.py` 在私有 Hyprland 环境中保留一个属于夹具的 W 键，观察 Right、slash、period、m、n 的真实 GTK 按下与释放事件。检查自己的键结束后 W 仍按住；默认排他、修饰键已按下和目标键已被占用的情况必须失败，而且不得释放夹具持有的键。这个夹具不向用户桌面发送测试输入。

第一次启动因系统 Cage 只声明 XDG-shell v5、私有 Hyprland 需要 v6 而被环境检查拒绝。随后复用已有的私有 Cage 构件，核对其构建摘要与 wlroots v6 探针，没有修改协议检查或连接用户桌面。

真实接收器随后发现 180 毫秒按键保持的边界错误：最后一次状态查询使用总按键期限，可能在正常到时附近返回 `timeout: Hyprland IPC deadline reached`。旧失败回执记录 188,764 微秒；当时未标出具体用例，不能据此断言失败发生在首个方向键还是后续并发用例。

`8b9d236a…` 候选用带来源的截止区分保持结束与通信预算：保持到时只结束观察并释放本次按键；通信自身超时、断线、无效响应和已发现的目标变化仍失败。没有为补查延长按住时间，也没有按错误字符串吞掉超时。新增回归覆盖正常到时、真实故障不转成功，以及到时收到 `stale_target` 仍失败并释放。

重新验收使用私有 Cage 0.3.1 v6 构件与 Hyprland v0.57.2。报告隔离模式为 `owned-headless-cage-and-private-hyprland`，`host_display_connection=false`、`host_focus_or_pointer_queries=false`、`physical_input_or_kms_fds=0`。

| 接收器用例 | 请求保持 | 实测按下至释放 | 额外确认 |
| --- | --- | --- | --- |
| ArrowRight | 180 ms | 172.759 ms | 本次键已释放。 |
| 用户 W + Right | 180 ms | 172.746 ms | 自己的键释放，W 仍按住。 |
| 用户 W + slash | 180 ms | 169.990 ms | 自己的键释放，W 仍按住。 |
| 用户 W + period | 180 ms | 171.193 ms | 自己的键释放，W 仍按住。 |
| 用户 W + m | 180 ms | 172.901 ms | 自己的键释放，W 仍按住。 |
| 用户 W + n | 180 ms | 173.462 ms | 自己的键释放，W 仍按住。 |
| 取消 | 在原保持期内取消 | 65.109 ms | 取消后本次键已释放。 |

请求时长与接收器看到的间隔分别记录，不能把请求值当作实际送达时长。焦点变化用例按预期返回 `stale_target`，并确认 `compositor_keys_released=true`。默认排他、已有修饰键、目标已被占用的拒绝用例分别保留原始按键 `[25]`、`[25,50]`、`[25,114]`。Num Lock 关闭和开启两种情况下，原键盘状态均保持不变。

原始报告路径为 `.ssmcp/zuse-game-holds-8b9d236a.json`，SHA-256：

```text
ab6a54436ed82c953e6e03d85d91687bc9da1f88772481b3ce02df084050fcbe
```

执行入口（`CAGE_V6` 为已核实的私有测试构件）：

```sh
python3 tests/native_wayland_private.py --headless --cage "$CAGE_V6" --hold-tests \
  --binary /absolute/path/to/verified/zuse --report /tmp/zuse-game-holds.json
```

本记录确认候选的语言、协议和隔离输入验收，不代表坦克对局胜利，也不代表递归分形已经在真实画板完成。最终 Git 提交、发布标签及消费者固定版本仍须形成各自回执；上游发布不等于消费者已经升级。
