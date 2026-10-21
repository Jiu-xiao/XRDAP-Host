# XRDAP-Host

[English](README.en.md)

[XRDAP](https://github.com/Jiu-xiao/XRDAP-ESP) 调试探针的电脑端工具 `xrdap`，支持 Windows 和 Linux。工具是一个可执行文件：双击或不带参数运行时打开图形界面，带命令运行时作为命令行工具。

它提供以下功能：

- 按变量名读写目标 MCU 的内存，由探针按固定周期采样变量，实时绘制曲线并导出 CSV。变量的地址和类型取自 ELF 中的调试信息。
- 把探针共享给 Keil、pyOCD、probe-rs、OpenOCD，使经串口连接的探针也能作为标准 USB CMSIS-DAP 探针使用。
- 停住、继续、复位目标内核；配置探针的 Wi-Fi。

在自己的程序中调用这些功能时，使用 Rust 库 [`xrdap`](xrdap/README.md)。探针与主机之间的协议见 XRDAP-Core 的[协议文档](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md)。

## 下载

在 [Releases](../../releases) 下载对应平台的文件，改名为 `xrdap`（Windows 上为 `xrdap.exe`）并放进 PATH：

| 平台 | 文件 |
|---|---|
| Windows x86-64 | `xrdap-windows-x86_64.exe` |
| Linux x86-64 | `xrdap-linux-x86_64` |

校验和在同一页的 `SHA256SUMS` 中。Linux 上的图形界面需要系统安装中文字体（Noto Sans CJK 或文泉驿）。普通用户访问 USB 探针需要以下 udev 规则：

```
SUBSYSTEM=="usb", ATTR{idVendor}=="1d50", ATTR{idProduct}=="6199", MODE="0666"
```

## 选择探针

每个连接探针的命令都用 `-p` 指定探针；只有一块探针时可以省略。`xrdap list` 列出找到的探针以及每种连接对应的 `-p` 写法：

```
xrdap list
xrdap list --ask-serial
```

`list` 列出 USB 探针、局域网上经 mDNS 广播的探针，以及按 USB 描述符认出的乐鑫芯片自带 USB 口（ESP32-C3 的 USB Serial/JTAG）。其他串口只列出名字和串口芯片，不打开；加 `--ask-serial` 时逐个打开这些串口发送 DAP_Info 询问，打开串口可能使接在上面的开发板复位。

| `-p` 的写法 | 含义 |
|---|---|
| 不写 | 只有一块探针时使用它（在 USB、局域网和乐鑫 USB 口中查找） |
| `usb`、`usb:<序列号>` | USB |
| `COM3`、`/dev/ttyUSB0`、`serial:<串口>` | 串口。波特率按串口芯片选择：CH343 等先试 4000000，CP2102、CH340 用 921600，其他芯片两个都试；`--baud` 可以指定 |
| `192.0.2.10`、`192.0.2.10:3241`、`xrdap-aabbcc.local`、`tcp:<主机>` | TCP（探针的 Wi-Fi，或 `xrdap share openocd`） |
| `XRobot-XRDAP-AABBCCDDEEFF`、`DDEEFF` | 序列号全称或末尾几位 |

同一时间只有一条连接（USB、串口、TCP 之一）使用探针；另一条连接的命令得到“忙”的回复，`xrdap` 显示“探针正被另一条连接使用”，DAP_Info 照常回答。`xrdap` 只停止自己开始的采样。探针固件的厂商扩展要是协议版本 2（见协议文档），版本 1 的探针需要先升级固件。

打开串口时 DTR 和 RTS 都保持无效，与乐鑫 idf_monitor 相同；ESP32 开发板的自动下载电路用这两根线控制 EN 和 GPIO0。接在 USB 转串口芯片上的探针要用串口名指定。

## 按场景使用

### 查看变量曲线并在运行中修改参数

1. 运行 `xrdap`（不带参数）。左上“找到的探针”中点选一行，把它填入“探针”一栏（也可以直接填写，写法同 `-p`），再点“连接”。界面显示内核型号和运行状态。
2. 在“ELF”一栏选择目标程序的 ELF（编译时带 `-g`）。ELF 在后台读取。
3. 在变量列表中搜索变量名，点 `+` 加入监视表；也可以在表达式框中写 `motor[1].pid->out` 这类表达式后点“添加”。结构体和数组展开为成员。
4. 周期框填 `1ms`，点“开始采样”，曲线开始滚动。
5. 在监视表的写入框中填值并按回车修改变量；点“录制 CSV…”开始记录。

采样由探针定时，周期不受 USB 或 Wi-Fi 往返延迟的限制。探针不估计耗时，跟不跟得上看运行时的计数器：跳拍说明 SWD 跟不上，丢帧或缓冲接近满说明读取跟不上；这时界面在统计行下方给出建议（加长周期、加分频或减少变量）。曲线可以分到 1 到 4 张图（时间轴联动），悬停显示读数，单击和 Shift+单击放置 A、B 两个游标。探针、ELF、监视表等设置在退出时保存。采样期间不能重新读取 ELF。

### 在命令行读写变量、导出 CSV

```
xrdap vars --elf app.elf motor
xrdap read --elf app.elf motor.speed motor.state
xrdap read 0x20000010:u32
xrdap write --elf app.elf motor.kp=1.5 mode=RUN
xrdap watch --elf app.elf motor.speed motor.current@10 --period 1ms --csv log.csv --duration 10
xrdap watch -p COM3 --elf app.elf motor.speed@5:2 --period 500us --no-overwrite --all
```

`vars` 列出 ELF 中名字含指定文字的全局变量。`read` 读一次；`write` 写入后读回；`watch` 由探针按周期采样：`变量@10` 表示每 10 拍采一次，`变量@5:2` 同时指定相位，不写 `--duration` 时采样到 Ctrl-C。`--no-overwrite` 使缓冲满后丢弃新帧（默认覆盖最旧的帧），`--all` 在终端打印每一帧。`--csv` 的每一行是一帧：主机时间、探针时间、节拍号，然后每个变量一列，不到期的格为空，读取失败为 `ERR`。

变量的写法：`counter`、`state.speed`、`motor[1].pid->out`、`*ptr`、`p->q->x`、`(uint32_t***)p`，或原始地址 `0x20000010:u32`。不带 `--elf` 时只能使用原始地址。

### 用 Keil、pyOCD、probe-rs、OpenOCD 调试

| 探针的连接 | 方法 |
|---|---|
| ESP32-S3 插 USB | 探针本身就是 USB CMSIS-DAP 探针，直接使用 |
| 经串口连接的探针（ESP32-C3、传统 ESP32 等） | `xrdap share usbip` 后用 USB/IP 挂载，见下 |
| Wi-Fi | 探针自己提供 USB/IP，直接挂载，见 [XRDAP-ESP](https://github.com/Jiu-xiao/XRDAP-ESP#23-走-wi-fi用-keilpyocdprobe-rsopenocd) |

把串口上的探针共享为 USB 探针：

```
xrdap share usbip -p COM3
```

另开一个终端挂载（Linux 上先执行 `sudo modprobe vhci-hcd`，并在命令前加 `sudo`；Windows 上先安装 [usbip-win2](https://github.com/vadimgrn/usbip-win2)）：

```
usbip attach -r 127.0.0.1 -b 1-1
```

系统中出现 `<产品名> CMSIS-DAP` 设备，各调试工具按 USB 探针使用，例如：

```
openocd -f interface/cmsis-dap.cfg -f target/stm32f4x.cfg
```

在 `xrdap` 的终端按 Ctrl-C 停止共享，对调试工具来说相当于拔掉探针。图形界面中连上探针后勾选“共享为 USB DAP”效果相同，取消勾选后探针交回界面。给局域网中另一台电脑使用时加 `--listen 0.0.0.0:3240`，那台电脑执行 `usbip attach -r <本机 IP> -b 1-1`。

### 让 OpenOCD 使用串口上的探针（不安装 USB/IP）

```
xrdap share openocd -p COM3
xrdap share openocd -p COM3 --listen 0.0.0.0:3241
```

`xrdap` 在 127.0.0.1:3241（或 `--listen` 指定的地址）提供 OpenOCD 的 CMSIS-DAP TCP 后端，并打印 OpenOCD 使用的配置：

```
openocd -c "adapter driver cmsis-dap; cmsis-dap backend tcp; cmsis-dap tcp host 127.0.0.1; cmsis-dap tcp port 3241; transport select swd" -f target/stm32f4x.cfg
```

TCP 后端只在 OpenOCD 的 master 中，见“OpenOCD 版本”。

### 配置探针的 Wi-Fi

```
xrdap wifi status
xrdap wifi set -p COM3 lab 12345678
xrdap wifi set -p COM3 lab
xrdap wifi clear -p COM3
```

`status` 显示保存的热点、是否连上、IP 和信号强度，以及最近一次清除热点参数的来源（板上按键、串口命令行、0x86 清除命令或 NVS 修复）、本次启动的复位原因和 NVS 是否在本次启动时重建过。`set` 保存热点名和密码（开放网络省略密码），`clear` 清除热点参数（请求带确认字节）；两者之后探针约 0.5 秒后重启，连接随之断开。配置完成后 `xrdap list` 列出探针的局域网地址（`xrdap-xxxxxx.local`），之后可以用 `-p xrdap-xxxxxx.local` 无线连接。

图形界面中连上探针后展开连接区最下方的“Wi-Fi”，显示当前状态，填写热点名和密码后点“保存并重启”，或点“清除”。

### 停住、继续、复位目标

```
xrdap info
xrdap info --no-target
xrdap halt
xrdap run
xrdap reset
xrdap reset --halt --clock 1M
```

`info` 显示探针信息（DAP_Info、厂商扩展 INFO），并连接目标读取 DPIDR 和内核型号；`--no-target` 只查询探针。`reset --halt` 复位后停在第一条指令之前，复位结束时 DEMCR 恢复原值。`--clock` 指定 SWD 时钟（默认 4M）。图形界面中的“停住/继续”“复位”“复位并停住”按钮作用相同，采样期间也可以使用。

## 参考

### 命令和选项

每个命令只接受自己用到的选项，选项写在命令之后；`xrdap 命令 -h` 显示完整参数。

| 命令 | 作用 | 选项 |
|---|---|---|
| （不带命令） | 图形界面 | |
| `list` | 列出探针 | `--ask-serial` |
| `info` | 探针和目标信息 | 探针选项、`--clock`、`--no-target` |
| `vars [文字]` | 列出 ELF 中的全局变量 | `--elf`（必需） |
| `read 变量...` | 读变量 | 探针选项、`--clock`、`--elf` |
| `write 变量=值...` | 写变量并读回 | 探针选项、`--clock`、`--elf` |
| `watch 变量[@分频[:相位]]...` | 探针端周期采样 | 探针选项、`--clock`、`--elf`、`--period`、`--csv`、`--duration`、`--no-overwrite`、`--all` |
| `halt`、`run`、`reset` | 内核控制 | 探针选项、`--clock`；`reset` 另有 `--halt` |
| `wifi status`、`wifi set 热点 [密码]`、`wifi clear` | 探针 Wi-Fi | 探针选项 |
| `share usbip`、`share openocd` | 把探针共享给其他调试工具 | 探针选项、`--listen` |

探针选项：`-p` 探针、`--baud` 串口波特率（默认按串口芯片选择）、`--timeout-ms` 等待一个响应的超时（默认 2000）。

### USB/IP 客户端

| 系统 | 安装 | 挂载 | 卸载 |
|---|---|---|---|
| Linux | 内核自带：`sudo modprobe vhci-hcd`；Ubuntu 的 `usbip` 命令在 `linux-tools-generic` 包中 | `sudo usbip attach -r <地址> -b 1-1` | `sudo usbip detach -p 0` |
| Windows | [usbip-win2](https://github.com/vadimgrn/usbip-win2) | `usbip attach -r <地址> -b 1-1` | `usbip detach -p 0` |

同一时间只服务一个客户端。调试工具退出后设备仍处于挂载状态，`usbip detach` 之后才算拔掉；共享停止时设备被拔掉。客户端断开时，探针如果还在采样，`xrdap` 停止采样。探针读写出错（超时、断开）时 `xrdap` 断开该客户端并显示原因。

### OpenOCD 版本

- 经 USB 或 USB/IP 使用探针时走 OpenOCD 的 USB 后端，正式版 0.12.0 即可。
- `cmsis-dap backend tcp`（`share openocd` 和 Wi-Fi 直连）不在 0.12.0 中，需要从 master 编译。Windows 上要包含提交 e89e73839d53，更早的 master 在 Windows 上把“暂时没有数据”当作错误，启动时等待十几秒并反复输出 `error reading header`。

### 从源码编译

```
cargo build --release -p xrdap-tool
```

生成 `target/release/xrdap`。Linux 上先安装 `libgtk-3-dev libxkbcommon-dev libudev-dev`。

| 目录 | 内容 |
|---|---|
| `xrdap` | Rust 库，见 [xrdap/README.md](xrdap/README.md) |
| `xrdap-tool/src/cli` | 命令行：选项组、各命令 |
| `xrdap-tool/src/gui` | 图形界面：后台线程、各面板、曲线 |
| `xrdap-sim` | 测试用的探针模拟器，不随程序发布 |

```
cargo test --workspace
```

测试不需要硬件。`xrdap-sim` 依据 CMSIS-DAP 规范和协议文档模拟探针与目标，经 TCP 提供和探针相同的帧格式；测试覆盖帧编解码、厂商命令、采样、DWARF 解析、各个命令和共享。CI 还检查 `cargo fmt` 和 `cargo clippy`。

## 许可证

Apache-2.0，见 [LICENSE](LICENSE)。
