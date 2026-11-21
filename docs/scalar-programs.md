# 用数值、分支和循环生成操作

Zuse 可以先计算坐标、选择分支和展开循环，再执行得到的电脑操作。
这让分形、重复布局和参数化轨迹直接写在 `.zuse` 中，不必用外部程序生成长坐标表。
准备阶段没有桌面输入；完整展开程序通过类型、步数和等待预算检查后才开始执行。

## 变量与表达式

用 `let` 绑定一个不可变值。数值、字符串和布尔值可以传给函数与原语；
同一作用域不能重复声明或重新赋值，内层块可以遮蔽外层同名变量。
函数通过参数取得值，不捕获调用方或顶层变量。

```zuse
let width = 650
let half = width / 2
fn edge(canvas, x, y, length) {
    draw.line(anchor=canvas,x1=x,y1=y,x2=x+length,y2=y,duration=80)
}
edge(canvas="board",x=50,y=590,length=half)
```

`+`、`-`、`*`、`/`、`%` 使用通常的算术优先级；括号改变组合方式，负号可作用于表达式。
数值是有限浮点数，不能把字符串自动转成数值。除以零、余数除以零、无穷大和非法数学结果会拒绝程序。
运行时要求整数的参数仍要求整数结果，例如 `wait(ms=2.5)` 不会自动取整。

| 表达式 | 作用 |
| --- | --- |
| `math.pi()` | 圆周率。 |
| `math.sin(x)`、`math.cos(x)` | 弧度制三角函数。 |
| `math.sqrt(x)`、`math.abs(x)` | 平方根和绝对值。 |
| `math.floor(x)`、`math.ceil(x)`、`math.round(x)` | 向下、向上和最近整数取整。 |
| `math.min(a,b)`、`math.max(a,b)` | 两个数的最小值和最大值。 |

数学函数只接受位置数值参数。它们是准备阶段的纯计算；`window.list()` 等观察或操作命令
不能作为表达式返回值赋给变量。需要等待应用变化时仍使用 `watch`、`wait_until` 等运行时原语。

## 分支与有界循环

比较运算 `==`、`!=`、`<`、`<=`、`>`、`>=` 产生布尔值。`if` 要求布尔条件，
括号可省略，`else` 可省略；准备阶段只展开选中的分支。

```zuse
let detailed = 3 > 2
if detailed {
    wait(ms=20)
} else {
    wait(ms=5)
}
for i in 0..4 {
    let x = 40 + i*60
    draw.circle(anchor="board",x=x,y=80,radius=20,duration=80)
}
```

`for i in start..end` 使用整数半开范围：包含起点，不包含终点，依次递增。
每个循环最多 4096 次，空范围不执行；循环变量及循环体内的局部变量不会泄漏到外部。
现有 `repeat N { ... }` 保持常量重复语义。当前不提供可变赋值、`while` 或函数返回值。

## 用递归画分形

递归函数可以根据数值参数终止。示例模块
[fractal-sierpinski.zuse](../examples/fractal-sierpinski.zuse) 仅定义函数，导入不会画图。
每一层把三角形分成左下、右下和上方三个一半尺寸的子三角形；深度为零时描出一个完整三角形。
代码中的加减和除法由 Zuse 计算。

`draw.triangle(x1,y1,x2,y2,x3,y3)` 接受三个数值顶点，并复制第一个点闭合。
它与 `mouse.stroke` 共用锚点检查、点数预算和失败抬笔逻辑，支持
`anchor`、`duration=300`、`settle=30`、`button="left"`。每个三角形是一次连续笔画，
只描边，使用应用当前的颜色和笔宽，不填充内部。

先做不发送输入的预检：

```sh
zuse script - --dry-run --max-steps 4096 <<'ZUSE'
import(path="examples/fractal-sierpinski.zuse",as="fractal")
fractal.sierpinski(canvas="board",x=50,y=590,width=650,height=560,depth=5)
ZUSE
```

实际绘制时，先观察画板窗口、缩放和画布区域，选好画笔，并在同一程序中设置 `board` 锚点。
函数的 `x,y` 是底边左端，`width` 是底边长度，`height` 向屏幕上方延伸。
下面的坐标位于 755 × 646 的局部画布内。把窗口 ID 和锚点区域替换为实际观察值后，
将整段源码直接通过标准输入交给 `zuse script -`：

```sh
zuse script - --max-steps 4096 --max-wait-ms 90000 <<'ZUSE'
import(path="examples/fractal-sierpinski.zuse",as="fractal")
mouse.use(kind="user",appearance="system")
window.focus(id="OBSERVED_WINDOW_ID")
wait(ms=200)
anchor.set(name="board",window="OBSERVED_WINDOW_ID",x=160,y=180,width=755,height=646)
timer.start()
fractal.sierpinski(canvas="board",x=50,y=590,width=650,height=560,depth=5)
timer.stop()
window.capture(id="OBSERVED_WINDOW_ID",path="fractal.png")
mouse.release()
ZUSE
```

深度 5 产生 243 个三角形，每个 4 个路径点。示例把每笔设为 60 ms，起笔两次各等待 10 ms，
固定等待预算合计 19,440 ms；实际耗时还包括窗口检查和输入派发。深度每增加一层，笔画数乘以 3。
输入提交回执仍需结合画面确认；请在整个批次后观察，不为每个顶点请求截图。

要画三个颜色区域，可分三批调用下表参数，每批之间由调用方显式切换应用画笔颜色。
这三批与上面的深度 5 图形拥有相同的三角形位置。

| 区域 | `x` | `y` | `width` | `height` | `depth` |
| --- | ---: | ---: | ---: | ---: | ---: |
| 左下 | 50 | 590 | 325 | 280 | 4 |
| 右下 | 375 | 590 | 325 | 280 | 4 |
| 上方 | 212.5 | 310 | 325 | 280 | 4 |

## 预算、会话与兼容边界

递归调用深度最多 32，准备阶段的展开工作最多 16384 次；空循环和不产生输入的递归也计入限制。
实际原语数量继续受 `--max-steps` 限制，等待时间受 `--max-wait-ms` 限制，源码、模块与绘图点数上限保持生效。
达到限制或遇到非法计算会在输入前失败，之前保留的函数不会被未完成的定义替换。

这些变量控制一次程序准备过程；函数和设备状态仍按原有会话规则保留。
导入保持声明边界，禁止在模块顶层执行 `let`、`if`、`for` 或电脑操作；把计算放进函数体，再显式调用。
带数值参数的 scoped mouse 函数仍通过原有借用与并行冲突检查，数值展开不会绕过鼠标所有权规则。

对应验证见 [scalar_programs.rs](../tests/scalar_programs.rs)：包含优先级、作用域、半开范围、递归终止、
三角形几何与笔画数、无效计算、预算、导入和会话事务，以及 scoped mouse 的借用冲突。
