---
name: zuse-publish-package
description: 开发、验证和发布可复用的 Zuse 源码包，维护 zuse.json、版本标签和导出模块，并为 GitHub 仓库添加 zuse 主题。适用于封装自动化函数、制作标准库、分发或更新 Zuse 包。
---

# 开发与发布 Zuse 包

把可复用行为放进声明模块，通过显式参数接收窗口、节点、路径和内容。保留操作边界，避免导入产生输入。

## 创建源码包

在目标仓库根目录运行：

```sh
zuse pkg init my-tools
```

维护 `zuse.json` 的 `schema`、`name`、`version`、`entry`、`exports` 和 `dependencies`。
使用版本号和仓库内相对 `.zuse` 路径；入口与导出只放函数声明和模块导入。

```json
{"schema":1,"name":"my-tools","version":"0.1.0","entry":"lib.zuse","exports":{},"dependencies":{}}
```

在 `lib.zuse` 定义函数，需要独立模块时再添加对应的文件和 `exports` 项：

```zuse
/// Wait for an explicit, bounded duration.
fn pause(ms) { wait(ms=ms) }
```

用参数传递实际观察到的目标，不硬编码私密内容或用户专有窗口。计算坐标可用不可变变量、算术和有界控制流，以当前语言文档为准。
有依赖时用 `zuse pkg add URL --rev REV` 安装，提交 manifest 和生成的锁文件，排除 `.zuse/` 缓存。
一个项目内同名包只允许一个锁定源码版本；冲突应协调依赖，不覆盖缓存绕过检查。

## 验证

先核对当前 `zuse schema`，为函数写明确描述和调用例子。检查调用展开后的全部参数：

```sh
zuse -c 'import(path="lib.zuse",as="tools"); functions(namespace="tools")'
zuse -c 'import(path="lib.zuse",as="tools"); tools.pause(ms=20)' --dry-run
```

用自有夹具验证实际行为、超时和取消，不接管无关窗口。共享源码与必要的小资源，别把凭据、聊天记录或二进制放进仓库。
README 写清安装 revision、导入路径、必需参数、后端要求与实际验证结果。

## 发布版本

确认目标仓库和 `origin`，更新 manifest 版本并提交已审查源码。先生成具体发布计划：

```sh
zuse pkg publish --dry-run
```

用户已经授权发布时，检查计划后直接执行；否则先完成验证和计划，再请求发布授权。

```sh
zuse pkg publish
gh repo edit OWNER/REPO --add-topic zuse --add-topic zuse-package
```

发布只推送 `vVERSION` 标签，不创建仓库或推送分支，不能覆盖已有不同源码的标签。
GitHub 主题用于发现包，不代表质量或权限保证。
从新的消费项目安装刚发布的 revision，检查锁定提交、导出目录和一次无输入的调用，再报告仓库、版本和验证结果。
后续变更发布新版本；消费者显式更新锁文件，导入过程不联网更新。
