# Zuse Writer · 写字机

Zuse Writer 是用 zgpui 构建的原生桌面程序。窗口和外部 AI 客户端都把作业提交到同一个命名 Zuse 会话。该会话持有写字机串口，记录当前作业、状态修订号和执行回执。

## 运行

共享控制使用 Unix 域套接字，本版已在 Linux 上验收。在 Zuse 仓库根目录执行以下命令。两个可执行文件安装在同一目录。Linux 使用 zgpui 的原生 X11 窗口，在 Wayland 桌面上经 XWayland 运行。

~~~sh
cargo install --locked --path .
cargo install --locked --path apps/writer
zuse-writer
~~~

也可在源码目录编译。设置 `ZUSE_BIN` 可指定本次构建的 `zuse` 绝对路径。引擎默认按环境变量、应用程序同目录、系统 PATH 的顺序查找。若程序提示没有 `plotter.*` 操作，请使用与此桌面程序配套的引擎。

## 连接第一台写字机

把写字机插入 USB 并打开 Zuse Writer。设备区会列出系统识别到的 USB 串口。只有一个候选时，界面会预先选中它；有多个候选时，可以按设备名称、端口和 USB 标识选择。插拔后可重新扫描，也可以手动输入设备路径。

以已验证的 BtWriter 为例，Linux 将它的 USB 转串口芯片识别为 `1a86:7523`，端口为 `/dev/ttyUSB0`。选中该设备，波特率保持 **115200**，点「连接」。连接后界面会显示固件实际返回的机型、版本和坐标。这个 USB 标识也用于其他设备，连接前可用它辨认转接芯片，具体机型以连接回执为准。

## 书写

1. 连接后刷新状态。机器坐标来自控制器。控制器返回工作坐标或同帧的工作偏移时，界面也会显示工作坐标。缺失值显示为空。
2. 用低速点动确定纸面方向和笔高，分别记录抬笔和落笔的工作 Z 值。支持落笔时 Z 增大或减小的机构。不同机器的这两个值可能相反。
3. 输入文字，选择单线字体、字号、字距和行距。在校准页配置基准距离与坐标轴方向。随机偏移按整个字符平移，横向和纵向分别控制，用种子复现同一版排字。
4. 更新预览后再导出或发送。画布取自当前 G-code 的落笔轨迹，导出和发送使用同一份已生成的程序。改变排字或校准参数后需要重新生成预览。要安装或移除字体，使用 `zuse font add NAME FILE` 和 `zuse font remove NAME`。
5. “校验程序”使用已支持的 GRBL 控制器校验模式。固件解析每行但不驱动坐标轴；退出时固件会自行软复位。“发送到写字机”执行运动程序。程序会等待逐行确认及最终 Idle。取消会阻止继续派发，机器已接收的运动仍由控制器处理。

## 与 AI 共用一台机器

界面默认使用会话 `writer`。可以用 `ZUSE_WRITER_SESSION` 更改名称。另一个终端或具备本机命令执行权限的 AI 可以查询该会话，读取真实设备状态，并按返回的作业号取得回执。

~~~sh
zuse session status writer
printf '%s\n' 'plotter.status()' | zuse session observe writer -
# 将 JOB_ID 替换为上一条命令返回的 job_id
zuse session result writer JOB_ID
~~~

设备路径由 GUI 或同一会话中的 `plotter.connect(port="…",baud=115200)` 选定。支持 `plotter.jog`、`plotter.pen`、`plotter.check`、`plotter.send` 和 `plotter.disconnect`。一个作业运行期间，其他客户端可以查状态和结果；竞争作业会收到 busy。需要取消时先核对目标作业与最新修订号，再用 `zuse session cancel writer --expect-revision REVISION`。界面也可观察和取消外部客户端发起的作业。

[笔迹、字体与机器参数](../../docs/handwriting.md) · [会话协议](../../docs/session-protocol.md)
