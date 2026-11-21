# 运行与能力边界

## 命令模式

`.zuse` 是推荐后缀，执行器不依靠后缀猜测授权。`zuse schema` 列出注册的命令语法和参数，不代表所有 feature 或后端可执行；结合 `zuse doctor`、预检和命令返回的 unsupported 状态判断实际能力。`zuse eval`、`zuse run` 和持久 `zuse shell` 共享运行时命令。`zuse script` 还支持拥有或借用鼠标的作用域程序；模块语法见 scripts.md。

```text
timer.start()
observe(source="browser", selector="#conversation")
watch(source="browser", selector="#conversation", name="chat")
wait_until(watch="chat", condition="text", text="ready", timeout=5000)
timer.stop()
```

浏览器命令先要求显式连接或启动。`browser.launch(headless=true)` 只适合新建自有测试页。需要使用现有标签页时，优先使用 ssmcp 明确支持的既有 tab 入口，不为便利创建、激活或切换用户标签页。

## 本地动作与证据

`watch_until_click(source="browser", scope="#board", selector=".ready", max_clicks=1, timeout=5000)` 是显式动作命令。普通 observe/watch 只读。简单复合 selector 可组合 tag、id、class 与属性；复杂 CSS 关系不受支持时明确报错。

运行回执记录状态、已完成步骤、错误与 timing。对应用成功的判断应来自应用文本、状态事件或其可信点击回执；`submitted` 仅说明底层接受了输入。用自有页面或窗口测量延迟，不把小样本 headless 数据泛化到真实桌面或网络。

## 键盘

`keyboard.state()` 分开给出当前键盘和其它设备。其它设备的 Num Lock 开启不能阻塞当前键盘的安全输入。文本输入不能为通过前置检查而更改锁状态。Num Lock 的明确操作使用 `keyboard.num_lock(state="on")`、`off` 或 `toggle`，检查真实 readback。

Hyprland 对实际物理设备的锁设置还需要可用的 evdev 路径和权限；虚拟设备无法映射时应返回不支持。X11 检查核心键盘的状态，不把它描述为所有物理键盘的逐设备状态。

## 鼠标与触摸板

Wayland 的 glow 外观是共享用户鼠标的操作点标记，要求 layer-shell、xdg-output 和 ARGB8888；它点击穿透且不取得键盘焦点。它不是独立输入设备。真正独立多鼠标只在受支持的原生 X11 MPX 后端使用。

当前触摸板原生派发不可用时应返回 unsupported。可验证规划器的缩放累计、取消终点和重新开始，但不能用滚轮或浏览器 zoom 伪装原生 pinch。

## 可选图像功能

使用 `--features vision` 构建才启用模板定位与 timeline 命令。Linux PNG 解码依赖系统 libpng 1.6；不为一次任务自动安装系统包。匹配采用完整、有预算上限的多尺度 NCC；预算耗尽是失败，不得返回未经完整搜索的唯一目标。

```text
target.locate(template="assets/send-icon.png", window="active", name="send")
```

返回候选、分数和原生桌面坐标。当前图像目标的点击前检查需要原生窗口命中支持；无法证明命中时应停止。首次模板匹配后只比较保留位置的小块像素与窗口元数据，不再运行第二轮匹配。图片资源必须由用户明确提供或授权保存。

```text
timeline.start(window="active", columns=3, rows=3, name="board")
timeline.wait_until(name="board", timeout=5000, interval=32)
timeline.observe(name="board")
timeline.render(name="board", path="board-timeline.svg")
timeline.close(name="board")
```

视觉 timeline 是明确选择的本地取帧路径。首帧作为固定背景，连续多帧只记一次出现；消失再出现才是新 episode。默认回结构化文字，render 才写 SVG。越晚出现颜色越深，同格重复以编号与时间区分。记录取样时间边界，不把可见帧数或持续时间当作出现次数。

`start` 只采 baseline，`sample` 每次采一帧，`wait_until` 采样至首次出现后返回，`observe` 只读已记录数据；没有后台持续采样。要记录后续消失和重现，继续调用 sample 或后续 wait。interval 是每轮处理后的等待时间，实际间隔包含截图和处理耗时。窗口变化会拒绝当前采样，但保留旧结果；更换观察范围时显式 close/start。

## 分享

分享 `.zuse` 源码和必要说明。删除联系人、聊天内容、会话端点、凭据、截图与机器绝对路径。用户在技能页面使用分享功能；不要擅自发给第三方。模块导入使用显式文件路径或显式源码，导入阶段不执行发送动作。
