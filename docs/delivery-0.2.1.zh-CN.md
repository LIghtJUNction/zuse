# zuse 0.2.1 迭代与发布验收

记录日期：2026-11-20（Asia/Taipei）。

**zuse 0.2.1 已正式发布。** 五个平台的构建和发布工作流全部成功，`zuse-computer 0.2.1` 已发布到 crates.io。下载的 Linux x86_64 正式执行文件通过 **17 个 GTK 场景、12 个 MCP 测试、5 组 PTY 串口检查和 11 组 USB C ABI 检查**。源码阶段的完整 Rust 测试为 **133 通过、0 失败、3 项图形测试默认忽略**。本记录区分源码验证、正式产物验证和下游接入状态。[发布页](https://github.com/LIghtJUNction/zuse/releases/tag/v0.2.1) · [成功的发布工作流](https://github.com/LIghtJUNction/zuse/actions/runs/44648994167) · [完整发布回执](evidence/release-0.2.1.json)

## 1. 本轮改动

### X11：解决文本事件与接收程序不同步的问题

键盘事件携带的是按键编号，接收程序处理事件时才把编号转换成字符。如果临时修改全局键位表、发送事件后马上恢复，接收程序稍后处理时可能已经看不到原来的映射。等待一小段时间不能保证接收程序已经处理完毕。

本轮实现先检查整段文本和键盘状态：普通、可映射且键盘空闲的输入继续使用键盘路径；需要 Unicode 支持，或存在按住的 Enter、Shift 等按键时，在发送任何按键之前选择 AT-SPI 文本路径。该路径确认当前聚焦的本地可编辑控件，提交整段 UTF-8 文本，再核对文本和光标。它不临时改写全局键位表，也不代替用户松开按键。[实现与审阅提交](https://github.com/LIghtJUNction/zuse/commit/8cb97129feeed2eea82b52aefaaf0743a29122b1)

同时修复两个错误处理问题：插入后焦点改变时，返回“文本可能已经改变”的状态；应用在插入回调中改写旧选区时，删除前比较原文，保留应用修改并报告无法确认。旧选区快照有 65,536 字符及 262,144 UTF-8 字节的上限。[源码审阅证据](evidence/x11-utf8-review-2026-11-20.json)

### Windows 与 macOS：验证实际生产逻辑

| 范围 | 改动 | 验证依据 |
| --- | --- | --- |
| Windows 串口路径 | COM 后只接受 ASCII 十进制数字，编号为 1–4096；保留 `\\.\COM1` 和纯数字前导零形式 | 旧整数解析会接受 `COM+1`；先复现失败，再验证修复。2 项主机测试覆盖 8,194 个合法、60 个非法样本。[提交](https://github.com/LIghtJUNction/zuse/commit/4bce4ff186eef807e39421f1b919a2ac6c849c37) |
| Windows 后台键盘消息 | 先验证完整组合键并生成计划，再发送消息；修正 E0、扫描码、Alt/Ctrl/F10 消息种类与反向释放顺序 | 11 项生产规划逻辑模拟，包含 120 种三键组合排列，覆盖修饰键、字母键和 F10。[提交](https://github.com/LIghtJUNction/zuse/commit/4102ed007b3b618d2b8e13e14312a2ba69153c40) |
| macOS 窗口聚焦 | 传递窗口查询、标题读取、无匹配目标及提升窗口的错误；原生对象保持有效直到操作结束 | 8 项生产控制流程模拟，覆盖错误出口与对象生命周期。[提交](https://github.com/LIghtJUNction/zuse/commit/4102ed007b3b618d2b8e13e14312a2ba69153c40) |

模拟直接调用生产校验、消息规划和聚焦流程，并注入可控制的平台返回结果。Windows 专用 Runtime 整程序预检测试完成编译检查，本轮未执行；它没有被计入上述 2 项串口主机测试。Windows 前台 SendInput 路径未改动，后台消息是否被特定应用接受仍需对应桌面运行验证。[可移植模拟回执](evidence/portable-simulation-2026-11-20.json)

## 2. 验证结果与计数

| 验证层级 | 已确认结果 | 实际覆盖范围 |
| --- | --- | --- |
| 完整 Rust 测试 | **133 通过，0 失败，3 默认忽略** | 源码整合后的主机测试 |
| 新增可移植模拟 | **21 项，包含在 133 中** | 串口 2、Windows 键盘 11、macOS 聚焦 8 |
| 移动端相关测试 | **iOS/WDA 20、Android 浏览器 15，包含在 133 中** | 主机侧适配器、协议及受控响应 |
| 格式与静态检查 | 通过 | `cargo fmt`、Clippy all-targets 且警告视为错误 |
| `no_std` 检查 | 通过 | native 与 WebAssembly |
| 跨平台 all-target 检查 | 通过 | Windows GNU、macOS arm64、macOS x64 |
| 显式原生 X11 与 AT-SPI | 通过 | 私有显示中的指针、前台/后台文本、PNG、控件观察/编辑/动作/脱敏 |
| 正式 Linux CLI → GTK3/AT-SPI | **17/17 符合预期：9 成功、8 预期错误** | 真实 GTK 接收控件，隔离的 X11 显示与总线 |
| 正式 Linux CLI → MCP | **12/12，无跳过** | stdio、HTTP、Unix socket、错误与交互契约、真实隔离截图 |
| 正式 Linux CLI → PTY | **5/5 组** | 真实 Linux 内核伪终端，包含 7 项整程序预检 |
| 正式 Linux CLI → USB C ABI | **11/11 组** | 受控 libusb 设备响应，包含 11 项整程序预检 |
| 正式发布构建 | **5/5 平台成功** | Linux x64/arm64、macOS x64/arm64、Windows x64 MSVC |

来源：[源码综合回执](evidence/acceptance-0.2.1.json)、[正式产物综合回执](evidence/release-0.2.1.json)。子集、组内案例、debug 与正式产物的重复运行没有累加为一个虚高的总数。

三个默认忽略的图形测试仍保留在 Rust 套件结果中。AT-SPI 已显式运行；未改动的桌面 Chromium 路径沿用前次 0.2.0 记录，本轮没有重跑，不能表述为全部图形路径均已执行。[源码阶段技术记录](validation-0.2.1.md)

测试环境也完成了一次修正：最初的显示 helper 错误要求可选的 `.XN-lock` 文件，导致旧原生 X11 门禁中止；最终采用原子目录 lease、私有 Xauthority cookie、精确显示号及带认证的就绪探测。受影响的 X11、AT-SPI 与 MCP 检查随后全部通过，初次失败保留在回执中。这次 helper 修正没有改变生产 Rust 或已经通过的 GTK、PTY、USB 脚本。[修正提交](https://github.com/LIghtJUNction/zuse/commit/a6dc1b0a47de4327aa6407a0881da547811c41eb)

## 3. 正式发布身份与下载验收

| 对象 | 精确身份 |
| --- | --- |
| 正式源码提交 | [`d930769005d307c42639e8abedc81ab039427e40`](https://github.com/LIghtJUNction/zuse/commit/d930769005d307c42639e8abedc81ab039427e40) |
| tag | `v0.2.1`；annotated tag object `26bbed02d6c45dbda1c58145fe399060c3455815` |
| 发布工作流 | [44648994167](https://github.com/LIghtJUNction/zuse/actions/runs/44648994167)，发布工作流成功 |
| 工作流触发时的 head | `5510f553f0509a0aa2eb3ac02e6d3d0c989efc6e`；版本提升后构建源码为上面的 `d930769…` |
| 发布时刻 | 2026-11-20 08:52:44（Asia/Taipei），即 `2026-11-20T00:52:44Z` |
| crates.io 包 | [`zuse-computer 0.2.1`](https://crates.io/crates/zuse-computer/0.2.1)，归档 369,745 字节 |
| crate SHA-256 | `8da9a00ef8468d196234a99cb4d7f47991d9fb1ef704f67da2ce63d46395ea04` |
| 已下载的 Linux x64 压缩包 SHA-256 | `ff19a2428adfe98e0b39d7ee0f0eb2da7deeba7a57693c56723f37ae61419908` |
| 解压后二进制 SHA-256 | `0efb05ca3fc0a646519be82aa96ecbf96502f46cda0d4c9131161cd54ea2575c` |
| 二进制实际版本输出 | `zuse 0.2.1` |

下载摘要已与 GitHub 资产摘要核对；crate 归档摘要、`.cargo_vcs_info.json`、包版本及 31 个源码文件已与正式提交逐字核对。版本提升只修改 Cargo 版本信息，验收过的生产源码保持一致。上述正式执行文件实际完成了第 2 节的 GTK、MCP、PTY 与 USB C ABI 检查。[发布验收原始证据](evidence/release-0.2.1.json)

| 平台 | 发布资产 |
| --- | --- |
| Linux x64 | [zuse-v0.2.1-x86_64-unknown-linux-gnu.tar.gz](https://github.com/LIghtJUNction/zuse/releases/download/v0.2.1/zuse-v0.2.1-x86_64-unknown-linux-gnu.tar.gz) |
| Linux arm64 | [zuse-v0.2.1-aarch64-unknown-linux-gnu.tar.gz](https://github.com/LIghtJUNction/zuse/releases/download/v0.2.1/zuse-v0.2.1-aarch64-unknown-linux-gnu.tar.gz) |
| macOS Apple Silicon | [zuse-v0.2.1-aarch64-apple-darwin.tar.gz](https://github.com/LIghtJUNction/zuse/releases/download/v0.2.1/zuse-v0.2.1-aarch64-apple-darwin.tar.gz) |
| macOS Intel | [zuse-v0.2.1-x86_64-apple-darwin.tar.gz](https://github.com/LIghtJUNction/zuse/releases/download/v0.2.1/zuse-v0.2.1-x86_64-apple-darwin.tar.gz) |
| Windows x64 | [zuse-v0.2.1-x86_64-pc-windows-msvc.zip](https://github.com/LIghtJUNction/zuse/releases/download/v0.2.1/zuse-v0.2.1-x86_64-pc-windows-msvc.zip) |

源码阶段的 debug CLI 仍单独记录为 `0.2.0`，SHA-256 为 `ba171c507503ba48c60026a7648fc904fc0d27ac63b1851bde4843f7bab82562`；最终源码验收提交为 `a6dc1b0a47de4327aa6407a0881da547811c41eb`。这些是正式版本提升之前的历史身份，与已下载的 0.2.1 产物分开记录。[历史 CLI 回执](evidence/x11-cli-0.2.1.json)

## 4. 运行边界

GTK 的 8 个预期错误包含“已发生编辑，但无法确认最终状态”的情况。选区替换涉及插入、应用回调、比较和可能的后续删除，并非原子事务；比较和删除之间仍可能发生并发编辑。确认失败时，调用者需要先读取接收控件现状。zuse 不自动重发，也不盲目回滚可能由应用或用户修改的文本。

夹具中 `CLIPBOARD` 的文本、二进制载荷及 owner 保持；`PRIMARY` owner 在部分场景发生变化。验收证明的是这些明确观察到的状态，不能扩展为所有选择区状态都不变。[GTK 逐场景记录](evidence/release-0.2.1.json)

Linux x86_64 使用真实的原生 GTK/X11/AT-SPI 软件栈与内核 PTY，但运行在独立测试会话。Windows/macOS 的控制逻辑已模拟验证并完成构建，另外四个正式发布目标没有在本轮执行桌面验收。iOS/WDA 与 Android 浏览器属于主机协议测试，没有宣称真机通过；USB 使用受控 C ABI 响应，没有执行物理 USB 设备传输。[验证范围](evidence/release-0.2.1.json)

## 5. 下游接入状态

| 项目或环节 | 本轮已有证据 | 接入责任与尚需回执的步骤 |
| --- | --- | --- |
| zuse 本身 | 0.2.1 源码、tag、五平台资产、crate 和正式 Linux CLI 下载验收均完成 | 本报告及发布 JSON 记录具体身份 |
| ztype 源码集成 | 私有 Cargo 源码替换下的真实消费者通过 11 项检查，包含 held-Enter 回归先失败后通过 | [已归档的源码集成回执](evidence/ztype-held-enter-2026-11-20.json) |
| ztype 正式 registry 依赖 | 尚未收到“0.2.1 pin/lock + 最终 11 项消费检查 + 已推送提交”的完整回执；本轮读取默认分支时仍为 `=0.1.1` | Project Z 的 semver_gate 负责独占更新，已收到正式源码、checksum 与下载验收结果 |
| ssmcp | 本轮读取默认分支仍为 `=0.1.0` 并使用本地 `crates/zuse` 路径；集成分支的完整应用验收未在本报告确认 | Project Z integration_review 与应用维护会话负责依赖传播及应用级验收 |
| oh-my-ztty、zrc 与聚合仓库 | 已把正式发布身份和消费影响范围交给协调会话，未收到本轮 0.2.1 的完整接入回执 | suite 维护会话与 Project Z 协调者分别负责消费者及聚合检查 |

消费者源码检查可以证明候选实现解决了相应场景；正式依赖传播还需要精确版本、锁文件、消费者提交和运行结果。本轮已完成 zuse 的迭代、发布与产物复验，其余项目按各自维护者的回执继续确认。

## 6. 证据索引

- [源码阶段技术说明](validation-0.2.1.md)：源码身份、命令、测试层级与初次夹具失败的解释。
- [源码综合验收 JSON](evidence/acceptance-0.2.1.json)：133 项测试、编译检查和最终原生门禁。
- [源码阶段 GTK CLI JSON](evidence/x11-cli-0.2.1.json)：旧 debug 二进制身份及 17 个场景的接收端状态。
- [正式发布验收 JSON](evidence/release-0.2.1.json)：发布工作流、五平台资产摘要、crate 源码对应关系和下载后运行结果。
