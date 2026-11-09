# 命令行

[English](cli.en.md)

本文是电脑端工具 `xrdap` 的参考：命令和选项、探针和变量的写法、采样的输出和计数器、Wi-Fi 配置，以及把探针共享给其他调试工具时的客户端和 OpenOCD 版本。图形界面使用相同的探针写法、变量写法和 CSV 格式。

## 命令和选项

每个命令只接受自己用到的选项，选项写在命令之后；`xrdap <命令> -h` 显示这个命令的完整参数。

| 命令 | 作用 | 选项 |
|---|---|---|
| （不带命令） | 打开图形界面 | |
| `list` | 列出探针 | `--ask-serial` |
| `info` | 探针信息，并连接目标读 DPIDR 和内核型号 | 探针选项、`--clock`、`--no-target` |
| `vars [文字]` | 列出 ELF 中名字含这段文字的全局变量 | `--elf`（必需） |
| `read 变量...` | 读变量 | 探针选项、`--clock`、`--elf` |
| `write 变量=值...` | 写变量并读回 | 探针选项、`--clock`、`--elf` |
| `watch 变量[@分频[:相位]]...` | 由探针按周期采样 | 探针选项、`--clock`、`--elf`、`--period`、`--csv`、`--duration`、`--no-overwrite`、`--all` |
| `halt`、`run`、`reset` | 停住、继续、复位目标内核 | 探针选项、`--clock`；`reset` 另有 `--halt` |
| `wifi status`、`wifi set 热点 [密码]`、`wifi clear` | 探针的 Wi-Fi | 探针选项；`set` 另有 `--password-stdin` |
| `share usbip`、`share openocd` | 把探针共享给其他调试工具 | 探针选项、`--listen` |

探针选项是 `-p`、`--baud` 和 `--timeout-ms`。

| 选项 | 含义 | 默认 |
|---|---|---|
| `-p`、`--probe` | 探针，写法见下一节 | 只有一块探针时使用它 |
| `--baud` | 串口波特率，与探针固件一致 | 按串口芯片选择，见下一节 |
| `--timeout-ms` | 等待一个响应的超时，毫秒 | 2000 |
| `--clock` | SWD 时钟，例如 `4000000`、`4M`、`500k` | `4M` |
| `--elf` | 目标程序的 ELF（编译时带 `-g`），按名字查找变量和类型 | 只能写原始地址 |
| `--no-target` | `info` 只查询探针 | 连接目标 |
| `--halt` | `reset` 后停在复位向量，第一条指令之前 | 复位后运行 |
| `--period` | 采样周期，例如 `1000`（µs）、`500us`、`2ms`、`2.5ms`、`1s` | `1ms` |
| `--csv` | 把每一帧写入这个 CSV 文件 | 打印到终端 |
| `--duration` | 采样的秒数 | 采样到 Ctrl-C |
| `--no-overwrite` | 缓冲满后丢弃新帧 | 覆盖最旧的帧 |
| `--all` | 终端中打印每一帧 | 最多每 0.1 s 打印一行 |
| `--password-stdin` | 从标准输入读一行作为密码 | |
| `--listen` | 共享的监听地址 | `127.0.0.1:3240`（usbip）、`127.0.0.1:3241`（openocd） |
| `--ask-serial` | `list` 逐个打开串口，发 DAP_Info 询问 | 只列出串口 |

`watch` 和 `share` 运行到 Ctrl-C 为止。第一次 Ctrl-C 让它们正常结束：`watch` 停止采样并读完缓冲中的帧，`share` 断开客户端；第二次 Ctrl-C 立即退出。

## 探针

### `-p` 的写法

| `-p` 的写法 | 含义 |
|---|---|
| 不写 | 只有一块探针时使用它 |
| `usb`、`usb:<序列号>` | USB |
| `COM3`、`/dev/ttyUSB0`、`serial:<串口>` | 串口 |
| `192.0.2.10`、`192.0.2.10:3241`、`xrdap-aabbcc.local`、`tcp:<主机>` | TCP：探针的 Wi-Fi，或 `xrdap share openocd`；端口默认 3241 |
| `XRobot-XRDAP-AABBCCDDEEFF`、`DDEEFF` | 序列号全称或末尾几位，不分大小写 |

不写 `-p` 和按序列号查找时，`xrdap` 在 USB、局域网（mDNS 服务 `_xrdap._tcp`）和乐鑫芯片自带的 USB 口（USB Serial/JTAG）中查找；接在 USB 转串口芯片上的探针用串口名指定。同一块探针经多条连接找到时，依次优先使用 USB、串口、TCP。`xrdap list` 列出每条连接和对应的 `-p` 写法。

### 串口

串口波特率要与探针固件一致。不写 `--baud` 时按串口芯片选择，依次尝试：

| 串口芯片 | 波特率 |
|---|---|
| WCH CH343 等、FTDI、其他芯片 | 4000000，然后 921600 |
| WCH CH340、CH341，Silicon Labs CP210x | 921600（传统 ESP32 的固件） |

乐鑫芯片自带的 USB 口与波特率无关。

`xrdap` 打开串口时把 DTR 和 RTS 都置为无效，与乐鑫 idf_monitor 相同：ESP32 开发板的自动下载电路用这两根线控制 EN 和 GPIO0。`xrdap list --ask-serial` 逐个打开串口发送 DAP_Info，打开串口可能使接在上面的开发板复位。

### 同一时间使用一条连接

探针同一时间只由一条连接（USB、串口、TCP 之一）使用，这由使用者保证：探针执行每条连接发来的命令，两条连接同时使用时，SWD 的状态互相干扰。另一条连接正在采样时，连接目标的命令得到 `FF`，`xrdap` 报告探针正在采样；开始采样的那条连接重新连接目标时，探针结束这次采样。规则的细节见 XRDAP-Core 的 [`docs/protocol.md`](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md) 第 3.1 节。

## 变量的写法

变量写成 `counter`、`state.speed`、`motor[1].pid->out`、`*ptr`、`p->q->x`，可以带 `(uint32_t***)p` 这样的类型转换，也可以写原始地址和类型，例如 `0x20000010:u32`。类型可以是 `u8`、`u16`、`u32`、`u64`、`i8`、`i16`、`i32`、`i64`、`f32`、`f64`、`bool`、常用的 C 类型名（`uint32_t`、`int`、`float` 等）和 ELF 中的类型名，后面可以带 `*`。变量名和 ELF 中的类型取自 `--elf` 指定的 ELF；不写 `--elf` 时只能写原始地址。

`read` 和 `watch` 把结构体和数组展开为各个成员；`write` 的每一项写给一个数值变量。值的写法：整数写十进制、`0x` 十六进制或 `0b` 二进制，浮点数写小数，枚举可以写名字，`bool` 写 `true`、`false`、`1` 或 `0`。写入后 `xrdap` 读回并显示读回的值。

## 采样

### 分频和相位

探针每个周期一拍。`watch` 的变量写成 `变量@分频` 时每隔几拍采一次，写成 `变量@分频:相位` 时同时指定相位：变量在 `(节拍号 + 相位) % 分频 == 0` 的拍上采样。分频为 1 到 65535，相位小于分频；没有写相位的分频变量由 `xrdap` 错开相位。一次采样的变量数以探针的上限为准，`xrdap info` 显示这个上限（“每次最多 N 个条目”）。

### 输出

不写 `--csv` 时，`watch` 在终端中最多每 0.1 s 打印一行：探针时间（秒，相对第一帧）、节拍号和每个变量最近的值；`--all` 打印每一帧。写 `--csv` 时每一帧写入文件，终端每秒更新一次已采帧数、跳过节拍和缓冲丢帧。结束时 `watch` 打印帧数、用时和下面的计数器。

### CSV

`watch --csv` 和图形界面的“录制 CSV…”写同样的格式。第一行是表头 `host_time,t,sample_tick,<变量…>`，之后每帧一行：

| 列 | 内容 |
|---|---|
| `host_time` | 换算到电脑时钟的 Unix 时间，秒 |
| `t` | 探针时间，相对本次采样的第一帧，秒 |
| `sample_tick` | 探针的节拍号 |
| 每个变量一列 | 这一拍采到的值；这一拍没有到期时为空，读取失败为 `ERR` |

`host_time` 由 SUB_READ 前后的电脑时间和响应中的探针时间对齐，误差为半个往返时间，`watch` 结束时打印这个误差。

### 计数器

采样能否按周期完成，由运行时的计数器判断。`watch` 结束时打印这些计数器，图形界面的统计行在采样期间显示它们：

| 计数器 | 命令行 | 图形界面 | 含义 |
|---|---|---|---|
| 跳过节拍 | 探针跳过节拍 | 跳拍 | 探针来不及采样而跳过的节拍 |
| 缓冲丢帧 | 缓冲丢帧 | 探针丢帧 | 缓冲满时被覆盖或丢弃的帧 |
| 缓冲占用 | | 缓冲 x/y 字节 | 探针缓冲中还没有读走的字节 |
| 帧序号缺口 | 帧序号缺口 | 缺帧 | 电脑按节拍号发现的缺口：有变量到期却没有收到帧的节拍，跳拍和丢帧都会产生 |
| 读取失败的值 | 读取失败的值 | 失败值 | 探针读目标失败的值，CSV 中为 `ERR` |

跳过节拍增加时，SWD 来不及读完一拍的变量，`xrdap` 建议加长周期、给慢变量加分频或减少变量。缓冲丢帧增加或缓冲占用超过四分之三时，电脑读取太慢或链路带宽不足，`xrdap` 建议给变量加分频或减少变量。命令行在采样中和结束时给出这条提示，图形界面把它显示在统计行下方。采样期间写变量和读内核状态与采样轮流使用 SWD，也会使跳拍增加。

## Wi-Fi

Wi-Fi 是 XRDAP-ESP 的功能：探针作为 Wi-Fi 客户端连接已有的热点。`xrdap wifi` 经探针当前的连接查询和修改热点参数。

```
xrdap wifi status -p COM3
xrdap wifi set -p COM3 热点名 --password-stdin < password.txt
```

`status` 显示保存的热点名、是否连上、IP 地址和信号强度（dBm），以及最近一次清除热点参数的来源（板上按键、串口命令行 `wifi clear`、0x86 清除命令或 NVS 修复）、本次启动的复位原因（上电、软件重启、看门狗、程序崩溃、欠压或其他）和 NVS 是否在本次启动时损坏并重建。

`set` 保存热点名（1 到 32 字节）和密码（8 到 63 字节），例如 `xrdap wifi set -p COM3 热点名 密码`；开放网络省略密码。`--password-stdin` 从标准输入读一行作为密码，去掉行尾的换行，其余原样保留，密码因此不出现在 shell 历史和进程列表中；不重定向时，命令回车后输入密码，再按回车结束。PowerShell 不支持 `<` 重定向，在其中写作 `Get-Content password.txt | xrdap wifi set -p COM3 热点名 --password-stdin`。`--password-stdin` 和命令中的密码参数只能用一个；经 TCP（Wi-Fi）连接探针时，密码以明文经过局域网。`xrdap wifi clear -p COM3` 清除热点参数，请求带确认字节。`set` 和 `clear` 成功后，探针在回复后约 0.5 s 重启，当前连接随之断开。

配置完成后 `xrdap list` 列出探针的局域网地址 `xrdap-xxxxxx.local`，`-p xrdap-xxxxxx.local` 经 Wi-Fi 连接。

图形界面中连上探针后，连接区最下方的“Wi-Fi”显示当前状态；填写“热点”和“密码”后点“保存并重启”保存；点“清除”清除热点参数，点“刷新”重新读取状态。点“保存并重启”后界面清空“密码”一栏。

## 给其他调试工具使用

### USB/IP

`xrdap share usbip` 在 127.0.0.1:3240 提供 USB/IP 设备，并打印挂载命令。客户端挂载后，系统中出现产品名为 `<探针产品名> CMSIS-DAP` 的 USB CMSIS-DAP v2 探针，Keil、pyOCD、probe-rs、OpenOCD 按 USB 探针使用，其中 probe-rs 0.31 在 Windows 上看不到 usbip-win2 挂载的设备。

| 系统 | 安装 | 挂载 | 卸载 |
|---|---|---|---|
| Linux | 内核自带，先执行 `sudo modprobe vhci-hcd`；Ubuntu 的 `usbip` 命令在 `linux-tools-generic` 包中 | `sudo usbip attach -r <地址> -b 1-1` | `sudo usbip detach -p 0` |
| Windows | [usbip-win2](https://github.com/vadimgrn/usbip-win2) | `usbip attach -r <地址> -b 1-1` | `usbip detach -p 0` |

例如在 Windows 上共享串口上的探针，在另一个终端挂载并用 OpenOCD 连接：

```
xrdap share usbip -p COM3
```

```
usbip attach -r 127.0.0.1 -b 1-1
openocd -f interface/cmsis-dap.cfg -f target/stm32f4x.cfg
```

同一时间服务一个客户端。调试工具退出后设备仍处于挂载状态，执行 `usbip detach` 后才拔出；停止共享时（Ctrl-C，或在图形界面取消“共享为 USB DAP”），设备被拔掉。客户端断开时，如果探针还在采样，`xrdap` 停止采样。探针读写出错（超时、断开）时，`xrdap` 断开这个客户端并显示原因，然后等待下一个客户端。

给局域网中另一台电脑使用时加 `--listen 0.0.0.0:3240`，那台电脑执行 `usbip attach -r <本机 IP> -b 1-1`。端口不是 3240 时，`xrdap` 打印的挂载命令带 `--tcp-port`。

图形界面的“共享为 USB DAP”在 127.0.0.1:3240 共享当前连接的探针。共享期间探针由客户端使用，取消勾选后探针交回界面，继续读写和采样。

### OpenOCD 的 TCP 后端

`xrdap share openocd` 在 127.0.0.1:3241（或 `--listen` 指定的地址）提供 OpenOCD 的 CMSIS-DAP TCP 后端，并打印 OpenOCD 的配置。这个命令通常用于串口上的探针；Wi-Fi 探针自己在 3241 端口提供同样的后端，OpenOCD 直接连接探针的地址（`cmsis-dap tcp host <探针 IP>`）。给局域网中其他电脑使用时加 `--listen 0.0.0.0:3241`。`xrdap` 打印的 127.0.0.1 是本机共享的地址：

```
xrdap share openocd -p COM3
```

OpenOCD 在另一个终端中启动：

```
openocd -c "adapter driver cmsis-dap; cmsis-dap backend tcp; cmsis-dap tcp host 127.0.0.1; cmsis-dap tcp port 3241; transport select swd" -f target/stm32f4x.cfg
```

### OpenOCD 版本

经 USB 或 USB/IP 使用探针时，OpenOCD 使用 USB 后端，正式版 0.12.0 可以使用。`cmsis-dap backend tcp`（`xrdap share openocd` 和直接连接 Wi-Fi 探针）不在 0.12.0 中，需要从 master 编译的 OpenOCD。Windows 上要包含提交 e89e73839d53：不含这个提交的版本在 Windows 上把“暂时没有数据”当作错误，启动时等待十几秒，并反复输出 `error reading header`，读写也可能失败。没有这样的版本时，用 `xrdap share usbip` 共享探针，挂载后经 USB 后端连接。
