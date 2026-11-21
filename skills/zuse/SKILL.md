---
name: zuse
description: 用 Zuse CLI 操作电脑、浏览器和画板，把观察、计算、输入与等待写成有界程序。适用于直接执行 Zuse、分形绘画、窗口锚点、常驻会话、外部控制循环、复用函数与计时。
---

# Zuse 电脑操作

先观察明确目标，再把同一阶段的操作合成一段程序。保留失败步骤和已完成动作，按应用结果判断任务是否完成。

## 直接执行

优先用已授权的 ssmcp 执行 Zuse CLI；遵循实际工具权限与用户指定的输入通道。
从 `zuse doctor`、`zuse schema` 读取当前能力，再观察窗口。不要用版本号猜后端。

```sh
zuse doctor
zuse -c 'window.list(); keyboard.state()'
zuse -c 'timer.start(); wait(ms=50); timer.stop()'
```

把短程序直接传给 `-c`，多行源码交给 `zuse script -`，共享时再保存 `.zuse` 文件。
用 `--dry-run` 预检。坐标计算使用不可变 `let`、算术、`if`/`else`、半开 `for`、纯 `math` 与递归，
保持在 Zuse 内展开；每个循环最多 4096 次、调用深度 32、展开工作 16384 次。见[数值程序](../../docs/scalar-programs.md)与[脚本模块](references/scripts.md)。

## 常驻会话与控制循环

跨工具调用时复用一个 `zuse session start board --idle-timeout-ms 900000`。
通过 `zuse session observe board -` 提交只读源码，查询 `zuse session result board JOB_ID`；
取得终态观察结果后，在外部决定下一步，再用 `zuse session script board -` 提交短程序。
保存每次 `job_id`，查结果、确认应用状态，再开始下一轮；`accepted` 仅表示已接收。

用 `zuse session status board` 查询、`zuse session cancel board` 请求取消、`zuse session close board` 关闭；运行中提交新作业返回 `busy`。
取消后继续查结果，关闭时检查 `resources_released`。重试保留同一 `--request-id` 和原请求，
用 `--expect-revision` 检查 Zuse 会话修订号，应用内容另行观察；去重缓存有界。外部客户端按[公开协议 v1](../../docs/session-protocol.md)连接 `start` 返回的 Unix socket。
该通道执行 flat 与展开后的数值程序；scoped mouse 仅作 dry-run 预检，实际执行使用 `zuse script`。
默认空闲 15 分钟释放状态，运行时间不计空闲；单一 stdin 流可用 `zuse shell`，`:quit` 结束。见[会话说明](../../docs/sessions.md)。

## 鼠标与锚点

默认独立鼠标要求可用的 X11 MPX 后端。要操作系统指针，显式调用
`mouse.use(kind="user",appearance="system")`，不要静默降级或猜坐标。
普通点移动与 `mouse.click_at` 采用默认 350 ms、`curve="smooth"`、`jitter=0.6`；
提速优先合并调用，`duration=0` 仅用于明确选择的瞬移操作，绘图描边沿原轨迹执行。
从真实窗口信息和截图映射获得 ID、缩放和局部区域，再替换示例参数：

```zuse
mouse.use(kind="user",appearance="system")
window.focus(id="OBSERVED_WINDOW_ID")
wait(ms=200)
anchor.set(name="board",window="OBSERVED_WINDOW_ID",x=160,y=180,width=640,height=480)
draw.triangle(anchor="board",x1=40,y1=120,x2=160,y2=40,x3=280,y3=120,duration=240)
```

锚点按同一窗口的身份、焦点、几何和缩放校验。遇到 `stale_anchor` 时读失败原因；
明确恢复已知窗口的焦点，几何改变则重新观察和绑定，不按相似标题换窗。
锚点绑定原生窗口，同窗标签页共用 ID；DOM、标签页及页面内滚动/缩放后的内容需重新观察。
发现用户切页就停止后续输入，取消在跑的批次，再确认目标。操作前释放实际残留的修饰键。

## 批处理与确认

按颜色、图层或界面阶段合并点按和笔画，复用 `mouse.click_at`、`mouse.stroke` 与 `draw.*`；`draw.triangle` 用三个数值顶点完成一笔闭合描边。
把截图放在批次边界或失败之后，避免每个点都观察。只在需要图像时使用截图或模板匹配。
优先用 `observe`、局部 `watch` 和有超时的 `wait_until` 等待状态；已有订阅继续复用。
`watch_until_click` 才会把观察连接到输入，显式给出目标、次数和超时。
以应用观察确认输入效果；失败后核对已完成步骤，避免重复绘制或重复提交。

用 `timer.start()` / `timer.stop()` 测量同一会话，依据实际耗时优化工具往返与输入节奏。
只执行用户已授权的行为；导入发送函数不等于获准发送消息，现有授权无需反复询问。

## 复用

导入只注册声明，显式调用才执行。找现成包用 `$zuse-find-package`，开发发布用 `$zuse-publish-package`。
本地函数例子见 [assets](assets)；平台、观察等待和视觉时间切片见 [运行说明](references/runtime.md)。
