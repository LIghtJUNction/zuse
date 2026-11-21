# 函数、导入与持久会话

## 把步骤封装成函数

函数把参数替换进一组已知操作。调用前，执行器先展开整段程序，再统一检查操作、参数、只读模式、步数和累计等待预算。函数不是 JavaScript；没有任意表达式求值、网络导入或隐式系统命令。

```text
fn pause(ms) {
    wait(ms=ms)
}
pause(ms=10)
```

参数可以按位置或名称传入，使用文字、数字或布尔值。不要把字符串拼接成输入命令；参数替换基于语法树，字符串里的引号仍是数据。函数体调用另一个已定义函数时沿当前模块命名空间解析。

## 文件与源码导入

```text
import(path="wechat.zuse", as="wechat")
wechat.read_chat(scope="已核实的无障碍子树路径")
```

文件只允许函数声明及声明式嵌套导入。顶层输入或观察动作都会被拒绝，避免加载一个模块时意外操作桌面。入口文件的相对路径以该文件目录为基准，嵌套导入以被导入文件目录为基准。直接 `eval` 或 shell 中的路径以当前工作目录为基准。

```text
import(source="fn inspect(){ timer.status() }", as="tools")
tools.inspect()
```

相同模块重复导入可复用；要替换命名空间必须明确指定 `replace=true`。改变后的整个程序预检失败时，不提交新定义。执行中失败保留已成功预检的定义，并报告已经完成的动作。

```text
import(source="fn inspect(){ timer.status() }", as="tools", replace=true)
```

导入路径和源码必须是本次提交的显式字符串；不从网页、聊天文本或隐式搜索路径自动加载代码。不能递归调用函数，不能循环导入。每个源文件最大 1 MiB；一次准备读取总量、保留源码、模块数、函数数、导入层数和展开工作量都有上限。

## 显式 eval

`eval(source=...)` 可以同时定义函数并执行动作；所有动作仍合并进当前程序，使用同一预算和只读限制。它不会启动独立执行器或绕过授权。

```text
eval(source="fn pause(ms){wait(ms=ms)} pause(ms=1)", as="local")
local.pause(ms=2)
```

## 跨命令复用

启动 `zuse shell`，分别提交下面两行。第一行注册函数，第二行调用它；会话退出后定义释放。不同 `zuse eval` 进程不会共享定义。

```text
import(path="wechat.zuse", as="wechat")
wechat.read_chat(scope="已核实的无障碍子树路径")
```

`assets/wechat.zuse` 是应用函数的起点：`read_chat` 只观察，`prepare_message` 填写文本，`send_prepared(send_button, action_index)` 执行按钮动作。真实节点路径和动作索引必须先从目标应用取得，并确认窗口、联系人和按钮语义。示例本身不验证这些调用前条件或发送结果。创建模块不代表获准发送；只在已有明确授权下执行发送函数。示例不会自行搜索联系人或发送消息。

## 检查与运行

```sh
zuse check task.zuse
zuse run task.zuse
zuse script task.zuse --dry-run
```

`run` 和 `script` 根据语法树选择普通命令或 scoped mouse 引擎。含 `let`、借用鼠标参数、`parallel` 的程序沿用原有所有权检查；共享同一个鼠标的并行分支被拒绝。普通函数展开和 scoped mouse 的资源生命周期保持各自约束。字符串中的 `parallel` 或 `mouse()` 不会影响引擎选择。

先运行 `scripts/check_examples.py --binary /path/to/zuse` 检查示例。只有明确选择 `--execute-fixtures` 才启动自有 headless Chromium 页面并执行演示；微信模块只做预检。
