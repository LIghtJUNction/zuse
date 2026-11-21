# Zuse 0.6.0 发布前传播准备快照

本轮从真实绘图、棋盘和双人游戏使用中迭代绘图原语、有界数值程序、Git 源码包与命名会话。本文是发布前传播准备快照，区分该截点已公开交付、已完成验证、已准备源码和待执行的传播。消费者准备回执截至 2026-11-21 18:20 UTC；后续客户端和主机输入发现另记于[验证记录](validation-0.6.0.md)。正式发布的 Git SHA、标签、资产和最终消费者回执在 Release 说明及 Project Z 传播记录中更新。

| 对象 | 本轮状态 |
| --- | --- |
| Zuse 0.6.0 本体 | `8b9d236a…` 已通过 473 项 Rust 测试、19 项忽略及隔离虚拟键盘验收。后续 `5028964e…` 加入严格原生设备映射，通过 192 项目标测试、4 项忽略、fmt、Clippy、构建及明确 PASS 的零输入原生预检。该截点位于源码提交和标签发布前；真实 evdev 输入及游戏验收另行安排。 |
| `std 0.1.0` | 已公开推送源码及 `v0.1.0`，并从公开 URL 在新项目安装验证。要求 Zuse 0.6.0；该次安装使用下文列出的固定预发布候选。 |
| 公开会话客户端 | [`examples/session_client.py`](../examples/session_client.py) 已完成独立服务中的函数复用和九项边界验收，也已实际完成游戏会话的只读状态与截图。随本体源码交付，未独立发布。 |
| 消费者源码 | 独立工作树已准备，部分配置与兼容性检查已通过。最终 Zuse SHA 尚未提供，没有把准备状态记为完成升级。 |
| Project Z 聚合 | 独立工作树已补全源码，46 个仓库的源码检查无错误；完整目录生成、聚合检查与依赖解析尚未执行。 |
| 实际游戏 | 尚未完成修复后的真实主机输入或对战验收。用户已授权继续后台修复、隔离测试与发布；不抢窗口、不发主机测试按键，实际对战留待用户回来。 |

## 版本与已公开标准库

本体合并基线为已发布 0.5.0 的
[`b5ebbd3679af215706cbd73b17045e014f5e0524`](https://github.com/LIghtJUNction/zuse/commit/b5ebbd3679af215706cbd73b17045e014f5e0524)。
这个既有提交不包含本轮完整功能，不能作为 0.6.0 的传播固定值。最终源码 SHA、标签和发布资产须在实际完成后补入；候选二进制摘要见[验证记录](validation-0.6.0.md)。

[`zuse-std`](https://github.com/LIghtJUNction/zuse-std) 的 `main` 与 `v0.1.0` 已固定到
[`d79f43f8f3c2a1bff05962420f014b6c16403c5c`](https://github.com/LIghtJUNction/zuse-std/commit/d79f43f8f3c2a1bff05962420f014b6c16403c5c)。
仓库只含源码、文档和示例，已设置 `zuse`、`zuse-package` 主题。包包含 22 个公共函数，导出 `desktop`、`drawing`、`fractal`、`input`、`timing` 五个模块，最低要求 Zuse 0.6.0。

已实际执行 `pkg publish --dry-run` 和 `pkg publish`，均退出 0。随后在全新项目中从公开 URL 按 `v0.1.0` 添加依赖，执行 `pkg install --locked` 和 `pkg list`，确认锁文件完整 SHA 与公开标签一致，状态为已锁定且已安装。`pkg:std` 导入得到 22 个函数，完整 API 预检为 43 步，递归分形深度 5 预检为 243 个三角形。安装验收使用固定的 `3a4aba42…` 候选；没有把它表述为最终本体发布后的复验。

## 消费者基线与实际依赖方式

下表来自本轮最后一次核对的公开主分支、manifest、锁文件、gitlink 与项目清单。四个主要消费者的独立工作树均干净；SSMCP 与 suite 已在各自隔离分支快进到表内提交，没有生成本轮消费者提交或推送。

| 消费者 | 已核对的公开主分支 | 实际关系与待传播对象 |
| --- | --- | --- |
| ztype | [`bb2c1dc71a0f1dc77b56b0bc1bc7fd3cdb2501f8`](https://github.com/LIghtJUNction/ztype/commit/bb2c1dc71a0f1dc77b56b0bc1bc7fd3cdb2501f8) | 默认 `desktop` 功能通过规范 Git 依赖 `zuse-computer` 消费 Zuse；等待最终公开提交后更新真实 Cargo.lock。 |
| ssmcp | [`d4887aee7117973f890e7806c429eabf2199856c`](https://github.com/LIghtJUNction/ssmcp/commit/d4887aee7117973f890e7806c429eabf2199856c) | 已移除 `zuse-computer`、`crates/zuse` 与嵌入式桌面后端，改为独立 Zuse CLI 的版本、下载、安装与卸载管理。程序经通用 exec 运行；本轮没有需要更新的 Zuse Cargo pin 或 gitlink。最新源码的下载器使用已配置 TLS 的 HTTP builder。 |
| zrc | [`c127bd2965525adc05c46c6fba492aa1e45460f1`](https://github.com/LIghtJUNction/zrc/commit/c127bd2965525adc05c46c6fba492aa1e45460f1) | 可选 SSMCP 运行时关系；自身没有 Zuse Cargo 依赖或 Zuse gitlink。最终运行时集成仍须单独检查。 |
| oh-my-ztty | [`2fa1b47d41fbc441b482d7c4c23beac0c1e903e4`](https://github.com/LIghtJUNction/oh-my-ztty/commit/2fa1b47d41fbc441b482d7c4c23beac0c1e903e4) | 通过组件源码与 bundle 消费更新；`components/ssmcp` 仍为 `dde2938e050e1d9f8186a600f57dbb67e12de211`，待现有同步流程更新 gitlink 与 `dependencies.lock.json`。 |
| zuse-skills | [`2161508d009dd671c9de2ac0f232cdef01a2eb5e`](https://github.com/LIghtJUNction/zuse-skills/commit/2161508d009dd671c9de2ac0f232cdef01a2eb5e) | 纯源码扩展，通过 CLI 导入并预检 `.zuse` 模块；最新文档提交已纳入本轮兼容性复验。 |
| zuse-std | [`d79f43f8f3c2a1bff05962420f014b6c16403c5c`](https://github.com/LIghtJUNction/zuse-std/commit/d79f43f8f3c2a1bff05962420f014b6c16403c5c) | 已发布的 Zuse 源码包，通过 `pkg` 安装并在运行时导入。 |

SSMCP 的传播验收现在需要检查 CLI 管理器和正式资产契约：实际版本输出、平台匹配、`zuse-vVERSION-TARGET.{tar.gz|zip}` 命名以及 GitHub 提供的 SHA-256 摘要。旧报告中的嵌入式 Computer 后端测试不能代替这些检查。

## 已完成的准备验证

以下检查已经执行；跳过项和待执行项没有计为通过。

| 对象 | 实际命令与结果 | 覆盖范围与限制 |
| --- | --- | --- |
| zrc | `python3 scripts/check.py`：退出 0，9 passed、12 skipped，共 21 项。 | compose、headless nvim、终端配置、zsh 语法及 git/fzf/bat/rg 检查通过；缺少 `ZRC_TEST_BINARY` 的 12 项跳过，未验收最终 SSMCP 运行时。 |
| oh-my-ztty | `python3 scripts/workspace.py check --component suite`：退出 0，181 passed、0 skipped，145.574 s。 | 首次因缺少 zshard 源码未通过；初始化精确组件及递归源码后完整重跑通过。没有构建或安装正式 bundle。 |
| zuse-skills | `python3 zskills.py --zuse "$ZUSE_BIN" check`：退出 0，14 个函数通过原生导入、描述和预检，0.386 s。 | 最新提交使用 `5028964e…` 固定候选复验，并清除桌面显示环境；正式发布资产另记。 |
| zuse-skills | `ZUSE_SKILLS_BROWSER_TESTS=0 python3 -m unittest discover -s tests -v`，`ZUSE_BIN` 指向 `8b9d236a…` 候选：退出 0，32 passed、2 skipped，共 34 项，1.193 s。 | 浏览器集成开关关闭，没有向应用或浏览器发送输入。 |
| SSMCP | `python3 scripts/test_release.py` 中的 `BumpWorkflowTests` 与 6 项指定发布重放用例：退出 0，12 passed，1.357 s。 | 临时 Git/HTTP 夹具覆盖空提交、旧基线、重放、tag 冲突及 dry-run；没有实际发布。完整 argv 见 `consumer-contract-checks-20261122.json`。TLS Rust 检查和正式资产下载另记。 |
| Project Z 源码 | `repositories`、`checkout_errors`、`source_link_errors`：46 个仓库，0 个错误。 | 直接调用源码检查入口，未启动 Cargo；不代表完整聚合门禁通过。 |

本轮消费者准备没有启动 Rust 编译或依赖下载。ztype 最终锁文件与测试、SSMCP 当前 CLI 管理器测试、suite 最终组件同步及 bundle 检查均待执行。

## 隔离工作树与已解除的源码阻塞

主要消费者工作树位于：

```text
/home/lightjunction/Documents/GITHUB/project_z/dist/zuse-drawing-packages-consumers-20261122/
```

其中 `ztype/`、`ssmcp/`、`zrc/`、`oh-my-ztty/` 使用分支
`chore/zuse-drawing-packages-20261122`。

聚合工作树位于：

```text
/home/lightjunction/Documents/GITHUB/project_z/dist/zuse-drawing-packages-aggregate-20261122/
```

其分支为 `chore/zuse-drawing-packages-aggregate-20261122`，基线为 Project Z 的
`6e786250812849079c870804ce4141b74f88f7d3`。活动根目录已有的 26 项 gitlink 变更及未跟踪目录均未被修改；本轮只准备自己的隔离工作树。

此前 `zi18n: initialize the submodule first` 已解除。核查发现借用的本地对象池虽包含目标提交和树，却缺少两份文档 blob。对精确提交 `cdff8dea19e97268d296afa827767ea689420591` 获取一次浅层源码并通过 `git fsck --connectivity-only --no-dangling` 后，仅替换本轮新建的 `zi18n/` 与 `zmcl/vendor/zi18n/` 两个不完整检出；原目录保留在本轮备份区，活动源目录未改动。两个检出现在均干净并通过对象完整性检查。

聚合工作树已准备 `.gitmodules` 和 `project-z.toml` 的两项关系：SSMCP 通过独立 CLI 依赖 Zuse；`zuse-std` 注册为 `role = "extension"`、`manifests = []`、`requires = ["zuse"]`。后者与纯源码包的实际形式一致。加上它以后，共有 46 个仓库。当前这些文件和新的 `zuse-std/` 尚未暂存、提交或推送，也未生成最终 catalog。

## 尚待执行的聚合门禁

五次非阻塞获取共享 Cargo 所有权锁均退出 75，元数据命令未启动。因此本批次没有执行正式 `sync --record`、完整 `check`、`impact --repositories zuse` 或 `resolve --all`，没有新 catalog 可供验收。已通过的源码检查与这一待执行状态分别记录。

该截点已核对持锁方为另一轮 `work-maintainer` 的 SSMCP 任务 `8c29e68777659d8550f91b4d66c8af76-1c9`：Python PID 2869626 持锁，其 Cargo 子进程在 `dist/remote-dependency-sync-20261121/ssmcp` 执行 `cargo test --workspace --locked`，该批次还包含 Clippy。本轮没有终止持锁任务。聚合工作树的规则将依赖解析、编译与验收均纳入该锁；本段记录的是该聚合门禁的等待状态。

已有准备脚本在取得共享锁后强制离线元数据读取，并按项目原有顺序执行上述命令、保存真实退出码：

```sh
flock --nonblock --conflict-exit-code 75 \
  /var/tmp/project-z-cargo-target/.project-z-cargo-owner.lock \
  python3 /home/lightjunction/Documents/GITHUB/project_z/dist/zuse-drawing-packages-consumers-20261122/run_aggregate_metadata.py
```

该入口用于元数据与聚合检查，不替代消费者编译或安装验证；需要与本体构建协调共享锁，不覆盖其他维护任务。

## 最终传播顺序与回执

本轮维护会话可直接继续已有准备，不需要用户重新初始化源码或接管隔离目录。后续按实际完成状态补齐：

1. 完成本体修复与隔离门禁，记录最终源码完整 SHA、`v0.6.0` 和正式发布资产。明确真实游戏对战仍未验收。
2. ztype 使用 Cargo 生成必要锁文件变更，确认解析到最终 SHA，再运行 `cargo test --all-targets --locked --offline`。
3. SSMCP 运行 `cargo test --locked --offline --lib computer::`，检查最终 CLI 版本和正式发布资产契约；无需制造已移除的 Zuse Cargo/gitlink 更新。
4. zuse-skills 针对最终产物复验兼容性；zrc 检查最终运行时入口。std 的既有公开标签保持固定，若出现 API 差异应另发版本。
5. suite 使用 `workspace.py sync ssmcp` 同步已验收来源，同时核对 gitlink 与 `dependencies.lock.json`，再运行相关检查；需要分发时单独构建并 smoke 验证 bundle。
6. Project Z 在消费者公开提交可获取后更新相关 gitlink，通过正式聚合门禁并生成 catalog，再提交、推送最终快照。

每个消费者须分别记录最终源提交、相关 Zuse SHA 或 CLI 版本、实际命令及退出码、跳过项，以及源码推送、构建、安装状态。源码更新不会自动改变运行中的服务；通知也不等于消费者传播完成。

本轮开发机准备依据保存在上述消费者目录的 `preparation-receipt-20261122.json` 与 `aggregate-source-preparation.json`。两份回执明确记录最终 Zuse 提交尚为空、没有本轮消费者提交或推送、元数据批次未取得共享锁。后续执行结果应追加独立回执，保留这次准备阶段的原始范围。
