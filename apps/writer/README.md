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

### 从 Linux 发布包安装

从 [GitHub Releases](https://github.com/LIghtJUNction/zuse/releases) 下载与机器架构匹配的 `x86_64-unknown-linux-gnu` 或 `aarch64-unknown-linux-gnu` 压缩包。解压后进入含有 `zuse` 和 `zuse-writer` 的目录，运行 `./zuse-writer` 即可打开桌面程序。两个文件放在一起，Writer 可以找到同一版本的引擎。

要从应用菜单启动，将程序和菜单项安装到当前用户的目录。桌面会话的 `PATH` 需要包含 `$HOME/.local/bin`。

~~~sh
install -Dm755 zuse "$HOME/.local/bin/zuse"
install -Dm755 zuse-writer "$HOME/.local/bin/zuse-writer"
install -Dm644 apps/writer/zuse-writer.desktop \
  "${XDG_DATA_HOME:-$HOME/.local/share}/applications/zuse-writer.desktop"
~~~

在应用菜单搜索 **Zuse Writer** 或 **Zuse 写字机**。需要保留旧会话时，在解压目录运行 `ZUSE_BIN="$PWD/zuse" ZUSE_WRITER_SESSION=writer-new ./zuse-writer`，并将 `writer-new` 换成尚未使用的会话名。新会话与原会话分别管理各自的作业和设备连接。

## 连接第一台写字机

把写字机插入 USB 并打开 Zuse Writer。设备区会列出系统识别到的 USB 串口。只有一个候选时，界面会预先选中它；有多个候选时，可以按设备名称、端口和 USB 标识选择。插拔后可重新扫描，也可以手动输入设备路径。

以已验证的 BtWriter 为例，Linux 将它的 USB 转串口芯片识别为 `1a86:7523`，端口为 `/dev/ttyUSB0`。选中该设备，波特率保持 **115200**，点「连接」。连接后界面会显示固件实际返回的机型、版本和坐标。这个 USB 标识也用于其他设备，连接前可用它辨认转接芯片，具体机型以连接回执为准。

## 三步校准纸面和装笔

打开「对笔」，也可以运行 `zuse-writer --align`。

1. 取下笔或松开笔夹。用低速 XY 点动把笔夹移到纸张可写范围的左上角，留出夹具间隙，点「记录左上角」。
2. 移到可写范围的右下角，点「记录右下角」。程序用两个角点设定共享会话的 XY 边界、纸张起点、宽高及反转方向。
3. 在当前 XY 点「定位装笔」。已识别且使用零工作偏移的 BtWriter 只沿 Z 低速移动到 Z=5 mm。到位后放下笔尖至刚好贴纸，锁紧笔夹，点「已装笔，保存校准」。抬笔位为 Z=0 mm。

下次点「载入已保存的校准」。只需确认实体原点、纸张和笔的位置未变，再应用档案，载入期间机器不会移动。撞边、断电、移纸或松动笔夹后，重新核对实体位置。坐标偏移读数相同也可能对应不同的实体位置。

保存或载入后可用「回到左上角」「回到右下角」「回到中心」「抬笔」「落笔」。回到纸面位置时先抬笔，再移动 XY。Writer 的写字作业要求已安装边界及已校准笔高，执行前核对整条轨迹。

## 用写字机书写

1. 连接后刷新状态。机器坐标来自控制器。控制器返回工作坐标或同帧的工作偏移时，界面也会显示工作坐标。缺失值显示为空。
2. 完成上面的装笔流程，确认抬笔和落笔的工作 Z 值。纸面上的落字位置由「起点 X mm」「起点 Y mm」决定。在默认轴向下，字形向右沿 X 增大、向下沿 Y 减小。假设范围为 X 20–100、Y 40–90 mm，以 (25, 85) 开始可在左边和上边各留 5 mm。字号、总行宽及随机偏移仍需能放进纸面。
3. 输入文字，选择单线字体、字号、字距和行距。在校准页配置基准距离与坐标轴方向。随机偏移按整个字符平移，横向和纵向分别控制，用种子复现同一版排字。
4. 更新预览后再导出或发送。画布取自当前 G-code 的落笔轨迹，导出和发送使用同一份已生成的程序。改变排字或校准参数后需要重新生成预览。要安装或移除字体，使用 `zuse font add NAME FILE` 和 `zuse font remove NAME`。
5. “校验程序”使用已支持的 GRBL 控制器校验模式。固件解析每行但不驱动坐标轴；退出时固件会自行软复位。“发送到写字机”执行运动程序。程序会等待逐行确认及最终 Idle。取消会阻止继续派发，机器已接收的运动仍由控制器处理。

## 用鼠标、触摸屏或虚拟触摸板书写

在写字页的「输出到」选择「屏幕输入」，再选择「鼠标」「触摸屏」或「虚拟触摸板」。左侧的文字、字体、字号、字距、行距、整字偏移和随机种子沿用当前排版。屏幕起点用像素表示，Y 向下增大。「比例」表示一个字形单位在屏幕上对应的像素数。例如字号 10、比例 4，对应 40 px 的字形框。

1. 填写目标画布的屏幕起点。触摸输出还需要逻辑触摸面的宽和高。
2. 点「预览屏幕笔迹」核对字形。触摸坐标必须在逻辑触摸面以内。
3. 点「发送到画布」。在三秒内切换到目标绘图或手写应用。倒计时结束前可取消。鼠标使用系统指针。Linux 的触摸输出通过可写的 `/dev/uinput` 创建设备；触摸屏发送直接触点，虚拟触摸板发送间接触点，指针映射和笔迹接收由系统与目标应用决定。

例如，当前目标画布位于屏幕 (200, 150) 处时，可以先在命令行执行 `zuse -c 'write.preview(text="Z",x=200,y=150,size=40)'` 查看顶点。同一字体可通过 `write.mouse`、`write.touchscreen`、`write.touchpad` 输出。更多参数及本地试运行见[笔迹与字体说明](../../docs/handwriting.md)。

## 与 AI 共用一台机器

界面默认使用会话 `writer`。可以用 `ZUSE_WRITER_SESSION` 更改名称。另一个终端或具备本机命令执行权限的 AI 可以查询该会话，读取真实设备状态，并按返回的作业号取得回执。

~~~sh
zuse session status writer
printf '%s\n' 'plotter.status()' | zuse session observe writer -
# 将 JOB_ID 替换为上一条命令返回的 job_id
zuse session result writer JOB_ID
~~~

设备路径由 GUI 或同一会话中的 `plotter.connect(port="…",baud=115200)` 选定。查看真实工作坐标、偏移和当前行程边界，可以分别提交 `plotter.status(work=true,offset=true)` 和 `plotter.bounds()`。支持 `plotter.jog`、`plotter.pen`、`plotter.check`、`plotter.send` 和 `plotter.disconnect`。一个作业运行期间，其他客户端可以查状态和结果；竞争作业会收到 busy。需要取消时先核对目标作业与最新修订号，再用 `zuse session cancel writer --expect-revision REVISION`。界面也可观察和取消外部客户端发起的作业。

[笔迹、字体与机器参数](../../docs/handwriting.md) · [会话协议](../../docs/session-protocol.md)

在「校准参数」页设置书写宽度和高度。两者使用与字高相同的单位，应用参考比例前计算。宽度设定后自动换行，超出高度时停止生成并提示行号。填 0 取消该方向的版面限制。默认逐字左右、上下各随机偏移字高的 ±3.5%，倾斜 ±3°，字号变化 ±4%。四项各自可以设为 0。字距和行距以相邻字和相邻行的起点测量。

可在底部文件栏输入 UTF-8 文章的路径，点「读文本」保留原有段落。
AI 可在相同的 `write.gcode` 和 `write.export` 操作中传入 `area_width`、`area_height`、`advance`、`line_height`、`offset_x_ratio`、`offset_y_ratio`、`tilt_degrees`、`size_ratio` 和 `seed` 来复现排版。

## 字体、文章与纸张

点「字体库」浏览已安装的字体，或从 [Zuse 字体 Release](https://github.com/LIghtJUNction/zuse/releases/tag/fonts) 一键安装中文楷体和两款英文单线字体。本地字体使用 UTF-8 `.zfont` 笔画文件，在同一页填写名称和路径即可导入。混排按所选字体的顺序补齐缺字。例如先选择 `hanzi-kaiti`，再选择 `latin-script`。使用 `zuse font available` 枚举官方字体，`zuse font install` 安装，`zuse font list` 检查本地字体。

在 Linux 发布包目录中，可以用 Python 3 将有权使用的本机未加密字库（`.gfont` 版本 5、6 或 9）导入 Writer。以下命令生成新字体，再用同一发布包里的 Zuse 安装。

~~~sh
python3 scripts/import_gfont.py my-font.gfont --output my-font.zfont
./zuse font add local-pen my-font.zfont
./zuse-writer --kuixiang-page page_settings.json "信纸"
~~~

只需部分字形时，可在导入命令后加 `--text "你好"`。纸张预设名称取自你自己的 JSON，只有一份预设时可省略名称。Writer 读取毫米纸宽、纸高及四边页边距。已校准的纸面使用实测宽高，只采用预设的页边距。字号、字距、轴向与机台参数继续使用 Writer 中的值。

「编辑文章」支持多行输入和粘贴，也可以在底部导入 UTF-8 文本。预览显示纸张、页边距和最终笔迹。宽度决定自动换行，超出可写高度时提示行号。默认纸张 150 × 200，四边各留 5 个单位。使用无界宽高时，可将该方向的末端边距设为 0。

GUI 使用同一个进给值控制走笔、空移和抬落笔。通过 AI 接口可传入 `travel_feed`、`lift_feed` 自行设置移动速度。点击停止时，应用请求固件使运动停稳，并区分已停、空闲和无法确认。若固件进入 Hold，需清除旧队列后重新校准，不要恢复旧作业。

「复位控制器」需要再次点击确认。固件启动块不可验证时，程序拒绝发出复位。复位会使先前的坐标标定失效。普通手动点动没有固定步长上限，已示教行程与作业时间限制仍然有效。

「回到左上角」先抬笔再返回标定的左上角，并回读到位坐标。需要本次会话已应用机台行程，并确认抬笔 Z 和纸张起点。AI 可使用 `plotter.park(x=…,y=…,z=…)`。

记录左上角或右下角时，先填入 XY 点动速度（40–500 mm/min），再点「启用键盘方向键」。按住 ← / → 沿 X 轴负向 / 正向移动，按住 ↑ / ↓ 沿 Y 轴正向 / 负向移动。使用系统键盘自动重复，松键后停止续发。点击速度框或按 Tab 会停用方向键，修改后可再次启用。

每段点动都等待实际坐标回执。松键时已经提交的一段仍可能完成，40、300、500 mm/min 分别对应 0.16、1.2、2 mm 指令行程。单击屏幕轴按钮使用所选步长，长按则使用同样的小段，移出按钮也会停止续发。校准时请通过软件移动笔头，使两角之间的距离计入控制器坐标。
