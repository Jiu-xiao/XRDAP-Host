[English](README.en.md)

# XRDAP-Host

[XRDAP](https://github.com/Jiu-xiao/XRDAP-ESP) 调试探针（ESP32-S3、ESP32-C3）的电脑端工具，用 Rust 编写，支持 Windows 和 Linux。它通过 USB、串口或 TCP 连接探针，按变量名读写目标 MCU 的内存，让探针按固定周期采样变量并记录为 CSV，也可以停住、继续、复位目标内核，以及配置探针的 Wi-Fi。变量的地址和类型取自目标程序 ELF 里的 DWARF 调试信息。

| 使用方式 | 名称 | 作用 |
|---|---|---|
| Rust 库 | `xrdap` | 协议、三种连接方式（USB、串口、TCP）、探针发现、DWARF 解析，可直接在自己的程序里调用 |
| 命令行 | `xrdap` | 列出探针，读写变量，周期采样并导出 CSV，控制内核，配置 Wi-Fi |
| 图形界面 | `xrdap-gui` | 搜索 ELF 里的变量加入监视表，实时画曲线，采样时修改变量，录制 CSV |
| 串口转 TCP | `xrdap-forward` | 让 OpenOCD 通过 `cmsis-dap backend tcp` 使用串口连接的探针（S3 的板上串口、C3 的 USB 口） |

## 下载和编译

[Releases](../../releases) 的每个版本提供以下文件，另附校验文件 `SHA256SUMS`：

| 文件 | 内容 |
|---|---|
| `xrdap-windows-x86_64.exe`、`xrdap-linux-x86_64` | 命令行 |
| `xrdap-gui-windows-x86_64.exe`、`xrdap-gui-linux-x86_64` | 图形界面 |
| `xrdap-forward-windows-x86_64.exe`、`xrdap-forward-linux-x86_64` | 串口转 TCP |

自行编译需要 Rust 工具链：

```
git clone https://github.com/Jiu-xiao/XRDAP-Host.git
cd XRDAP-Host
cargo build --release
```

程序生成在 `target/release/` 下。

Linux 上编译需要先安装开发包（Ubuntu）：

```
sudo apt install libudev-dev libgtk-3-dev libxkbcommon-dev
```

Linux 上通过 USB 使用探针需要 udev 规则放开设备权限，探针的 VID:PID 是 `1D50:6199`。创建 `/etc/udev/rules.d/99-xrdap.rules`，内容为一行：

```
SUBSYSTEM=="usb", ATTRS{idVendor}=="1d50", ATTRS{idProduct}=="6199", MODE="0666"
```

然后重新加载规则并重新插拔探针：

```
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## 选择探针

命令行用 `--probe`（简写 `-p`）指定探针，库的 `open()` 接受同样的字符串。

| 写法 | 含义 |
|---|---|
| 不写 | 自动选择唯一的探针 |
| `usb`、`usb:<序列号>` | USB（CMSIS-DAP v2） |
| `COM35`、`/dev/ttyUSB0`、`serial:<串口>` | 串口 |
| `192.168.0.108`、`192.168.0.108:3241`、`xrdap-93ef1c.local`、`tcp:<主机>` | TCP，端口默认 3241 |
| `XRobot-XRDAP-E8069093EF1C`、`93EF1C` | 按序列号（全称或末尾几位，不分大小写）查找；同一块探针有多条连接时，按 USB、串口、TCP 的顺序取第一条 |

`--usb[=<序列号>]`、`--serial <串口>`、`--tcp <地址[:端口]>` 分别等同于对应的 `--probe` 写法。`--probe`、`--usb`、`--serial`、`--tcp` 只能写一个。

不写探针时按以下顺序自动选择：

1. 只有一块 USB 探针时直接使用，不扫描串口和局域网。
2. 否则同时扫描串口和局域网（mDNS 服务 `_xrdap._tcp`），约 1.5 秒。串口只尝试常见 USB 转串口芯片（WCH CH34x、Silicon Labs CP210x、FTDI）和乐鑫芯片自带的 USB Serial/JTAG，向它们发送 `DAP_Info`，应答的才算探针。
3. 同一序列号的 USB、串口、TCP 连接合并为一块探针。结果恰好是一块时使用它；一块也没有时报错；多块时报错，并附上 `xrdap list` 格式的列表。

`xrdap list` 列出所有探针、每块探针可用的连接，以及对应的 `--probe` 写法。两块探针的输出：

```
> xrdap list
XRobot-XRDAP-9C139E721384  XR-DAP-C3
    串口 COM116    --probe COM116
    TCP 192.168.0.112:3241（xrdap-721384.local）    --probe 192.168.0.112:3241
XRobot-XRDAP-E8069093EF1C  XR-DAP-S3
    USB XRobot-XRDAP-E8069093EF1C    --probe usb:XRobot-XRDAP-E8069093EF1C
    串口 COM35    --probe COM35
    TCP 192.168.0.108:3241（xrdap-93ef1c.local）    --probe 192.168.0.108:3241
```

此时 `xrdap info --probe 93EF1C` 选中第二块探针的 USB 连接，`xrdap info --probe COM116` 选中第一块探针的串口连接。

打开串口时，S3 板上的 USB 转串口芯片会让探针复位一次，xrdap 最长等待 10 秒让探针启动。乐鑫芯片自带的 USB Serial/JTAG 口按 DTR/RTS 复位芯片，xrdap 自动保持这两根线不变；其他串口需要同样处理时加 `--keep-lines`。

## 命令行 xrdap

变量表达式取自 ELF 里的 DWARF 信息（编译时加 `-g`），用 `--elf <文件>` 指定 ELF。可以写结构体成员、数组下标、指针和类型转换，例如 `state.speed`、`motor[1].pid->out`、`head->next->value`、`(uint32_t***)p`。没有 ELF 时直接写地址和类型，例如 `0x20000010:u32`，类型可以是 `u8`、`u16`、`u32`、`u64`、`i8`、`i16`、`i32`、`i64`、`f32`、`f64`。

连接选项适用于所有命令：

| 选项 | 作用 |
|---|---|
| `-p, --probe <探针>` | 选择探针，见上一节 |
| `--elf <文件>` | 目标程序的 ELF |
| `--clock <Hz>` | SWD 时钟，例如 `4000000`、`4M`、`500k`，默认 4M |
| `--baud <波特率>` | 串口波特率，要和探针固件一致，默认 4000000 |
| `--keep-lines` | 打开串口后不改 DTR/RTS |
| `--timeout-ms <毫秒>` | 等待一个响应的超时，默认 2000 |
| `--fake` | 使用内置的模拟探针，不需要硬件；配合 `--elf` 时按 ELF 填模拟内存 |

下面的例子以测试程序 [g431-test.c](xrdap/tests/fixtures/g431-test.c) 及其 ELF 为目标。在仓库根目录执行 `cd xrdap/tests/fixtures` 后，`g431-test.elf` 可以直接引用；读写、采样、内核控制的命令需要探针连着运行这个程序的目标板，或者加 `--fake` 使用模拟探针。

### list：列出探针

```
xrdap list
```

输出见上一节。

### info：探针和目标信息

```
> xrdap info --probe 93EF1C --no-target
连接：USB XRobot-XRDAP-E8069093EF1C
探针：XRobot XR-DAP-S3，序列号 XRobot-XRDAP-E8069093EF1C，固件 0.2.0
CMSIS-DAP：2.0.0，包长 1024 字节，最多 4 个包在途
厂商扩展：版本 1，功能 READ、WRITE、采样、8 字节访问（0x1F）
  每次最多 32 个条目，最多 8 级指针，采样缓冲 16384 字节
  探针时间 929.113669 s，单次 SWD 访问耗时 26 µs（上次 SUB_SET 时标定）
```

不加 `--no-target` 时，xrdap 还会连接目标，追加 `目标：` 一行（DPIDR 和 SWD 时钟）和 `内核：` 一行（内核型号、运行状态、DHCSR）。

### vars：列出 ELF 里的全局变量

```
> xrdap --elf g431-test.elf vars cnt
0x20000008  cnt_a                    uint32_t，4 字节
0x2000000C  cnt_b                    uint32_t，4 字节
0x20000010  cnt64                    uint64_t，8 字节
```

文字参数过滤名字，不写时列出全部。`vars` 不连接探针。

### read：读变量

`chain_p1` 是三级指针链，`null_p1` 是空指针：

```
> xrdap --elf g431-test.elf read v_u16 "***(uint32_t***)chain_p1" "*null_p1"
v_u16                    = 4660 (0x1234)
***(uint32_t***)chain_p1 = 3405691582 (0xCAFEBABE)
*null_p1                   读取失败：第 1 级指针 null_p1 为空（NULL）（地址 [0x20000038]）
```

数值按 DWARF 类型显示：有符号数、浮点、枚举名和指针分别按各自的格式输出。结构体和数组展开成各个成员读取。探针按 1、2、4、8 字节访问内存，访问位域成员时 xrdap 报错，并给出它所在字节的地址写法（如 `0x20000043:u8`）。

### write：写变量

```
> xrdap --elf g431-test.elf write v_u8=0x5A
v_u8 ← 0x5A，读回 90 (0x5A)
```

整数可以写十进制或 `0x` 十六进制，浮点写小数，枚举写名字。一次可以写多个 `变量=值`，写完读回一次；有变量写入失败时命令以错误退出。

### watch：周期采样

```
> xrdap --elf g431-test.elf watch cnt_a cnt_b@10 v_u32@100 --period 1ms --csv log.csv
采样 3 个变量，周期 1000 µs，估计每帧耗时 163 µs，一帧最多 23 字节
  cnt_b 每 10 拍采一次，相位 1
  v_u32 每 100 拍采一次，相位 0
写入 log.csv，按 Ctrl-C 停止
```

| 选项 | 作用 |
|---|---|
| `--period <周期>` | 采样周期，写 `1000`（µs）、`500us`、`2ms`，默认 1ms |
| `--csv <文件>` | 把每一帧写入 CSV 文件 |
| `--duration <秒>` | 采样指定秒数后停止，不写时采到 Ctrl-C |
| `--all` | 终端里打印每一帧，默认最多每 0.1 秒打印一行 |
| `--no-overwrite` | 探针的采样缓冲满后丢弃新帧，默认覆盖最旧的帧 |

`变量@分频` 表示每几拍采一次，`变量@分频:相位` 指定相位。不指定相位时，各个分频变量的相位由 xrdap 错开，尽量不落在同一拍。一次最多采样 32 个变量。

CSV 每帧一行。`host_time` 列是按 SUB_READ 往返时间换算到电脑时钟的 Unix 时间，`t` 列是相对第一帧的探针时间，`sample_tick` 是探针的节拍序号，该拍没有采的变量为空，读取失败为 `ERR`：

```
host_time,t,sample_tick,cnt_a,cnt_b,v_u32
1792345704.807567,0.000000,0,0,,2309737967
1792345704.808567,0.001000,1,1,,
1792345704.809567,0.002000,2,2,,
```

不写 `--csv` 时，终端按列打印时间、节拍和各变量的最新值，例如采样 5 秒：

```
xrdap --elf g431-test.elf watch cnt_a cnt_b@10 --period 1ms --duration 5
```

按 Ctrl-C 或 `--duration` 到时后，xrdap 停止采样、读完缓冲，打印帧数、探针报告的跳过节拍数、缓冲丢帧数、帧序号缺口和读取失败的值的个数。周期短于探针估计的单帧耗时时，探针拒绝配置，xrdap 给出估计值和可用的周期。

采样期间探针只接受 `DAP_Info`、`DAP_HostStatus` 和厂商命令，其他命令回 `0xFF`，所以采样不能和 OpenOCD 调试同时进行。

### halt、run、reset：内核控制

```
xrdap halt
xrdap run
xrdap reset
xrdap reset --halt
```

`halt` 停住内核，`run` 让停住的内核继续运行，`reset` 做系统复位（AIRCR.SYSRESETREQ）后运行，`reset --halt` 复位后停在复位向量。每条命令输出内核的状态，例如（模拟探针）：

```
> xrdap --fake halt
内核：已停住（DHCSR 0x00030003）
```

这几条命令通过厂商命令访问 DHCSR、AIRCR，采样期间也能使用。`xrdap info` 显示目标内核的型号和运行状态。

### wifi：配置探针的 Wi-Fi

经 DAP 本身配置，不需要另开串口终端。固件版本需要 0.2.0 及以上。

```
> xrdap wifi status
热点 lab，已连接，IP 192.168.0.108，信号 -25 dBm
```

```
xrdap wifi set lab 12345678
xrdap wifi set open-hotspot
xrdap wifi clear
```

`wifi set <热点> [密码]` 保存热点参数，热点名最长 32 字节，密码 8 到 63 字节，开放网络不写密码。保存后探针约 0.5 秒重启并连接该热点，当前连接随之断开。`wifi clear` 清除热点参数，探针重启后不再连 Wi-Fi。`wifi status` 不显示密码。

## 图形界面 xrdap-gui

```
xrdap-gui
```

![xrdap-gui：两张图、归一化、悬停读数](docs/images/xrdap-gui.png)

界面左侧是连接和变量设置，中间上方是控制栏和监视表，其余是曲线区。

- 找到的探针：启动时自动查找 USB、串口、局域网上的探针，每个连接一行，点击后填入连接参数；“重新查找”再查一次。
- 连接：选 USB、串口、TCP 或模拟（不需要硬件的模拟探针），填写参数和 SWD 时钟后点“连接”。USB 和串口有“刷新”按钮重新列出设备。
- 内核状态：连接后显示内核型号和运行状态（运行中、睡眠、已停住、锁死），每 0.5 秒刷新，带“停住/继续”“复位”“复位并停住”按钮，采样期间也能使用。
- ELF：选择带 `-g` 调试信息的 ELF。
- 添加变量：在全局变量列表里搜索，点 `+` 加入监视表，或者在表达式框里写 `motor[1].pid->out` 这类表达式后点“添加”。结构体和数组展开成各个成员。勾选“只看自己的变量”会按源文件路径隐藏 HAL、CMSIS、RTOS、中间件和 C 库里的变量。
- 控制栏：采样周期、开始/停止、读一次、录制 CSV、曲线窗口长度、归一化、暂停跟随、清空曲线。采样时显示帧率、探针丢帧、跳拍、缺帧和读取失败的个数，任何一项不为 0 时变成警告色；目标停住时提示变量不会变化。周期短于探针估计的单帧耗时时探针拒绝，界面把周期改成建议值。
- 监视表：每个变量一行，可以设分频、选是否画曲线、画在第几张图、查看当前值（变化时短暂高亮）。在写入框里填值后按回车或点“写”，写完读回一次，采样期间也能写。新加的指针默认不画曲线。
- 曲线：变量可以分到 1 到 4 张图，几张图的时间轴联动。归一化把每条曲线按窗口里的最小、最大值缩放到同一高度。悬停显示所有曲线在该时刻的值；单击放游标 A，Shift+单击放游标 B，显示 Δt 和两处的值。每条曲线按屏幕宽度做最大最小值抽稀，尖峰不会被抽掉，保留最近 120 秒。暂停跟随后可以拖动和缩放，数据照常接收。
- CSV：格式与 `xrdap watch --csv` 相同。“录制 CSV…”在采样期间可用，从选好文件的时刻开始记录。

首次使用的步骤：

1. 运行 `xrdap-gui`，在“找到的探针”里点要用的连接。
2. 点“连接”，界面显示内核型号和运行状态。
3. 在“ELF”一栏选择目标程序的 ELF。
4. 在搜索框输入变量名，点 `+` 加入监视表。
5. 在周期框填采样周期（如 `1ms`），点“开始采样”，曲线开始滚动。
6. 在监视表的写入框填值并按回车修改变量；点“录制 CSV…”选择文件开始记录。

连接方式、ELF 路径、监视表、周期、窗口长度和归一化在退出时保存，下次打开时恢复。界面需要系统里有中文字体（Windows 的微软雅黑或黑体，Linux 的 Noto Sans CJK 或文泉驿），否则中文显示为方框。

## Rust 库

在 `Cargo.toml` 中加入 git 依赖：

```toml
[dependencies]
xrdap = { git = "https://github.com/Jiu-xiao/XRDAP-Host" }
```

库的 feature 默认全部开启：

| feature | 内容 |
|---|---|
| `usb` | CMSIS-DAP v2 USB Bulk 传输 |
| `elf` | 读取 ELF 符号和 DWARF 类型 |
| `discover` | 探针发现：串口逐个询问，局域网用 mDNS（依赖 `usb`） |

快速上手：打开探针，连接 SWD 目标，按名字读变量。

```rust
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> xrdap::Result<()> {
    // 自动选唯一的探针；也可以写 Some("COM35")、Some("192.168.0.108")、Some("93EF1C")
    let mut probe = open(None, &OpenOptions::default())?;
    probe.connect_swd(4_000_000)?;

    let mut syms = xrdap::elf::load(std::path::Path::new("app.elf"))?;
    let speed = Resolver::new(&mut syms).resolve("motor.speed")?;
    println!("{} = {}", speed.name, speed.read_text(&mut probe)?);
    Ok(())
}
```

`Target` 提供 `read`（读出字节）、`read_text`（按类型格式化）和 `write`（写入字节，可用 `target.ty.parse("1.5")` 从文字得到）。模块概览：

| 模块 | 内容 |
|---|---|
| `open` | 按一个字符串打开探针，或自动选择 |
| `discover` | 查找 USB、串口、局域网上的探针 |
| `dap` | CMSIS-DAP 命令、SWD/ADIv5 初始化、厂商命令 |
| `expr`、`types`、`elf` | 变量表达式、类型、DWARF |
| `watch`、`csv` | 探针端采样、CSV 记录 |
| `cortex` | 内核状态和停住、继续、复位 |
| `wifi` | 探针 Wi-Fi 的查询、设置、清除 |
| `vendor`、`frame`、`transport` | 厂商命令编码、串口和 TCP 帧、三种传输 |
| `fake` | 进程内的模拟探针，测试用 |

厂商命令的字节格式见 [XRDAP-ESP 的 docs/protocol.md](https://github.com/Jiu-xiao/XRDAP-ESP/blob/main/docs/protocol.md)。

`xrdap/examples/` 下有四个示例，在仓库根目录运行。读写、采样和 Wi-Fi 示例用环境变量 `XRDAP_PROBE` 指定探针（写法同 `--probe`），不设时自动选唯一的探针。

| 示例 | 作用 | 运行命令 |
|---|---|---|
| `list` | 列出 USB、串口、局域网上的探针 | `cargo run -p xrdap --example list` |
| `read_write` | 按变量名读；给出第三个参数时先写再读 | `cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5` |
| `sample` | 让探针以 1 ms 周期采样若干变量，取回 1 秒的数据，每 100 帧打印一行并输出统计 | `cargo run -p xrdap --example sample -- app.elf motor.speed motor.current` |
| `wifi` | 不带参数时查询探针的 Wi-Fi 状态，带热点名和密码时保存 | `cargo run -p xrdap --example wifi` |

指定探针的写法，例如：

```
XRDAP_PROBE=COM35 cargo run -p xrdap --example read_write -- app.elf motor.speed
```

## xrdap-forward：串口转 TCP

```
xrdap-forward --serial COM35
```

| 参数 | 默认值 | 作用 |
|---|---|---|
| `--serial` | 无，必填 | 探针的串口，例如 `COM35` 或 `/dev/ttyUSB0` |
| `--baud` | 4000000 | 串口波特率，与固件一致 |
| `--listen` | `127.0.0.1:3241` | 给 OpenOCD 连接的 TCP 地址 |
| `--timeout-ms` | 2000 | 一个请求等待响应的超时（毫秒） |
| `--keep-lines` | 关 | 打开串口后不改 DTR/RTS |
| `--verbose` | 关 | 打印每个转发的请求和响应 |

打开串口时，USB 转串口芯片的 DTR/RTS 会让 ESP32 复位一次。xrdap-forward 反复查询探针，等它启动完成（最长 10 秒），打印探针报告的 DAP 包长后开始监听。ESP32-C3 用自带的 USB Serial/JTAG 时没有这个复位问题，加 `--keep-lines`。同一时间只服务一个 OpenOCD。

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
