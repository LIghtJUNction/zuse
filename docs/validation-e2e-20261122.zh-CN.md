# 2026-11-22：桌面与浏览器迭代、阶段验收草案

本轮已通过纯 Zuse 的 ChatGPT 桌面多语言输入、发送和回复读取链路，两项整体验收仍有未完成项。早先官方 [I'm Not a Robot](https://neal.fun/not-a-robot/) 已到第 15 关停车的第二张图，尚未通关。连接故障恢复后，新一轮全程纯 Zuse 重跑已确认通过第 1–10 关，最后一次实际观察为第 11 关；ChatGPT 归档结果仍待单独确认。因此当前证据支持已执行的文本与控件操作，不能据此宣称快速通关或完全控制所有桌面功能。

## 纯 Zuse 重跑检查点

本次恢复复用了原 Chromium 与页面，由新的 Zuse shell attach 现有 CDP session，没有重开浏览器。页面读取通过 `browser.evaluate` 的公开 DOM、CSS 显示属性与 `browser.capture`；输入通过 Zuse 的 selector 点击与文本填入。第 3 关的新文字为 `FNZZSD`，第 4 关图片顺序也发生变化，均以本次显示内容为准。第 5 关从公开 CSS 观察到旋转角度为 0、0、180、180、270、270、180、180、0 度，经真实点击后九块均返回单位矩阵，Verify 后页面显示 `Level 6: XOXO`。相关程序和返回值见[本次重跑检查点](evidence/pure-game-through-l05.json)。

后续第 6 关先试验了旧路线，但没有获得胜局。重新观察空棋盘后，采用角落开局与分叉：X 依次落在 1、9、7，再落在未被 O 占据的获胜格 4；公开 SVG 标记确认连线 1、4、7，Verify 后晋级。第 7 关根据本次可见字母重新查找单词，第 8 关根据本次截图填写 `867V309`；第 9 关按实际四叉细分逐次校验可见方格矩形，最终选择 31 个与停止标志相交的格子。相关回执见[第 6–10 关检查点](evidence/pure-game-through-l10.json)。

第 10 关暴露了真实标签可见性的边界。Chromium 当时显示“新标签页”，游戏标签处于隐藏状态；`background(true)` 虽让 focus 与 visibility 观察返回可用状态，却没有让该场景的 CSS 缩放动画前进。公开观察反复得到动画时间 0 与零面积矩形，不能据此点击不可见元素。Zuse 原生 `window.focus` 和窗口限定 `Ctrl+2` 激活游戏标签后，先读回 `visibilityState=visible`；原有 `.mole.active` 等待点击随后在 6720 ms 内确认剩余四次 trusted 输入，检测到点击延迟为 2.6–14.1 ms，所有松开均确认。页面共五格选中后，Verify 实际晋级至 `Level 11: Waldo`，后台 override 随后清除。

第 5 关曾在启用后台模式时遇到 CDP 观察超时，返回 `completed=0`；清理和只读复查确认拼图未变后才重试。第 9 关另一次写入在工具传输层超时，未重放输入，随后按累计 stdin 字节数与完整回执确认该程序完成。当前控制 shell 到第 11 关的计时为墙钟 2764.887 秒、Zuse 命令累计 99.258 秒；它从第 9 关期间开始，不是全程耗时。传输等待、模型判断与前一会话的步骤另占时间，当前没有可支持“快速通关”结论的完整墙钟记录。

用户随后要求停止现场操作并集中改进软件，执行会话已停止游戏输入与重连。最后一条第 11 关候选程序在第一步读取页面身份时收到 `Broken pipe`，回执为 `completed=0`，后续点击与 Verify 均未执行；此前最近一次后台关闭已确认。第 11 关不计通过。

本文记录窗口与控件定位、原生剪贴板、截图坐标、鼠标与反馈按键、公开绘制观察、显式后台焦点，以及会话生命周期。接口依据当前源码；现场结果依据主执行会话报告的控制回执。私密对话、桌面截图和临时运行日志不放入源码仓库。

最终验收采用最新的“只靠 Zuse 完成”要求。此前使用 Python 游戏控制器、截图处理或外部剪贴板辅助的步骤只保留为排障历史，不能作为符合该标准的完成证据。下述 Build 5 实测重新使用 Zuse 原生剪贴板、无障碍语义操作和窗口限定键盘完成了多语言链路，没有坐标点击或外部辅助。构建与协议测试用于验证实现，单独列示。

## 窗口限定与无障碍定位

显式指定窗口的文本输入会保留该窗口身份。Hyprland 的 `keyboard.type(text="...",window="实际窗口ID")` 在目标已经聚焦、且原键盘能够恢复时使用 Unicode 路径；发送前复核窗口身份、几何、缩放、焦点与键盘状态。后台输入仍依赖当前键盘映射，不能据接口存在推断任意中文都能送达。源码与边界检查见 [Wayland 实现](../src/platform/wayland.rs)及[原生夹具](../tests/native_wayland.py)。

原生无障碍路径上限提高到桌面根节点以下 64 层，仍保留 1024 字节路径与 4096 节点边界。`accessibility.tree(scope="现场窗口路径",limit=4096,depth=64)` 可限定一个已观察到的窗口；省略 `scope` 保留原入口，默认节点数和深度仍为 256/8。返回的节点列出路径、PID、角色、名称、文本、动作，以及 `enabled`、`focusable`、`focused` 等原生状态；无法读取的状态为 `null`。`depth` 是相对枚举预算，输出路径的全局深度仍受 64 层限制。超过节点、深度、时间或响应大小预算时，必须检查 `truncated` 和 `errors`。依据：[无障碍实现](../src/accessibility.rs)、[目标定位](../src/target.rs)、[目标测试](../tests/target.rs)。

精准操作直接选择枚举到的控件及动作，不依赖像素或节点的 `bounds`。`accessibility.action(path=...,index=...)` 与 `accessibility.focus(path=...)` 可带 `expected_pid`、`expected_role`、`expected_name`，对同一个已解析并持有引用的 AT-SPI 对象作精确身份检查；任一不匹配即返回 `stale_target`，不再输入。显式空 `expected_name` 要求控件无名。这些 guard 当前仅支持 Linux AT-SPI，其他平台带 guard 时明确返回不支持；旧的无 guard 调用兼容。身份检查与应用 UI 变化并非原子事务，界面变化后仍须重新枚举。依据：[控件预检测试](../tests/accessibility_controls.rs)、[原生身份检查测试](../tests/accessibility_guard_native.rs)。

现场已通过 AX action 打开 ChatGPT 模型菜单，读到左右方向键调整说明和五档选项，随后读回中档处于第 2 项、共 5 项；尚未记录逐档切换结果。异步编辑器曾在 300 ms 后仍返回旧文本，因此输入后应等到实际目标文本或控件状态出现，不能把固定等待时间当作成功条件。窗口枚举与身份 guard 的测试已纳入 Build 4，后续语义接口已用于 Build 5 的真实窗口操作。

## 原生剪贴板与多语言发送

Build 5 的真实 ChatGPT 链路使用 Zuse 的 `ext_data_control_v1` 后端读写了 117 字节测试文本。读取时确认当前剪贴板仍等于本轮测试文本，随后原生 `clipboard.write` 成功；这一步没有调用外部剪贴板程序。接口、所有权和有界传输规则见[原生剪贴板说明](native-clipboard.md)。原剪贴板备份的恢复与清理结果尚未在本记录中确认。

控件操作完成了聚焦、替换文本、发送和等待回复。执行顺序为：按名称对“隐藏侧边栏”按钮执行 `press`，聚焦目标窗口，再对“询问 ChatGPT”编辑器执行语义 `focus`，使用窗口限定 `Ctrl+A`、`Ctrl+V` 替换内容，最后通过 `accessibility.find` 精确读回三行中文、英文和 emoji。回读同时确认已去掉先前多余的逗号；此次输入使用原生剪贴板粘贴，不能据此推断所有键盘后端的直接 Unicode typing 都已通过。

发送按钮的语义 `press` 随后成功，`accessibility.wait` 在同一窗口与 PID 范围内等待段落文本 `ZUSE_DESKTOP_OK`，返回 `status=satisfied`、精确匹配 1 项，耗时 3653 ms。这证明本次多语言输入、发送与回复观察链路完成；归档等其他流程仍需单独验收。依据：[语义实现](../src/accessibility/controls.rs)、[语义预检测试](../tests/accessibility_semantics.rs)、[原生剪贴板实现](../src/clipboard.rs)。

后续原生窗口流程另验证了一次直接中文输入：聚焦 ChatGPT 搜索控件后，以窗口限定 `keyboard.type` 输入“桌面验收测试”，无障碍等待精确读回同一文本，耗时 3693 ms。随后对已枚举的搜索结果执行 `select`，打开本轮已有测试会话，并读回测试消息与目标 document。它补充了这条直接 Unicode 输入路径的真实证据，不能泛化为所有键盘模式。归档尝试尚无成功回执。依据：[桌面原生控件证据](evidence/desktop-native-controls-20261122.json)。

## 从截图像素转换到输入坐标

截图的物理像素可能多于输入使用的逻辑单位。本轮浏览器现场曾出现 PNG 为 1225×1417、CSS viewport 为 980×1133 的情况；两个方向分别取整，因此不能把所有图像坐标都直接当作 CSS 坐标。

`browser.capture` 现在返回真实 PNG `width`、`height`、`cssViewport`、`pixelsPerCssUnit` 和捕获时的页面观察信息。映射可用时，转换规则是 `cssX=imageX/pixelsPerCssUnit.x`、`cssY=imageY/pixelsPerCssUnit.y`；上述场景中 x 比例为 1.25，y 比例为 1417/1133。捕获前后会核对文档、viewport、DPR、滚动与 visual viewport；变化时返回 `stale_target`，保留原目标文件。截图可观察后台页面，不要求聚焦。Pinch zoom/pan 的图片仍可保存，但映射为 `null` 并返回 `mappingError`。依据：[浏览器实现](../src/browser.rs)、[截图测试](../tests/support/browser_capture.rs)。

每条新 CDP 连接应先显式设置所需 `browser.viewport`，再使用截图坐标。现场另一条连接曾返回 1225×1417 的 PNG，却观察到 1000×900 的 CSS viewport；直接相除会产生约 1.225 和 1.574 的不一致比例。显式重设 1000×900 viewport 后，现场截图恢复为 1000×900。新增防线允许两轴各一像素取整误差，若仍无法对应同一缩放比例，则保存图片但将映射置为 `null` 并说明重新设置 viewport。这个检查识别已见的表面与 viewport 不一致，不证明所有截图裁切原点都正确；本轮没有把未经现场验证的显式 clip 当作保证。

Hyprland 原有合成桌面区域截取在现场曾返回错误窗口，因此该路径已被禁用。新增原生窗口表面导出实现使用完整 64 位窗口身份，唯一映射后保留同一个 WLR toplevel 对象并通过 Hyprland 协议导出；不能导出时明确失败，不回退到桌面裁切。新回执为 `captureMode="hyprland-toplevel-export"`、`coordinateSpace="window-local"`，`logicalRegion.x/y=0`。随后私有 headless 双窗口门禁 `native_wayland_capture_preserves_window_identity_across_focus_and_close` 已通过：1 passed、0 failed、1 filtered，耗时 1.63 秒，runner 退出 0；隔离检查确认私有 compositor sockets，被检查的 compositor 进程没有物理输入或 KMS 文件描述符。该结果来自纯 Shell/cc 启动的私有 Cage 环境，不操作真实宿主桌面。空 `cleanup.log` 不能单独证明额外清理断言已通过。请求的缩放是 1.25，夹具核验实际图像/逻辑尺寸的比例及焦点切换前后的稳定性，没有断言最终比例必须等于 1.25。Build 11 较早的 2+1 项检查是拒绝不可靠路径的门禁，两者分开记录。两类截图回执都不绑定后续操作：页面、窗口或内容改变后，需要重新观察。实现、证据与不使用 Python 的复跑步骤见[原生窗口捕获](../src/platform/wayland_capture.rs)、[本轮原生截图证据](evidence/native-window-capture-20261122.json)、[私有构建与测试](native-testing.md#window-capture-without-python)。该证据属于基于 `f13412e3c148ecf01c347423caba509d45df9ccc` 的未提交候选，不是正式 release 验收。

## 浏览器鼠标与双方向键

坐标点击和拖动使用真实 CDP 输入事件，适合截图定位的按钮与 canvas：

```text
browser.click_at(x=100,y=120)
browser.stroke(points="100,100;180,100;180,180",duration=300)
browser.hold_keys(keys="ArrowUp,ArrowRight",duration=500)
```

`browser.stroke` 完整预检 2–4096 个 CSS viewport 点，时长为 0–60000 ms；非零时长至少为每段提供 1 ms。路径和时长计入程序总预算。输入要求所选桌面页面可见且聚焦，操作过程中复核文档、几何、滚动与焦点。错误或取消后尝试松开本次按钮；真正断线时明确报告未确认释放。依据：[鼠标实现](../src/browser/pointer.rs)、[运行时与原生浏览器测试](../tests/browser_pointer.rs)。

`browser.hold_keys` 接受最多两个唯一、非相反方向，时长为 1–10000 ms。两个按下请求在同一许可内连续发送，所有松开请求先发送再等待回执；它们仍是独立 CDP 命令。现有单键 `browser.hold` 和原生键盘 guard 保持原契约。该能力为停车关的前进加转向提供输入原语，是否完成停车仍由游戏状态判断。详见[按键保持](key-holds.md)。

最后一次目标观察遇到保持期限时，应先按时松键，再确认仍在途中的观察回复。新增修复仅把这个明确的期限边界延后到有界清理阶段核验；真正的协议错误、断线、目标变化或最终观察无法确认仍返回失败。鼠标路径也区分“期限已过、尚未发送”和真正写帧失败：前者保留健康连接，并在最后已发送位置松开，避免一次未发送的 move 阻断释放。

`browser.hold_until` 把停止条件放入本地观察循环；初始条件为 true 时不按键，超时和错误时仍统一释放。配套 `canvas.watch/read/close` 只记录公开 `drawImage` 参数、source 元数据和绘制变换，不读取游戏变量，也不把 source 当作控件或独立实体。`latest` 是每个 source 的最后一次绘制，条件需考虑 `age_ms` 和覆盖限制。详见[条件按键](key-holds.md#hold-until-an-observed-condition)与[绘制观察](canvas-observer.md)。

主执行会话已在真实停车页面观察到一次以 `x>=100` 为停止阈值的 `hold_until` 成功触发，回执为 `released:true`。这确认了该次条件观察与松键链路。随后用户手动驾驶影响了轨迹，不能把混合轨迹计为 Zuse 自动完成。该轮当时停在第 15 关第二张图，停车与全站终局均未通过。

现场完整历史曾在约 500 次绘制后耗尽 256 KiB，观察停止，也使 `canvas.read` 回执过大。新增 `canvas.watch(...,history=false)` 只保留每个 source 的最新绘制和元数据；同一 source 的替换扣除旧记录字节，`event_count` 累计，`retained_event_count=0`、`history_recorded=false`、`events=[]`。默认 `history=true` 保留原契约。两种模式都保留 64 个 source、256 KiB 和最长 15 秒的边界，删除历史不会使记录成为当前可见性的证明。主执行会话已报告远端 Build 8 的原生 Chromium 定向夹具通过：5000 次重复绘制后仍能记录末次替换，source、字节及 TTL 预算检查通过；canvas 参数预检也通过。这支持该模式的保留策略及限制，不构成真实游戏通关证据。

## 有界反馈按键控制

现场固定 chord 的转向阶段结束后仍有惯性，主执行会话报告最终角度在约 -0.09 至 +0.09 弧度间漂移，位置也随帧率和切换时刻变化。另一次以公开绘制位置估计速度的条件刹车，在观测速度不高于 0.005 像素/ms 时松开 Down 并停住。这些是单次轨迹与停止证据，尚未形成可复验的停车完成流程。

新增 `browser.control_keys(allowed_keys=...,policy=...,timeout=...,canvas=...)` 把反馈策略放入 Zuse 的本地有界循环。允许列表最多四个方向，每轮严格返回 `{keys:[...],done:bool}`；活动按键最多两个，必须唯一且非相反方向，空数组可无键等待。Rust 先松开退出集合的键，确认后再按新增键，不重复按仍保持的键。原有文档、焦点、元素和几何保护保留，后台模式仍需显式开启；异常、取消、期限耗尽或换文档进入同一按键释放路径，断线不自动重放。

策略运行在 isolated world，保留 `throwOnSideEffect=true`。公开 canvas 状态与控制信息先复制成既有只读对象，`memory` 每轮在受检查的 evaluation 内重新 JSON.parse，只保存不超过 4 KiB 的自有 JSON。页面 JavaScript globals 不提供给策略。离线 DSL 检查覆盖类型、权限和总等待预算；策略 JavaScript 在执行时、首次按键前编译并完整检查，不能把这写成离线已解析全部 JavaScript。实现不内置停车路线或车辆模型。API、时限和观察陈旧风险见[反馈控制说明](key-holds.md#adjust-keys-from-observed-feedback)。

Build 11 已通过七项协议/决策回归、两项参数预检及两项 `hold_until` 回归。原生夹具最初在编译策略时返回 `Runtime agent is not enabled`；实现已在选中页面 session 启用 Runtime domain。Build 12 的 250 项 lib 与 11 项集成门禁通过，原生控制器继续暴露该 Chromium 对部分 DOM 读取的 side-effect 限制。Build 13 在真实 Chrome 158.0.8127.36、V8 15.8.62.31 上完成两项原生控制器门禁，耗时 1.70 秒，并构建正式二进制：完整策略包装器、公开 DOM/canvas 读取、fresh memory 及 trusted 输入通过，DOM 和既有 control 写入仍被拒绝。该机 `getElementById(...).textContent` 被拒绝，`querySelector(...).textContent` 可用；不能把所有 DOM getter 都视作无副作用。动态 `eval` 与 `Object.freeze` 没有用于最终策略执行，`throwOnSideEffect` 保持开启。依据：[Build 12](evidence/build12-20261122.json)、[Build 13 原生证据](evidence/build13-native-controller-20261122.json)。这些门禁尚不证明停车或全站通关。

## 显式后台焦点与输入范围

现场部分尝试在用户切回聊天后因游戏 document 失焦而中止。这符合原有焦点保护。新增 `browser.background(enabled=true)` 通过官方 CDP 的实验性 [`Emulation.setFocusEmulationEnabled`](https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setFocusEmulationEnabled) 显式模拟选中页面的 focus/activity；每个新连接默认关闭，`enabled` 必须为布尔值，开启仍需写权限。回执标明 `focus_emulated`、`scope="selected_page_session"` 及启用时的文档身份，不调用切换前台窗口或标签页的动作。实现见 [background.rs](../src/browser/background.rs)。

真实输入仍通过选中 CDP session 发送；document、URL、activeElement、viewport 和 scale 的既有输入校验保留。Zuse Runtime 在每条普通命令前后比对顶层 frame ID、loader ID 与 URL；校验位于保持循环外，避免在松键前额外等待协议往返。正常 Zuse 导航、切页、`close` 和 `Drop` 会向旧页面 session 显式关闭该模式；直接 page WebSocket 的空 target ID 也按持有的连接清理。清理失败会报告未确认，并保留本连接的待清理所有权。成功回执使用 `focus_emulation_cleared=true`，仅表示本连接的 CDP override 已移除，不宣称恢复 OS 焦点，也不要求 `document.hasFocus()` 变为 false。

**空闲期间的外部导航要到下一条 Zuse 命令才检测并清理。** 当前客户端没有后台读线程，不能承诺导航发生时立即恢复；Chromium 的 [Emulation handler](https://chromium.googlesource.com/chromium/src/+/main/content/browser/devtools/protocol/emulation_handler.cc) 会将模式传递到替换 renderer，因此不能依赖导航自动关闭。发现 document 身份变化时，下一条命令尝试关闭模式并返回 `stale_target`。跨命令检查由 Runtime 提供，直接绕过 Runtime 使用底层 Rust Browser 的调用者没有这项自动边界。

后续每次使用该模式的实际运行，主执行会话都会在成功或失败后**另行提交** `browser.background(enabled=false)` 并核对清理回执。同一程序中排在失败步骤后面的关闭命令可能不会执行，不能作为已清理的证据。主执行会话已报告 Build 9 cleanup 的四项 loopback、一项参数预检，以及修订后的原生双页面夹具通过；原生夹具耗时 2.33 秒，二进制构建完成。原生检查覆盖选中目标的 trusted 输入与完整 keyUp、另一页未收到输入、无输入开关、实际 visibility 基线，以及导航和切页清理。早先的失败断言要求关闭后立即 `hasFocus()==false`；官方 [InputHandler](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/content/browser/devtools/protocol/input_handler.cc) 显示输入注入会调用 `RenderWidgetHost::Focus()`，清除 override 不撤回这个内部焦点状态。修订夹具只在自身两页之间制造真实 blur，确认没有残留 override；生产清理不抢占其他用户窗口。现场背景路线随后连续执行约 7 秒，未因用户在聊天而触发失焦中止，游戏仍为第 15 关第二张图。不能由此推断完整游戏通过，或同一页面上多个控制器的未知既有模拟设置能被恢复。边界与使用方法见[后台模式说明](key-holds.md#explicit-background-focus-for-the-selected-page)。

## Hyprland 鼠标事件帧

仅改变合成器指针坐标，不足以保证应用收到完整的鼠标事件。本轮在精确 warp 后补充零相对位移和 Wayland pointer frame，由合成器继续负责坐标变换；不为补帧额外点击或产生非零相对位移。能力依赖 `zwlr_virtual_pointer_manager_v1`，会检查唯一 seat，并使用有期限的同步；失败后应重新观察实际指针位置。依据：[pointer frame 实现](../src/platform/wayland_pointer.rs)。早期 Python 指针追踪启动器是人工诊断探针，已从本轮交付移除，不作为符合纯 Zuse 要求的验收证据。

## 通用源码示例与交付范围

[浏览器控件示例](../examples/browser-visible-controls.zuse)提供现有 page 连接、可见控件等待点击与条件松键的参数化函数；[桌面语义控件示例](../examples/desktop-semantic-controls.zuse)提供限定窗口枚举、预期文本确认后的替换与回读、授权动作后的结果等待。文件只有函数声明，导入不会启动浏览器或发送输入，不包含游戏答案、现场窗口 ID、PID 或聊天内容。格式整理后的 CLI 已分别执行 `zuse check`，两份源码均返回 `valid`、`steps=0`。这确认离线检查接受声明，不代表调用函数体或执行真实 UI；新增组合源码不能另记一项端到端通过。检查范围见[代码门禁记录](evidence/code-hardening-20261122.json)。

本轮两个早期 Python 游戏控制器与一个 Python 指针诊断启动器已移除。既有 `tests/native_wayland.py` 保留其回归测试职责；本轮增加的显式窗口 Unicode、多键盘映射批次恢复、原生窗口表面回执检查及私有输出 marker 检查仍对应现行实现。保留测试源码不等于把 Python 辅助流程计入最终纯 Zuse 验收。公开证据中的开发机 home 路径与临时 client/job 标识使用稳定别名，测试数字、版本、源码 SHA 与错误的关键语义保留。

## 长会话与浏览器 owner

调用 `browser.launch` 的 Runtime 拥有浏览器进程和临时配置。它退出或释放资源时，其他 CDP 连接也会失去该浏览器；增加附着连接不能代替保持 owner 存活。本轮一次断线已定位为 owner shell 达到默认 15 分钟空闲期限，不是已经证实的 CDP idle 心跳缺陷。长验收应在启动 owner 时选择覆盖任务的期限，例如：

```sh
zuse shell --idle-timeout-ms 14400000
```

示例为四小时空闲期限。宿主任务仍需保持运行；受宿主进程回收约束的环境可使用有明确期限的 `session serve` 长期任务。正常结束后再释放拥有的浏览器。依据：[shell 生命周期](../src/main.rs)、[会话说明](sessions.md)、[浏览器关闭语义](../src/browser.rs)。

通过 `/devtools/page/...` 附着的 Runtime 不拥有页面，可以显式 `browser.close()` 后重新 `browser.connect()`；不要在仍需保留游戏状态的 owner 上执行关闭。写帧失败或收到终态关闭后，连接会拒绝后续帧，避免向可能残缺的协议流追加数据。动作结果不明时先重新观察，不自动重放。

## 固定提交的代码门禁与消费者兼容性

停止真实 UI 操作后，工作转为实现复核、编译和可隔离的测试。Build 15b 的全部测试源码编译成功，但首次 all-targets 执行在库测试阶段以退出码 101 结束：261 passed、9 failed、8 ignored。九项失败均在 Unix socket 夹具的 `UnixListener::bind` 处直接收到 `EPERM`，原始失败名称和日志 SHA 保留在[代码门禁记录](evidence/code-hardening-20261122.json)。这是环境阻断的失败记录，不能改写成该次全量门禁通过。

最终格式与 Clippy 整理后，沙箱复跑库测试得到 261 passed、0 failed、8 ignored、9 filtered；过滤的正是上述九项 Unix socket 夹具。另执行 59 个集成目标，得到 300 passed、0 failed、27 ignored；整个 `sessions.rs` 目标的 11 项 Unix socket session 测试未在沙箱运行。候选提交固定后，补跑 `cargo test --offline --locked --bin zuse -- --test-threads=1`，得到 12 passed、0 failed、0 ignored，退出 0。该轮库、CLI 二进制与集成合计 **573 项通过（261 + 12 + 300）**，三组互不重叠；9 项过滤、11 项未运行和 35 项忽略均未计入通过数。较早 18 个集成目标的 64 项通过是子集，不再相加。文档测试命令退出 0，但其中没有测试；这组沙箱结果不是完整 all-targets 通过。具体日志及其 SHA 见[代码门禁记录](evidence/code-hardening-20261122.json)；后续开发机结果单独列示。

`cargo fmt --check`、`no_std` 检查、CLI 构建，以及 Clippy 1.100 的 `--all-targets -- -D warnings` 均通过；其中已记录的沙箱 Clippy 运行耗时 10.76 秒。已计入库测试的八项 AT-SPI ABI mock 和两项许可准备/取消回归覆盖许可后对同一保留对象复核、动作索引变化、提交诊断和不重放边界，不能当作新的真实桌面验收。候选源码固定为 commit `14c413dc385e076c6e0ecbd9002165a1acd68518`，tree `f86332e1688c4a7f5f572a3f1dc6363aba1721b3`。

该源码提交已推送到官方 GitHub 分支 [`work/zuse-control-hardening-20261122`](https://github.com/LIghtJUNction/zuse/tree/work/zuse-control-hardening-20261122)。主执行会话随后从沙箱独立执行 `git ls-remote --exit-code origin refs/heads/work/zuse-control-hardening-20261122`，退出 0，读回与上述完整 commit 相同的 SHA。此项仅记录源码分支发布；没有创建 PR，未发布 release，也未安装候选二进制，不据此宣称已合并 main。远端 ref 的核验回执见[代码门禁记录](evidence/code-hardening-20261122.json)及[固定提交原生门禁证据](evidence/native-code-gates-20261122.json)。

开发机在该提交的干净 fresh worktree 上完成复验，runner 状态为 `passed`、整体退出 0；fmt、no_std、完整 Rust、Clippy、CLI 构建及五项隔离 headless Chromium 门禁均退出 0。完整 Rust 日志包含 63 个测试汇总：库 270、CLI 二进制 12、60 个集成目标 311 项通过，合计 **593 passed、0 failed、35 ignored、0 filtered**；另一个汇总是 0 项文档测试。此前九项 Unix socket 夹具逐项通过，`sessions.rs` 的 11 项测试也全部通过，已关闭这两组环境阻断。原沙箱失败与过滤记录仍保留。开发机 Clippy 运行耗时 11.91 秒。日志、目标计数和源码身份见[固定提交原生门禁证据](evidence/native-code-gates-20261122.json)。

五项指定 Chromium 门禁分别覆盖控制器只读能力、选中页面激活、JPEG 完整 viewport 尺寸、原生 PNG 浏览器夹具，以及 DOM/canvas 反馈控制与 fresh memory，每项 1 passed。已逐项确认它们在上述完整 Rust 运行中属于 ignored，因此这批固定提交的**独立通过总数为 598（593 + 5），剩余 30 项 ignored**。定向调用为选择单项测试而产生的 `filtered out` 不代表新增遗漏，也不计作通过；沙箱 573 项与此前不同源码的原生窗口截图证据均不再相加。

该批 Chromium 为 158.0.8127.36，使用自有临时 profile 与 data/file 私有夹具；DISPLAY、WAYLAND_DISPLAY、AT_SPI_BUS_ADDRESS 未设置，DBus 指向不存在的私有地址，XDG_RUNTIME_DIR 为私有目录。测试未恢复真实游戏或桌面操作。候选 CLI 仍显示 `zuse 0.6.0`，已记录二进制 SHA，未安装或发布。公开记录核对了 20,377 字节归档及其中全部 16 份原始文件的 SHA，只保留脱敏结果，不提交原始日志、归档或二进制。用户 STOP 后的真实验收边界不变：纯 Zuse 游戏通过第 1–10 关、最后观察为第 11 关，归档成功数仍为 0。

消费者方面，仅 ztype 完成了临时本地 path 依赖夹具的部分兼容性检查。固定 ztype 源码为 `5d245e5b0633b5ce0f9479fec9741fb23b7b5b80`，配置为 `--no-default-features --features desktop`，验证了候选 `zuse::platform::Desktop` API 的编译。两次调用合计 65 passed、3 failed、1 ignored：主 all-targets gate 仍退出 101；随后单独执行 composition、dictionary 等集成目标，退出 0。两项服务测试直接报告 Unix datagram socket 的 `EPERM`；另一项错误文本断言失败的实际返回值未打印，只能将其与同类 socket 限制的关系记为推断。plugin feature 未开启，其零测试结果不计作覆盖。原锁文件已恢复，消费者工作区保持干净，没有正式传播或 push。原始回执和两份日志的 SHA、候选源码清单 SHA 见[ztype 兼容性记录](evidence/ztype-compatibility-20261122.json)。

[消费者传播准备记录](evidence/consumer-propagation-20261122.json)因此仅将 ztype 标为部分兼容性已测、`validation_complete=false`、`propagated=false`；其他五个消费者与 Project Z 聚合记录仍未做本轮候选验证或传播。临时 path 候选的源码清单标识只证明当时的快照，不能替代本次固定 commit 的兼容性结果，也没有其他维护者承诺。

## 本轮证据与待完成事项

下表区分早期现场与协议门禁、停止 UI 后的沙箱与固定提交开发机门禁，以及消费者检查。文档整理核对已有日志与回执，没有重复执行产品测试。固定提交仍有 30 项 ignored；代码门禁不替代暂停中的真实应用验收，也不构成发布完成。

| 项目 | 已确认结果 | 结论范围 |
| --- | --- | --- |
| Build 3 基线 | 库测试 209 通过、4 忽略；target 17、hold_keys 预检 1 通过 | 忽略项不算执行通过 |
| 真实 Chromium key hold | 2 通过 | 已执行的单键及双键重叠、取消释放夹具 |
| Build 4 | 库测试 212 通过、4 忽略；另 23 项定向测试通过 | 23 项为 accessibility_controls 5、hold_keys 1、target 17；包含 pointer、hold 边界和截图比例回归 |
| Build 5fix 定向门禁 | clipboard 单元 3、semantic 5、clipboard 集成 1 通过 | 只记录这批已报告执行项，不推断未重跑的全量库测试结果 |
| CLI 构建 | Build 4 与 Build 5fix 均完成 | 不据构建推断实际应用结果 |
| 官方游戏 | 早先运行到第 15 关第二张图；本次纯 Zuse 重跑通过第 1–10 关，最后观察为第 11 关 | 两段记录分开；全通关和“快速”验收未完成 |
| 原生窗口表面截图 | 私有双窗口门禁 1 passed、0 failed、1 filtered，1.63 秒；runner 退出 0 | 当时未提交候选的独立历史证据，不加进当前 598；请求 scale=1.25，实际比例与稳定性通过检查，但未证明最终比例恰为 1.25；空 cleanup 日志不补充额外断言 |
| 纯 Zuse 剪贴板与编辑框 | 原生读取、写入 117 字节；三行中文、英文、emoji 精确读回 | 同一真实 ChatGPT 窗口；未使用坐标或外部粘贴辅助 |
| 纯 Zuse 发送与回复 | 语义发送成功；3653 ms 后精确读到 `ZUSE_DESKTOP_OK`，1 项匹配 | 本次文本链路通过，不能泛化为所有桌面功能通过 |
| ChatGPT 直接中文输入与搜索 | 窗口限定 keyboard.type 输入中文，3693 ms 后精确读回；select 打开已观察到的本轮测试聊天 | 只证明该输入与搜索流程，未覆盖所有键盘模式 |
| ChatGPT 菜单与归档 | 中档为第 2 项、共 5 项已读回；归档尝试仍无成功回执 | 未确认逐档切换或归档结果 |
| hold_until 与 canvas 组合 | 真实停车页面一次 `x>=100` 条件触发，`released:true` | 仅确认该次条件松键；用户后续手动驾驶不计作 Zuse 自动完成 |
| canvas 最新状态模式（Build 8） | 原生 Chromium 的 5000 次绘制、末次替换、source/字节/TTL 预算及 canvas 参数预检通过 | 只确认已执行的定向门禁，不等于游戏通过 |
| 显式后台焦点（Build 9 cleanup） | loopback 4、参数预检 1、修订后的原生双页面夹具 1 通过；二进制构建完成；现场背景路线约 7 秒无失焦中止 | 关闭仅确认清除 override；空闲外部导航到下一命令才检测清理；游戏仍未通关 |
| control_keys 反馈控制（Build 13） | 两项真实 Chromium 控制器门禁通过，1.70 秒；正式 CLI 构建完成；保留 side-effect guard | 公开 DOM getter 支持有实测边界；不等于停车或全站通过 |
| Build 15b 首次 all-targets | 全部测试源码编译成功；库 261 passed、9 failed、8 ignored，整体退出 101 | 九项 Unix socket 夹具直接报告 EPERM；保留原失败，不据此称全量通过 |
| 沙箱门禁历史 | 库 261、CLI 二进制 12 与 59 个集成目标 300 项通过，合计 573，所选命令退出 0 | 该轮 9 filtered、11 项 session 未运行、35 ignored；后续开发机已补过前两组，573 不与新结果相加 |
| 固定提交完整 Rust 门禁 | 库 270、CLI 二进制 12、60 个集成目标 311，合计 593 passed、0 failed、35 ignored、0 filtered；整体退出 0 | 原九项 Unix socket 失败与 11 项 session 全部通过；63 个汇总包括 0 项文档测试 |
| 固定提交指定 Chromium 门禁 | 控制器能力、activate、JPEG、PNG、反馈控制各 1 项通过，runner passed | 五项此前均 ignored；独立通过总数 598，剩余 30 ignored；不加沙箱或旧源码截图结果 |
| 格式、静态检查与声明示例 | 沙箱与开发机 fmt、no_std、CLI 构建、Clippy 均通过；两份示例离线检查均 valid、steps=0 | 示例函数未调用；CLI 仍为 0.6.0，未安装或发布 |
| ztype 消费者兼容性 | desktop-only 临时 path 夹具编译成功；两次调用合计 65 passed、3 failed、1 ignored | 主 gate 退出 101；直接 EPERM 两项、相关推断一项；原锁恢复，尚未正式传播或完整验证 |

### 草案自检

- **贡献是否明确？** 各项改动对应现场坐标、输入或生命周期问题，未宣称完整解题能力。
- **说明能否复现？** 给出接口、单位、限制和对应源码；窗口与节点 ID 必须现场获取。
- **实验是否足够？** 固定提交在开发机独立通过 598 项，已补齐沙箱 Unix socket 与 session 阻断；仍有 30 项 ignored，完整消费者验证与暂停中的游戏、桌面终局验收仍缺证据。
- **覆盖是否完整？** 不完整；未给出符合新标准的全程通关耗时、归档或原剪贴板恢复结果。
- **设计边界是否说明？** 取消不能撤销已提交动作，截图映射不锁定未来输入，失去传输时不能承诺松键已送达。
