# 绘图原语与窗口锚点

用一段 Zuse 程序完成一组点按和笔画，可以减少工具往返与重复坐标换算。
`mouse.stroke` 负责整条折线的按下、移动和抬笔；窗口锚点保存已经观察过的局部区域。

## 直接执行

`-c` 与 `eval` 使用同一个执行器，接受完整脚本，无需 Python 包装或临时文件。
先检查可用后端，再观察目标窗口：

```sh
zuse doctor
zuse -c 'window.list()'
```

从结果中复制画板所在窗口的明确 `id`，确认它已获得焦点，并观察画布相对窗口的区域。
下面的 `WINDOW_ID`、区域位置和尺寸都是待填写的示例参数；替换为实际观察结果后执行。
先在画板中选好画笔和颜色，脚本将点击一个点并画两条折线。

```sh
zuse -c '
mouse.use(kind="user", appearance="system")
anchor.set(name="board", window="WINDOW_ID", x=160, y=180, width=640, height=480)
mouse.click_at(anchor="board", x=40, y=40)
mouse.stroke(anchor="board", points="40,120;160,40;280,120", duration=240)
mouse.stroke(anchor="board", points="40,160;160,80;280,160", duration=240)
anchor.clear(name="board")
mouse.release()
'
```

在命令末尾加 `--dry-run` 可检查语法、参数和静态预算；该模式不读取真实窗口或执行输入。
单次 `-c` 退出后会结束本次会话；后续命令需要重新选择鼠标和设置锚点。

## 常用原语

| 调用 | 作用与默认值 |
| --- | --- |
| `anchor.set(name,window,x,y,width,height)` | 保存明确窗口及其局部区域；`x`、`y` 默认 0，其他参数必填。 |
| `anchor.clear(name)` | 移除当前会话中指定名称的锚点。 |
| `mouse.click_at(x,y,anchor,button,settle,duration,curve,jitter,seed)` | 默认用 `duration=350`、`curve="smooth"`、`jitter=0.6`、`seed=1` 移动后点按；`button="left"`，`settle=30`，`anchor` 可省略。 |
| `mouse.stroke(points,duration,anchor,button,settle)` | 按顺序发送折线顶点并抬笔；`duration=300`，`button="left"`，`settle=30`。 |
| `draw.line(x1,y1,x2,y2)` | 描出起点至终点的线段。 |
| `draw.triangle(x1,y1,x2,y2,x3,y3)` | 用三个数值顶点描出闭合三角形，适合递归生成图案。 |
| `draw.rect(x,y,width,height)` | 从左上角出发描出闭合矩形；宽高必须为正数。 |
| `draw.circle(x,y,radius,steps)` | 以 `x,y` 为圆心描圆；半径必须为正数，`steps=64`。 |
| `draw.polygon(points)` | 描出至少三个顶点组成的多边形，自动补齐闭合点。 |
| `mouse.move(x,y,duration=0)` | 立即移动，只发送目标点一次，回执为 `samples=1`。 |

普通 `mouse.move(x,y)` 默认也为 350 ms、smooth 加减速和 0.6 轻微抖动；终点精确到达。
`mouse.click_at` 支持显式 `duration=0`，以及 `jitter=0`；先完整检查移动路径，再移动和点按。

五个 `draw.*` 原语共用 `mouse.stroke` 的输入、取消和抬笔路径，均支持可选的
`anchor`、`duration=300`、`settle=30`、`button="left"`，只描边，使用应用当前画笔和颜色。
坐标与尺寸允许有限小数；圆的 `steps` 为 3–4095 条线段，实际点数为 `steps+1`。
闭合点计入 4096 点上限；圆精确复制起点闭合，多边形已有闭合点时不会再追加。
例如 `draw.circle(anchor="board",x=160,y=100,radius=40,duration=160)`。
原有 `mouse.circle` 仍只移动指针；需要按住画笔描圆时使用 `draw.circle`。

表中的参数使用 `name=value` 形式。时间单位为毫秒，`settle` 允许 0–1000。
点按默认移动 350 ms 后等待一次 `settle`，合计预留 380 ms；折线在移动到起点后、按下后各等待一次。
这些等待和 `duration` 一起计入程序的 `--max-wait-ms` 预算。

`points` 使用分号分隔坐标对，例如 `"10,20;80,40;120,10"`，允许小数。
每条折线接受 2–4096 个点，每个程序最多 65536 个绘图点；执行前检查整条路径。
非零 `duration` 必须至少为线段数对应的毫秒数；`duration=0` 显式选择立即提交全部顶点。
顶点按顺序发送，程序不会用一条起终点连线替换中间顶点。

折线完成后释放按钮；按下或移动失败、执行被取消时也会尝试释放，并报告清理失败。
如果当前鼠标已经持有该按钮，点按和折线会拒绝接管。
`delivery="submitted"` 记录输入已提交；画作是否完成仍需读取应用状态或查看画面。

## 锚点与坐标

锚点的 `x,y,width,height` 使用窗口内部的原生坐标单位。
带 `anchor` 的点按、折线和图形使用该区域左上角为原点；省略时使用桌面坐标。
截图像素与原生坐标可能受缩放影响，填写参数时应依据当前窗口几何和截图映射换算。
局部点必须满足 `0 <= x < width`、`0 <= y < height`。

每次带锚点的操作开始前，Zuse 检查同一个窗口的 ID、PID、焦点、位置、尺寸和缩放。
窗口移动、缩放或失焦会返回 `stale_anchor`；重新观察后再设置锚点，程序不会自动换窗。
进入运行阶段后，同名锚点的替换若失败，旧值也会被移除。

锚点保存窗口及区域的位置，不追踪网页画布内部的平移、缩放、滚动或遮挡。
发生这些变化后应重新观察区域；窗口检查也不构成整个笔画期间的连续跟踪。
这适合位置稳定的绘图批次，语义控件定位仍使用 `target.locate` 等已有接口。

## 持久 shell 与函数复用

`zuse shell` 保留鼠标、函数和锚点，适合同一画板的连续操作。
每一行必须是一段完整程序；上一行结束后，下一行可以复用已经注册的函数。

```sh
zuse shell --idle-timeout-ms 900000
```

在该 shell 中先输入一行完成观察后的初始化和函数注册，再输入第二行复用。
同样先将 `WINDOW_ID` 和区域参数替换为实际观察值：

```text
mouse.use(kind="user",appearance="system"); anchor.set(name="board",window="WINDOW_ID",x=160,y=180,width=640,height=480); fn line(points){mouse.stroke(anchor="board",points=points,duration=240)}
line(points="40,120;160,40;280,120"); line(points="40,160;160,80;280,160")
```

默认空闲超时为 `900000` ms，即 15 分钟；此处 `--idle-timeout-ms` 控制 shell，必须为正整数。
计时发生在等待下一行输入期间，命令执行时间不计入空闲时间。
超时或输入 `:quit` 后会话结束，保留的函数、锚点和鼠标资源随之释放。

## 操作前检查与结果确认

先释放来自键盘或其他控制程序的 Super、Alt、Ctrl、Shift 等修饰键，再开始绘画。
外部残留的 Super 状态可能使窗口管理器接管拖动，导致画板没有收到预期笔画。
支持键盘状态检查的后端可用 `zuse -c 'keyboard.state()'` 辅助观察。

按颜色或图层组织一次内联调用，在批次之间确认画面，避免每个顶点都请求截图。
用 `timer.start()` 和 `timer.stop()` 查看执行耗时；异常时读取失败步骤及已完成动作，
确认画板现状后再继续，避免重复提交已经画出的笔画。

接口以 `zuse schema` 为准。相关实现位于 [drawing.rs](../src/drawing.rs) 和
[runtime.rs](../src/runtime.rs)，路径、锚点和取消行为见 [绘图测试](../tests/drawing.rs)。
