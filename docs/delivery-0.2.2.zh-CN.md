# zuse 0.2.2 迭代交付报告

日期：2026-11-20（Asia/Taipei）

## 已交付的结果

zuse 0.2.2 已正式发布。此次迭代修复浏览器页面切换、macOS 窗口选择和移动端截图保存中的三个状态可靠性问题。五个平台的发布包与 crates.io 包均已生成；下载的 Linux x64 正式二进制已通过实际 MCP stdio 浏览器验收。

- [正式版本 v0.2.2](https://github.com/LIghtJUNction/zuse/releases/tag/v0.2.2)
- [发布提交 94c837551a7a2fd8ec9f577a3e2f57309bd5ce5a](https://github.com/LIghtJUNction/zuse/commit/94c837551a7a2fd8ec9f577a3e2f57309bd5ce5a)
- [发布流水线 44681321724](https://github.com/LIghtJUNction/zuse/actions/runs/44681321724)
- [crates.io：zuse-computer 0.2.2](https://crates.io/crates/zuse-computer/0.2.2)
- [源码验收说明](https://github.com/LIghtJUNction/zuse/blob/main/docs/validation-0.2.2.md)、[源码验收记录](https://github.com/LIghtJUNction/zuse/blob/main/docs/evidence/acceptance-0.2.2.json)、[正式包验收记录](https://github.com/LIghtJUNction/zuse/blob/main/docs/evidence/release-0.2.2.json)

## 具体修复

### 浏览器：重复选择当前页面保留原有状态

浏览器页面和与页面连接的 CDP 会话是两个不同的对象。原实现会先释放当前对象，再尝试连接目标；当目标就是当前页面时，这可能先关闭自己随后要连接的页面。

现在，重复选择当前 target 会直接保留原会话与所有权。切换到其他页面时，先连接并初始化候选会话，再释放原选择；明确的准备失败会保留原选择。对关闭旧目标的返回值也会检查，防止把关闭失败当成成功。用户原有页面的所有权不会被误当成 zuse 创建的页面。

验证覆盖七个有状态 CDP 回归场景，修复前其中五个失败、修复后全部通过。真实 Chromium 验收进一步确认：连续两次选择当前页面后，DOM、页面变量及输入的“preserve 中文 🙂”保持不变；选择不存在的页面报错后，原页面仍可操作。

证据：[浏览器修复提交](https://github.com/LIghtJUNction/zuse/commit/fe5d6bc4f47a3464a46d7ea358b99b2766ec56c2)。

### macOS：先确认唯一窗口，再切换焦点

原实现会先把应用带到前台，再从标题相同的窗口中选择第一个。这使不存在的目标也可能改变前台应用，重名窗口则可能被误选。

现在会完整扫描 Accessibility 窗口列表，确认标题只有一个匹配项后，才执行前台激活与窗口提升。找不到目标、标题无法读取、标题重复或窗口数量超过限制时，会先返回明确错误。

生产代码共用的选择逻辑通过十二项可移植测试，其中四项为新增回归测试；两个 macOS 架构均通过编译检查。这里仍采用标题对应 Quartz 与 Accessibility 窗口，重名时会拒绝选择，尚未引入跨接口稳定窗口标识。

证据：[macOS 修复提交](https://github.com/LIghtJUNction/zuse/commit/2195fce8bb3da8b73ad47834d43d6a2f1d68316f)。

### Android / iOS：确认截图结构后，再替换已有文件

此前 Android 截图只检查很短的 PNG 头部，24 字节的截断输出也可能被接受并覆盖旧截图。现在会检查完整块边界、首个 13 字节 IHDR、尺寸范围、至少一个非空 IDAT，以及长度为零且位于末尾的 IEND，并拒绝重复 IHDR 和尾随字节。

Android 与 iOS 共用暂存文件写入逻辑：先在目标目录创建独立临时文件，写入并刷新后，再做最终期限检查和文件替换。替换前发生错误会保留原目标，并尝试清理临时文件；已测试的失败场景没有留下暂存文件。这样可以避免一次失败截图覆盖此前的截图。Android 的空输出路径也会在整段程序执行前被拒绝。

验证包括五项 ADB shell-v2 集成测试、一项确定性的期限失败测试，以及二十项既有 iOS 协议测试。ADB 回归复现了“协议正常结束但图片只有 24 字节”的真实错误路径。

此处的 PNG 检查针对结构与边界，不包含 CRC 校验或像素解码。最终期限检查也不能中断已经开始的同步磁盘系统调用。

证据：[移动端截图修复提交](https://github.com/LIghtJUNction/zuse/commit/b64d940202bbf2aefc9c76dbb9d9856368c0a897)。

## 验收结果与范围

| 验收项目 | 结果 | 能支持的结论 |
| --- | --- | --- |
| 完整 Rust 测试 | 150 通过、0 失败；4 项默认忽略 | 单元、集成和协议回归通过 |
| 格式与 Clippy | 通过，Clippy 将警告视为错误 | 当前源码通过项目本地门禁 |
| 原生与 WebAssembly no_std | 均通过 | 无标准库核心保持可编译 |
| Windows GNU 全目标检查 | 通过 | 编译兼容；没有新的 Windows 桌面实机验收 |
| macOS arm64 / x64 全目标检查 | 均通过 | 编译兼容及可移植控制流测试；没有新的 macOS 桌面实机验收 |
| 源码阶段真实 Chromium 回归 | 显式运行 1 项，通过 | 私有无头浏览器中的页面选择与清理正常 |
| 下载的 Linux x64 正式包 | MCP stdio 7 项通过 | 正式二进制版本、页面状态、错误恢复与清理通过 |

七项 CDP、十二项 macOS helper、五项 Android、二十项 iOS 和一项期限测试均包含在 150 项中，不重复累加。显式执行的源码浏览器测试是四项默认忽略测试之一。正式包的七项 MCP 验收是独立脚本中的七个检查点。未修改的 X11、Accessibility 和原有浏览器原生测试没有在本轮重新执行。

Android / iOS 验收使用受控的主机端协议对端，不能据此宣称新增了手机实机覆盖。

正式包验收使用 zuse 自己创建的 Chromium 进程、私有 profile 与本地 data URL。结束时 MCP 进程退出码为 0，临时 profile 数量为 0，调试端口已关闭。复验脚本为 [tests/release_browser_selection.py](https://github.com/LIghtJUNction/zuse/blob/main/tests/release_browser_selection.py)：

~~~sh
python3 tests/release_browser_selection.py \
  --binary /path/to/downloaded/zuse \
  --output /path/to/acceptance-output
~~~

## 发布包身份

发布任务已成功完成。下列五个资产均由 v0.2.2 正式发布任务生成：

| 平台 | 正式资产 |
| --- | --- |
| Linux x64 | zuse-v0.2.2-x86_64-unknown-linux-gnu.tar.gz |
| Linux arm64 | zuse-v0.2.2-aarch64-unknown-linux-gnu.tar.gz |
| macOS Apple Silicon | zuse-v0.2.2-aarch64-apple-darwin.tar.gz |
| macOS Intel | zuse-v0.2.2-x86_64-apple-darwin.tar.gz |
| Windows x64 | zuse-v0.2.2-x86_64-pc-windows-msvc.zip |

用于实际 MCP 验收的 Linux x64 下载包为 729,270 字节，SHA-256：

~~~
db531b29615417646964b6ed68c810af7cc0cfc94193cdab94a5dcf715013956
~~~

解压出的二进制为 1,378,256 字节，`--version` 输出 `zuse 0.2.2`，SHA-256：

~~~
0fc5bc80c991b754e4b864560eaf52105e99d9433fa219f021d13d27ab34fcdf
~~~

crates.io 包为 387,761 字节，SHA-256：

~~~
16d3bf8c73bf64721672bf0483899b796967c8d3b53fa0ea934b3e705d6e2fde
~~~

包内 `.cargo_vcs_info.json` 指向正式提交，32 个 `src` 文件逐一与该提交一致。官方 API、下载 archive 与消费者真实 Cargo.lock 的校验和一致。发布版本提交只把 Cargo.toml 和 Cargo.lock 中的 zuse 版本从 0.2.1 改为 0.2.2，未改动已验收的生产源码。

本轮将正式包解压到隔离的验收目录，没有把它安装到用户 PATH。已发布、已通过正式包测试与已在日常环境部署，是三个不同的状态。

## 下游接入与尚未完成的部署

本轮影响图包括 ztype、ssmcp、zrc、oh-my-ztty 及 Project Z 聚合仓库。各层状态如下；源码依赖已更新，不代表应用已经发布或安装。

| 对象 | 本轮落地状态 | 验收与后续边界 |
| --- | --- | --- |
| ztype | [main 提交 12e8a458](https://github.com/LIghtJUNction/ztype/commit/12e8a458d69b222a001a9e479823b65da840db6b)，精确依赖正式 registry 包 `=0.2.2`，真实 Cargo.lock 已更新 | 13 项 Rust、14 项 CLI、11 项真实 GTK 全部通过；macOS arm64 的 desktop/plugins 编译通过。完整 macOS GUI 构建受本机缺少 `cc1obj` 限制；本轮未发布或安装新版 ztype 应用。 |
| SSMCP | [独立候选 bfe5f25](https://github.com/LIghtJUNction/ssmcp/commit/bfe5f25ffab718f100b11d2ce55c8dc839cbad71)，分支 `deps/zuse-0.2.2-20261120` | 实际构建通过，12 项后端契约测试通过，6 个真实 GTK 场景通过；尚未合入应用 main，也未据此发行或安装。 |
| Project Z | [聚合快照 142dbe04](https://github.com/LIghtJUNction/project-z/commit/142dbe043bffd8fa1cf8aa9b1d55fe7422f08127) 固定 zuse、ztype 和 SSMCP 候选源码 | sync、check 和 `resolve --all` 通过，36 个 Rust 仓库实际解析成功。详细记录保留在原聚合仓库，访问遵循该仓库权限。 |
| oh-my-ztty | 核查的 [6f6994e8 快照](https://github.com/LIghtJUNction/oh-my-ztty/blob/6f6994e8fb5ceab37d7598506d5f8cfbcfc7a6ea/dependencies.lock.json) 仍锁定 SSMCP `v0.10.10` | 等待已接入 zuse 0.2.2 的 SSMCP 正式版本和资产，再由现有套件维护会话更新 tag、校验和及安装验收。 |
| zrc | 核查源码为 [f4a0fe84](https://github.com/LIghtJUNction/zrc/commit/f4a0fe84c3499c00c5a01f1831e34fcbf5d5790c)，经运行时 SSMCP 间接使用 zuse | 需对实际 PATH 中的新 SSMCP 二进制核验版本与摘要，再执行 Neovim `:ZrcConnect`。本轮不把源码候选算成该安装链已经升级。 |

ztype 的 [接入报告](https://github.com/LIghtJUNction/ztype/blob/12e8a458d69b222a001a9e479823b65da840db6b/docs/zuse-0.2.2-consumer.zh-CN.md) 记录了正式包、Cargo 缓存和应用二进制的对应关系。GTK 接收端读取到了预期的 96 个 UTF-8 字节；其余 15 个锁定包保持不变。

SSMCP 候选以 `785e6b00440b048d00d9da626a33defa99108afe` 为基线，只修改 `Cargo.toml`、`Cargo.lock` 和 `crates/zuse` 三处。实际构建二进制 SHA-256 为：

~~~
5a34bdd5ec623a676ea4a378213b015488b70078e7cb0be8d5ea9c271e95fe9a
~~~

[SSMCP 接入证据](https://github.com/LIghtJUNction/zuse/blob/main/docs/evidence/ssmcp-consumer-0.2.2.json) 同时保留十二项契约测试和六个 GTK 场景。测试输出中的“28”是包含轮询的运行断言数，不能解释成 28 个独立场景，也不能再与六个 GTK 场景相加。隔离 Xvfb 与私有总线的清理已通过。

当前具体接手方是维护 [SSMCP PR #109](https://github.com/LIghtJUNction/ssmcp/pull/109)、[PR #110](https://github.com/LIghtJUNction/ssmcp/pull/110) 的现有会话，以及 oh-my-ztty / zrc 的现有维护会话。已向 SSMCP 与套件维护会话提供候选、验收结果和后续安装要求。仍需由 SSMCP 维护方把三处依赖变化与当前应用改动组合、验收并正式发布，再推进套件与 Neovim 连接验收。核查时 SSMCP 最新正式版本仍为 v0.10.13；本轮候选及聚合快照均未被记作新版 SSMCP 正式包或用户安装。

Project Z 的 [完整传播记录](https://github.com/LIghtJUNction/project-z/blob/142dbe043bffd8fa1cf8aa9b1d55fe7422f08127/docs/reports/zuse-0.2.2-propagation-20261120.zh-CN.md) 记录了三个固定源码指针、解析结果与安装边界，并保留了并发进入聚合 main 的既有文档提交。

## 交付位置

代码、测试脚本和验收记录随各自 GitHub 仓库提交。报告与公开证据同步到 [Google Drive 的 zuse 文件夹](https://drive.google.com/drive/folders/1eELzifcMDUrlYHoERHZYzh-d6W7ghh26)。

- [正式发布包验收 JSON](https://drive.google.com/file/d/17xg-9SS5ofrzLCj0cA-EDx4d6_FZdDkH/view)
- [SSMCP 接入候选验收 JSON](https://drive.google.com/file/d/1xMnRBP6wZgit2OVBxnA3csRPvviMtotD/view)
