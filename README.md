# XRDAP-Host

[XRDAP](https://github.com/Jiu-xiao/XRDAP-ESP)（ESP32-S3、ESP32-C3）调试探针的电脑端工具，用 Rust 写，Windows 和 Linux 都能用。

| 程序 | 作用 |
|---|---|
| `xrdap` | 命令行：按变量名读写目标内存，在探针上周期采样变量并导出 CSV |
| `xrdap-gui` | 图形界面：搜索 ELF 里的变量加进监视表，在探针上采样并实时画曲线，采样时可以改变量，可以录制 CSV |
| `xrdap-forward` | 串口转 TCP：让 OpenOCD 通过 `cmsis-dap backend tcp` 使用串口连接的探针（ESP32-C3、H2、MSPM0 等没有 USB 的芯片） |

库 `xrdap` 里是协议、三种连接方式（USB、串口、TCP）和 DWARF 解析，可以直接在自己的程序里用。

## 下载和编译

在 [Releases](../../releases) 下载 Windows 或 Linux 的可执行文件。自己编译需要 Rust 工具链：

```
git clone https://github.com/Jiu-xiao/XRDAP-Host.git
cd XRDAP-Host
cargo build --release
```

程序在 `target/release/` 下。Linux 上用 USB 需要 udev 规则放开权限，VID:PID 是 `1D50:6199`。Linux 上编译 `xrdap-gui` 需要 GTK3 和 xkbcommon 的开发包（Ubuntu：`sudo apt install libgtk-3-dev libxkbcommon-dev`）。

## xrdap：按变量名读写和采样

变量的地址和类型取自目标程序 ELF 里的 DWARF 调试信息（编译时加 `-g`），可以写结构体成员、数组下标、指针和类型转换，例如 `state.speed`、`motor[1].pid->out`、`head->next->value`、`(uint32_t***)p`；没有 ELF 时直接写地址和类型，例如 `0x20000010:u32`。xrdap 先完成 SWD 连接和调试域上电，再发送探针的厂商命令（命令格式见 [XRDAP-ESP 的 docs/protocol.md](https://github.com/Jiu-xiao/XRDAP-ESP/blob/main/docs/protocol.md)）。

连接方式用 `--usb`、`--serial COM35` 或 `--tcp 192.168.1.50` 指定，不写时用 USB；插着多个探针时用 `--usb=<序列号>` 选择，`xrdap list` 列出序列号。SWD 时钟用 `--clock` 设置，默认 4 MHz。

以测试程序 [g431-test.c](xrdap/tests/fixtures/g431-test.c) 为例，读几个变量，其中 `chain_p1` 是三级指针链，`null_p1` 是空指针：

```
> xrdap --elf g431-test.elf read v_u16 "***(uint32_t***)chain_p1" "*null_p1"
v_u16                    = 4660 (0x1234)
***(uint32_t***)chain_p1 = 3405691582 (0xCAFEBABE)
*null_p1                   读取失败：第 1 级指针 null_p1 为空（NULL）（地址 [0x20000038]）

> xrdap --elf g431-test.elf write v_u8=0x5A
v_u8 ← 0x5A，读回 90 (0x5A)
```

数值按 DWARF 类型显示：有符号数、浮点、枚举名和指针分别按各自的格式输出。结构体和数组展开成各个成员读取。探针按 1、2、4、8 字节访问内存，访问位域成员时 xrdap 报错，并给出它所在字节的地址写法（如 `0x20000043:u8`）。

采样用 `watch`。`变量@分频` 表示每几拍采一次，各个分频变量的相位由 xrdap 错开，尽量不落在同一拍：

```
> xrdap --elf g431-test.elf watch cnt_a cnt_b@10 v_u32@100 --period 1ms --csv log.csv
采样 3 个变量，周期 1000 µs，估计每帧耗时 163 µs，一帧最多 23 字节
  cnt_b 每 10 拍采一次，相位 1
  v_u32 每 100 拍采一次，相位 0
写入 log.csv，按 Ctrl-C 停止
```

CSV 每帧一行，`host_time` 列是按 SUB_READ 往返时间换算到电脑时钟的 Unix 时间，`t` 列是相对第一帧的探针时间，`sample_tick` 是探针的节拍序号，该拍没有采的变量为空，读取失败为 `ERR`：

```
host_time,t,sample_tick,cnt_a,cnt_b,v_u32
1792345704.807567,0.000000,0,0,,2309737967
1792345704.808567,0.001000,1,1,,
1792345704.809567,0.002000,2,2,,
```

不写 `--csv` 时在终端里每 0.1 秒打印一行。按 Ctrl-C 或 `--duration` 指定的秒数到了之后，xrdap 停止采样、读完缓冲，打印探针报告的跳过节拍数和缓冲丢帧数。周期短于探针估计的单帧耗时时，SUB_SET 被拒绝，xrdap 给出估计值和可用的周期。全部命令和选项见 `xrdap --help`。

`xrdap info` 会显示目标内核的型号和运行状态。`xrdap halt`、`xrdap run` 停住或继续内核，`xrdap reset` 系统复位后运行，`xrdap reset --halt` 复位后停在复位向量。这几条都通过厂商命令访问 DHCSR、AIRCR，采样期间也能用。

采样期间探针只接受 `DAP_Info`、`DAP_HostStatus` 和厂商命令，其他命令回 `0xFF`，所以采样不能和 OpenOCD 调试同时进行。

## xrdap-gui：图形界面

```
xrdap-gui
```

![xrdap-gui：两张图、归一化、悬停读数](docs/images/xrdap-gui.png)

- 左边：选连接方式（USB、串口、TCP，或不需要硬件的模拟探针）和 SWD 时钟后连接；选目标程序的 ELF；在全局变量列表里搜索，点 `+` 加进监视表，或者在表达式框里写 `motor[1].pid->out` 这类表达式。结构体和数组展开成各个成员。勾选“只看自己的变量”会按源文件路径隐藏 HAL、CMSIS、RTOS、中间件和 C 库里的变量。
- 连接后显示内核型号和运行状态（运行中、睡眠、已停住、锁死），每 0.5 秒刷新，带停住/继续、复位、复位并停住按钮，采样期间也能用。
- 中间上方：采样周期、开始/停止、读一次、录制 CSV、曲线窗口长度、归一化、暂停跟随。采样时显示帧率、探针丢帧、跳拍、缺帧和读取失败的个数，有任何一项不为 0 时变成警告色；目标停住时提示变量不会变化。周期短于探针估计的单帧耗时时探针拒绝，界面把周期改成建议值。
- 监视表：每个变量一行，可以设分频、选是否画曲线、画在第几张图、看当前值（变化时短暂高亮）；在写入框里填值后按回车或点“写”，写完读回一次，采样期间也能写。新加的指针默认不画曲线。
- 曲线：变量可以分到 1 到 4 张图，几张图的时间轴联动；归一化把每条曲线按窗口里的最小、最大值缩放到同一高度。悬停显示所有曲线在该时刻的值；单击放游标 A，Shift+单击放游标 B，显示 Δt 和两处的值。每条曲线按屏幕宽度做最大最小值抽稀，尖峰不会被抽掉，保留最近 120 秒。暂停跟随后可以拖动和缩放，数据照常接收。
- CSV 和 `xrdap watch --csv` 的格式相同，从选好文件的时刻开始记录。

连接方式、ELF 路径、监视表、周期、窗口长度和归一化在退出时保存，下次打开时恢复。界面需要系统里有中文字体（Windows 的微软雅黑或黑体，Linux 的 Noto Sans CJK 或文泉驿），没有时中文显示成方框。

## xrdap-forward：串口转 TCP

```
xrdap-forward --serial COM35
```

| 参数 | 默认值 | 作用 |
|---|---|---|
| `--serial` | 无，必填 | 探针的串口，例如 `COM35` 或 `/dev/ttyUSB0` |
| `--baud` | 4000000 | 串口波特率，与固件一致 |
| `--listen` | `127.0.0.1:3241` | 给 OpenOCD 连接的 TCP 地址 |
| `--keep-lines` | 关 | 打开串口后不改 DTR/RTS |
| `--verbose` | 关 | 打印每个转发的请求和响应 |

打开串口时，USB 转串口芯片的 DTR/RTS 会让 ESP32 复位一次。xrdap-forward 反复查询探针，等它启动完成（最长 10 秒），打印探针报告的 DAP 包长后开始监听。ESP32-C3/H2 用自带的 USB Serial/JTAG 时没有这个复位问题，加 `--keep-lines`。同一时间只服务一个 OpenOCD。

OpenOCD 使用 TCP 后端连接 127.0.0.1：

```tcl
adapter driver cmsis-dap
cmsis-dap backend tcp
cmsis-dap tcp host 127.0.0.1
cmsis-dap tcp port 3241
transport select swd
source [find target/stm32g4x.cfg]
adapter speed 4000
```

Windows 上 Espressif 的 OpenOCD v0.12.0-esp32-20260831 的 TCP 后端有 bug（把"暂时没有数据"当成错误），要多加两行：

```tcl
cmsis-dap quirk enable
cmsis-dap tcp min_timeout 10
```

代价是 OpenOCD 一次只发一个包，吞吐大约降到三分之一。OpenOCD 上游已修复（提交 e89e73839d53），换用带这个提交的构建后可以去掉这两行；Linux 上不需要。

## 测试

```
cargo test --release --workspace
```

测试里有一个在进程内模拟的探针，不需要硬件，覆盖帧编解码、条目编码、采样帧解码、DWARF 解析和内核停住、复位。CI 还会检查 `cargo fmt` 和 `cargo clippy`。

## 许可证

Apache-2.0，见 [LICENSE](LICENSE)。
