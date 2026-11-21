# Zuse 会话协议 v1

本协议让外部控制器向一个持续存在的 Zuse Runtime 提交源码、查询状态和取消工作。
连接端点由 `zuse session start NAME` 的 `socket` 字段给出。服务在本机 Unix domain socket
上交换 UTF-8 JSON-lines：每个请求和响应各占一行，以 `\n` 结束；源码中的换行必须进行 JSON 转义。
原生观察返回的嵌套 JSON 会去除结构空白后传输，字符串内的转义和结果对象类型保持不变。
同一连接可依次发送多个请求，服务也允许其他客户端读取状态。
连接闲置或一个未完成帧达到 5 秒会关闭连接；需要时重新连接同一 socket，已有会话与作业仍保留。
超长帧返回 `frame_too_large` 后断开；不完整、非 UTF-8 或超时帧返回 `invalid_request` 后断开。

## 三个标识

| 字段 | 含义 |
| --- | --- |
| `request_id` | 控制器给这次请求分配的字符串标识，用于响应关联和有界重试去重。 |
| `job_id` | 服务接收脚本后分配的整数作业标识，用于查询执行结果。 |
| `state_revision` | 会话状态变化计数，初始为 1；用于拒绝依据旧状态提交的动作。 |

不同操作使用不同 `request_id`。只有同一次请求的重试才复用原 ID 和完整帧内容。
ID 必须为 1–128 个 UTF-8 字节，不能包含控制字符。
`request_id` 不是认证凭证；能访问私有 socket 的本地进程可以控制会话。

## 请求

所有请求都包含 `version: 1`、`request_id` 和 `op`。例如：

```json
{"version":1,"request_id":"status-001","op":"status"}
```

| `op` | 额外字段 | 行为 |
| --- | --- | --- |
| `status` | 无 | 查询状态及当前、最近作业。 |
| `script` | `source`；可选 `base`、`options`、`expected_revision` | 提交完整 Zuse 程序。 |
| `observe` | 同 `script` | 提交强制只读程序。 |
| `result` | 可选整数 `job_id` | 查询指定或最近作业。 |
| `cancel` | 可选 `expected_revision` | 请求取消当前作业。 |
| `close` | 可选 `expected_revision` | 清理运行时并关闭会话。 |

`source` 是 UTF-8 源码字符串。`base` 如提供，必须是绝对目录，用于解析相对模块路径。
原始协议省略 `base` 时，使用服务启动时冻结的工作目录；CLI 自动传入本次控制命令的工作目录。
请求对象和 `options` 对象拒绝重复或未知字段；字段也必须用于表中允许的操作。
`options` 是可选对象，包含以下执行设置：

| 字段 | 类型 | 默认值 |
| --- | --- | --- |
| `read_only` | 布尔 | `false`；`observe` 始终强制为 `true`。 |
| `dry_run` | 布尔 | `false`。 |
| `max_steps` | 整数，1–65536 | 256。 |
| `max_wait_ms` | 整数，0–86400000 | 60000。 |

例如提交纯 Zuse 等待程序，并要求状态仍与之前观察一致：

```json
{"version":1,"request_id":"script-001","op":"script","expected_revision":1,"source":"fn inspect(){timer.status()} inspect()","options":{"max_steps":256,"max_wait_ms":60000}}
```

`expected_revision` 是可选整数。匹配当前状态修订号才接受相应动作；不匹配返回 `stale_revision`，
不会执行源码。它保护从观察到决策之间的状态关联，不是整个外部应用界面的事务锁。

## 响应

响应包含以下公共字段：

| 字段 | 含义 |
| --- | --- |
| `version` | 响应协议版本，当前为 1。 |
| `request_id` | 对应请求标识；无法读取有效标识的帧错误为 `null`。 |
| `ok` | 请求是否被协议层成功处理。 |
| `session` | 会话名。 |
| `state` | `idle`、`running`、`cancelling` 或 `closing`。 |
| `state_revision` | 该回执所记录的状态修订号。 |

成功响应另外包含 `status` 和操作相关字段。脚本提交响应使用
`status: "accepted"` 并返回整数 `job_id`。这只表示收到作业，不能作为动作已完成的证据。

`status` 操作使用 `status: "ready"`，包含 `pid`、`socket`、`active_job_id`、
`last_job_id` 和 `last_status`。状态修订号在接受作业、首次取消、完成和进入关闭状态时递增；
单纯 `status` / `result` 查询不会递增它。
`active_job_id` 在空闲时为 `null`；`last_job_id` / `last_status` 指最近结束的作业，没有时为 `null`。
省略 `result.job_id` 时优先查询当前作业，否则查询最近结束的作业。

`result` 操作的 `status` 为：

| 值 | 含义 |
| --- | --- |
| `running` | 作业尚未结束。 |
| `completed` | 作业已结束，读取 `result` 中的 Runtime 回执。 |
| `failed` | 准备或执行失败，读取结果中的错误与已完成步骤。 |
| `cancelled` | 作业因取消结束；读取内部回执中的清理信息和错误。 |
| `empty` | 没有可返回的作业结果。 |

终态结果包含 `job_id` 和 `result`，后者保留 Zuse 原执行回执。
同时返回 `elapsed_ms`、`result_bytes` 与 `result_truncated`。超过 512 KiB 的回执不保留正文：
`status` 为 `failed`、`result` 为 `null`、`result_truncated` 为 `true`，`result_bytes` 记录原大小。
这表示结果超限，不能推断操作未发生；应重新观察应用，不因缺少正文而自动重复输入。
`dry_run` 的作业也会结束，但其内部回执是检查计划，不表示输入发生。
`ok: true` 与 `status: "failed"` 可以同时出现：前者说明结果查询成功，后者说明被查询的作业失败。

## 状态和并发

一个会话同一时刻只运行一个作业，`script` 和 `observe` 都需要 `idle`。
运行中仍可发送 `status`、`result`、`cancel` 和 `close`。
取消请求进入 `cancelling`，清理完成后回到 `idle`；关闭进入 `closing`，清理完成后返回 `closed` 并断开服务。
取消响应含 `cleanup_complete`：有作业时为 `false`，已经空闲时返回 `status: "idle"` 和 `true`。
`cancelling` 只确认取消标志已设置并唤醒等待；不代表正在执行的原生输入派发已经返回。
关闭响应含 `resources_released`。等待清理超过 10 秒时可返回 `status: "closing"` 和 `false`；
只有 `status: "closed"` 和 `true` 才确认运行时及原 socket 已清理。
当前协议不提供暂停、恢复执行位置或回滚已经送达应用的动作。

作业使用保留的 Runtime 执行 flat 程序。数值程序先展开成原语；函数和 Runtime 字段可以跨作业保留。
当前通道不执行 scoped mouse 程序；其 `dry_run` 可以检查计划，但不创建跨作业 scoped 设备。

## 重试与去重

服务对 `script`、`observe`、`cancel`、`close` 保存有界去重记录。
服务仍可响应且缓存命中时，相同 `request_id` 和相同帧内容返回原回执，并附加 `replayed: true`，不会再次执行。
同一个已缓存 ID 对应不同帧内容时返回 `request_id_conflict`。帧比较不包含末尾换行，但包含 JSON 字段顺序和空白。
重试应重发原帧，不重新排列、修改或重建其中的状态条件。

动作的拒绝回执也会缓存，包括 `busy` 和 `stale_revision`。收到确定拒绝后，新的提交尝试必须用新 ID；
通信结果不确定时才用原 ID 和原帧找回原回执。`status` / `result` 不占去重缓存，读取请求可以复用 ID。

去重检查先于旧 `expected_revision` 的重新判定。因此已接收作业完成后，重发其原请求仍返回原作业 ID。
重放的是历史回执，其中 `state` 和 `state_revision` 不代表当前状态；后续决策先读取新的 `status` 或 `result`。
缓存最多保留 64 条且总数据最多 8 MiB，淘汰或会话重启后不保证同 ID 仍能去重。
保存并重发原 `expected_revision` 可使已淘汰的旧动作在状态变化后被拒绝。
会话重启后修订号重新计数；同名会话或相同修订号不表示原来的进程，应重新观察后再提交。

## 错误

协议错误使用 `ok: false`，并包含 `error: {"code": "...", "message": "..."}`。

| `code` | 处理方式 |
| --- | --- |
| `busy` | 当前作业尚未结束；查询结果或显式取消，新的提交尝试使用新 ID。 |
| `stale_revision` | 重新观察并取得当前修订号，再以新 ID 提交新的决定。 |
| `request_id_conflict` | 为新请求换新 ID；原请求重试保持原帧。 |
| `invalid_request` | 修正请求字段、操作名、参数或源码大小。 |
| `invalid_result` | 运行时结果无法编码为合法协议 JSON；重新观察状态，避免盲目重放输入。 |
| `unsupported_version` | 使用已支持的协议版本。 |
| `job_not_found` | 核对作业 ID；结果可能已超出保留窗口。 |
| `session_closing` | 停止提交，等待关闭完成。 |
| `frame_too_large` | 缩小单帧，勿继续发送超限帧。 |
| `request_pending` | 同 ID 的原请求仍在完成；稍后重发原帧。 |
| `worker_unavailable` | 会话执行线程不可用；读取现有结果并关闭或重新建立会话。 |

连接错误、EOF 或客户端超时不能证明作业未执行。先用原请求 ID 重试或查询已知作业 ID，
避免盲目创建新请求而重复输入。Zuse CLI 对协议请求错误返回非零退出码；查询一个失败作业本身仍可成功退出。

## 限额与生命周期

| 项目 | 限额 |
| --- | ---: |
| 单次源码 | 256 KiB UTF-8 字节 |
| 单个协议帧 | 2 MiB |
| 单条保留结果 | 512 KiB |
| 保留作业结果 | 32 条 |
| 请求去重记录 | 64 条，合计最多 8 MiB |
| 同时连接 | 16 条 |
| 完整帧接收或连接空闲期限 | 5 秒 |
| 响应写入超时 | 2 秒 |
| 单次关闭等待 | 10 秒 |

这些限额之外，脚本仍受自己的步数、等待、模块、展开和绘图点数预算约束。
服务的默认空闲超时是 900000 ms；有效控制请求续期，运行中的作业不计为空闲。
未完成帧和静默连接不会无限保持会话。关闭或超时会释放 Runtime，客户端不能假定函数与设备在重启后仍存在。
服务每 100 ms 检查读取状态；会话超时清理不等待未完成帧耗尽其 5 秒期限。
运行目录权限为 `0700`，socket 权限为 `0600`，两者均检查当前有效用户身份。

## 外部控制循环

1. 启动或连接明确的会话，用 `status` 确认它空闲。
2. 用 `observe` 提交观察源码，保存 `job_id`，查询到终态。
3. 根据返回的应用状态，由程序逻辑或模型选择下一步。服务不调用外部模型。
4. 将下一步写为 Zuse 源码，以新 `request_id` 和最近 `state_revision` 提交 `script`。
5. 查询执行结果并再次观察。发生 `stale_revision` 时回到观察；通信中断时优先重发原帧或查询原作业。

打地鼠、下棋或其他交互任务可以使用同一循环；目标选择与策略属于控制器，
坐标计算、时序和实际输入留在提交的 Zuse 程序里。接口不包含专用游戏逻辑或 JEV 集成。
