# Zuse 包：发布、安装与导入

包把可复用的 `.zuse` 函数、入口和依赖放进一个 Git 仓库。
v1 直接使用 Git 来源，不依赖中央注册服务；安装命令获取代码，`import` 只读取已锁定的本地代码。
适合把画图、观察和应用操作函数分开维护，再在同一个项目中组合。

## 先安装，再导入

在项目目录执行以下命令。先把 `PACKAGE_GIT_URL` 设置为你选定的包仓库地址，
并确认它确实包含 `v0.1.0` 标签及有效的 `zuse.json`：

```sh
zuse pkg init paintapp
zuse pkg add "$PACKAGE_GIT_URL" --rev v0.1.0
zuse pkg list
zuse pkg install --locked
```

`add` 从指定版本的清单读取包名，写入依赖并安装；它不接受另起包名的参数。
若包名为 `pens`，可先加载并查看函数目录：

```sh
zuse -c 'import(path="pkg:pens",as="pens"); functions(namespace="pens")'
```

`pkg:pens` 读取包的 `entry`；`pkg:pens/drawing` 读取名为 `drawing` 的公开导出。
导出名不是任意文件路径：没有声明的模块、`..` 和越界路径不能通过 `pkg:` 访问。
省略 `as` 时，入口默认使用包名，导出默认使用最后一段导出名；显式 `as` 可选择合法的函数命名空间。

安装时也会静态检查公开入口及相对模块，拒绝导入时执行的顶层动作。
一次导入只注册函数，不执行函数体。导入成功后，在同一个脚本或持久 shell 中调用它们。
画图函数需要的鼠标、颜色和锚点仍由调用方准备，参见[绘图与锚点](drawing.md)。

## 三份数据各管什么

| 文件或目录 | 内容 | 维护方式 |
| --- | --- | --- |
| `zuse.json` | 包名、版本、入口、公开导出，以及依赖的 Git 来源和精确 `rev` | 随项目提交 |
| `zuse.lock.json` | 解析后的完整 Git commit 和依赖关系 | 安装命令生成，随项目提交 |
| `.zuse/packages/<name>/<commit>/` | 对应提交的本地 Git 检出 | 由安装命令管理，不提交缓存 |

清单记录“请求哪个版本”，锁文件记录“实际使用哪个提交”。
分支和标签可能改变指向，因此可复现安装以锁文件中的完整 commit 为准。
版本字符串用于包的发布标识；v1 不解析 `^1.2`、`~1.2` 或其他版本范围。

每个项目对同一个包名只保留一个来源和版本。直接与间接依赖提出不兼容要求时，
安装明确报冲突，需要先统一依赖；v1 不自动挑选版本，也不并存多版本。

## 编写一个可发布的函数包

`zuse pkg init pens` 创建初始项目，起始版本为 `0.1.0`。按需要编辑清单和源码：

```json
{
  "schema": 1,
  "name": "pens",
  "version": "0.1.0",
  "entry": "lib.zuse",
  "exports": { "drawing": "drawing.zuse" },
  "dependencies": {}
}
```

入口 `lib.zuse` 可以用普通相对导入组织内部模块：

```text
import(path="drawing.zuse",as="drawing")
```

`drawing.zuse` 只放声明。例如，把画线接口包装成带明确参数的函数：

```text
/// 在调用方已观察并设置的锚点内画一条线。
fn line(anchor,x1,y1,x2,y2) {
    draw.line(anchor=anchor,x1=x1,y1=y1,x2=x2,y2=y2)
}
```

使用 `import(path="pkg:pens",as="pens")` 后，函数名为 `pens.drawing.line`。
使用 `import(path="pkg:pens/drawing",as="brush")` 后，函数名为 `brush.line`。
包需要其他包时，也使用 `pkg:`，并把依赖写入清单的 `dependencies`：

```json
"dependencies": {
  "shared": { "git": "https://example.com/team/shared.git", "rev": "v0.1.0" }
}
```

这里的 URL 是格式示例，必须换成真实来源。Git 来源也可以是本机绝对仓库路径或 `file://` 地址，
便于离线开发和测试；共享给其他机器的清单应使用它们能够访问的来源。

## 发布与日常维护

先提交源码、清单和必要的锁文件，并为 Git 仓库配置 `origin`，再检查发布计划：

```sh
zuse pkg publish --dry-run
```

`--dry-run` 检查本地干净的 Git 检出、模块声明、`origin` 配置和已有本地版本标签，
输出 tag、完整 commit 与目标地址；它不联网探测远端是否已有同名标签。
检查通过后，`zuse pkg publish` 把版本对应的 `vVERSION` 标签推送到 `origin`，不强制覆盖冲突标签。
它不替你创建仓库、提交源码或推送分支。升级包时先修改清单版本、提交变更，再检查发布计划。

| 命令 | 用途 |
| --- | --- |
| `zuse pkg init [NAME]` | 初始化清单；省略名称时使用当前目录名。 |
| `zuse pkg add URL --rev REV` | 添加指定 Git 来源与精确引用，并安装依赖。 |
| `zuse pkg install` | 保留声明匹配的已锁定 commit，补齐缺失的锁记录或缓存。 |
| `zuse pkg install --locked` | 严格按现有锁文件安装；清单与锁不匹配时失败。 |
| `zuse pkg update [NAME]` | 重新解析指定依赖现有的 `rev`，不修改清单中的引用；省略名称时重新解析全部。 |
| `zuse pkg remove NAME` | 移除项目的直接依赖。 |
| `zuse pkg list` | 查看项目依赖。 |
| `zuse pkg publish --dry-run` | 检查发布条件并查看发布计划。 |

团队和自动化环境优先提交锁文件并运行 `install --locked`。
准备升级时再显式 `update`，审阅锁文件中的 commit 变化，并检查使用这些函数的脚本。

## 导入边界与失败处理

一次程序准备过程固定使用入口目录或初始工作目录所属的项目。
嵌套包自己的 `zuse.json` 不会切换项目根目录；包内的 `pkg:` 依赖继续查原项目的锁文件。
普通相对文件导入仍以当前模块目录为基准，`import(source=...)` 的行为保持不变。
没有使用 `pkg:` 的程序不读取或要求包清单。

`import` 不联网、不安装依赖、不执行安装钩子。解析器检查锁定提交、本地检出状态和公开入口；
缺少锁或缓存、检出被修改、入口失效时会失败，需先处理问题并显式运行安装命令。
不要直接修改安装缓存；在包的源码仓库修改并提交，再让项目更新到新版本。

包沿用现有模块边界：只允许函数声明及声明式导入，禁止导入时执行顶层动作，
拒绝循环导入和递归调用，并共享模块数、读取字节、展开步骤和等待时间预算。
准备或校验失败不会把新函数提交到会话。已成功注册的函数可在同一会话内复用，调用时不会自动刷新文件。

完整模块语义见[本地程序与函数复用](local-programs.md)；实现见
[包解析器](../src/packages.rs)、[模块加载器](../src/script/modules.rs)，
离线用例见[包导入测试](../tests/package_imports.rs)。
