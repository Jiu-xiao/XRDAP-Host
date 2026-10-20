# XRDAP-Host

[English](README.en.md)

[XRDAP](https://github.com/Jiu-xiao/XRDAP-ESP) 调试探针（ESP32-S3、ESP32-C3、传统 ESP32）的电脑端工具 `xrdap`，Windows 和 Linux。一个可执行文件：双击或不带参数运行是图形界面，带命令是命令行。

它能做的事：

- 按变量名读写目标 MCU 的内存，让探针按固定周期采样变量，实时画曲线、导出 CSV。变量的地址和类型取自 ELF 里的调试信息。
- 把探针共享给 Keil、pyOCD、probe-rs、OpenOCD：C3 插 USB 线也能当标准 USB 探针用。
- 停住、继续、复位目标内核；配置探针的 Wi-Fi。

要在自己的程序里调用，用 Rust 库 [`xrdap`](xrdap/README.md)。

## 下载

在 [Releases](../../releases) 下载对应平台的文件，改名为 `xrdap`（Windows 上 `xrdap.exe`），放进 PATH：

| 平台 | 文件 |
|---|---|
| Windows x86-64 | `xrdap-windows-x86_64.exe` |
| Linux x86-64 | `xrdap-linux-x86_64` |

校验和在同一页的 `SHA256SUMS`。Linux 上图形界面要系统里有中文字体（Noto Sans CJK 或文泉驿），否则中文显示为方框；普通用户访问 USB 探针要 udev 规则：

```
SUBSYSTEM=="usb", ATTR{idVendor}=="1d50", ATTR{idProduct}=="6199", MODE="0666"
```

## 按场景使用

先插上探针运行一次 `xrdap list`，确认能找到。探针的连接写在 `-p` 后面（见“选探针”）；只有一块时可以不写。

### 看变量曲线，边跑边改参数

1. 运行 `xrdap`（不带参数），左上“找到的探针”里点要用的那一行，再点“连接”。界面显示内核型号和运行状态。
2. “ELF”一栏选目标程序的 ELF（编译时带 `-g`）。
3. 在变量列表里搜索变量名，点 `+` 加入监视表；也可以在表达式框写 `motor[1].pid->out` 这类写法后点“添加”。结构体和数组展开成成员。
4. 周期框填 `1ms`，点“开始采样”，曲线开始滚动。
5. 监视表的写入框填值、按回车修改变量；点“录制 CSV…”开始记录。

![xrdap 图形界面](docs/images/xrdap-gui.png)

采样由探针定时，周期不受 USB 或 Wi-Fi 往返延迟限制；周期短于探针估计的单帧耗时时探针拒绝，界面自动改成建议值。曲线可以分到 1 到 4 张图（时间轴联动），悬停显示读数，单击和 Shift+单击放 A、B 两个游标。连接方式、ELF、监视表等在退出时保存。

### 命令行读写变量、导出 CSV

```
xrdap --elf app.elf read motor.speed motor.state
xrdap --elf app.elf write motor.kp=1.5 mode=RUN
xrdap --elf app.elf watch motor.speed motor.current@10 --period 1ms --csv log.csv --duration 10
```

`read` 读一次；`write` 写入后读回；`watch` 让探针按周期采样，`变量@10` 表示每 10 拍采一次，不写 `--duration` 时采到 Ctrl-C。`--csv` 的每一行是一帧：主机时间、探针时间、节拍号，然后每个变量一列（不到期的格为空，读失败为 `ERR`）。

变量的写法：`counter`、`state.speed`、`motor[1].pid->out`、`*ptr`、`p->q->x`、`(uint32_t***)p`，或原始地址 `0x20000010:u32`。

### 用 Keil、pyOCD、probe-rs、OpenOCD 调试

| 探针怎么连 | 做法 |
|---|---|
| S3 插 USB | 它本身就是 USB CMSIS-DAP 探针，直接用 |
| C3、传统 ESP32（或任何一块 XR-DAP）插 USB / 串口 | `xrdap share usbip` 后用 USB/IP 挂载，见下 |
| 走 Wi-Fi | 探针自己提供 USB/IP，直接挂载，见 [XRDAP-ESP](https://github.com/Jiu-xiao/XRDAP-ESP#23-走-wi-fi用-keilpyocdprobe-rsopenocd) |

把插在电脑上的 C3 共享成 USB 探针：

```
xrdap -p COM7 share usbip
```

另开一个终端挂载（Linux 上命令前加 `sudo`，并先 `sudo modprobe vhci-hcd`；Windows 先装 [usbip-win2](https://github.com/vadimgrn/usbip-win2)）：

```
usbip attach -r 127.0.0.1 -b 1-1
```

系统里出现 `XR-DAP-C3 CMSIS-DAP`，各工具按 USB 探针使用，例如：

```
openocd -f interface/cmsis-dap.cfg -f target/stm32f4x.cfg
```

在 `xrdap` 的终端里按 Ctrl-C 停止共享，对调试工具来说探针被拔掉。图形界面里连上探针后勾选“共享为 USB DAP”效果相同，取消勾选后探针交回界面。给局域网里另一台电脑用时加 `--listen 0.0.0.0:3240`，那台电脑 `usbip attach -r <本机 IP> -b 1-1`。

### 让 OpenOCD 用串口上的探针（不装 USB/IP）

```
xrdap -p COM7 share openocd
```

`xrdap` 在 127.0.0.1:3241 提供 OpenOCD 的 CMSIS-DAP TCP 后端，并打印 OpenOCD 要用的配置：

```
openocd -c "adapter driver cmsis-dap; cmsis-dap backend tcp; cmsis-dap tcp host 127.0.0.1; cmsis-dap tcp port 3241; transport select swd" -f target/stm32f4x.cfg
```

TCP 后端只在 OpenOCD 的 master 里，见“OpenOCD 版本”。

### 配探针的 Wi-Fi

```
xrdap wifi set 热点名 密码
xrdap wifi status
xrdap wifi clear
```

`set` 和 `clear` 之后探针约 0.5 秒重启。配好后 `xrdap list` 会列出探针的局域网地址（`xrdap-xxxxxx.local`），以后可以 `-p xrdap-xxxxxx.local` 无线使用。

### 停住、继续、复位目标

```
xrdap halt
xrdap run
xrdap reset
xrdap reset --halt
```

`reset --halt` 复位后停在第一条指令之前。图形界面的“停住/继续”“复位”“复位并停住”按钮相同，采样期间也能用。

### 没有硬件时试用

```
xrdap --fake info
xrdap --fake --elf app.elf read motor.speed
```

`--fake` 用内置的模拟探针，按 ELF 填内存。图形界面的连接方式选“模拟”。

## 参考

### 选探针（`-p`）

| 写法 | 含义 |
|---|---|
| 不写 | 只有一块 USB 探针时用它；否则找遍 USB、串口、局域网，只有一块时用它 |
| `usb`、`usb:<序列号>` | USB |
| `COM7`、`/dev/ttyACM0` | 串口。乐鑫芯片自带的 USB 口自动保持 DTR/RTS 不变；波特率按串口芯片自动选（CH343 等先试 4000000，CP2102、CH340 用 921600），`--baud` 可以指定 |
| `192.168.0.108`、`xrdap-93ef1c.local`、`host:port` | Wi-Fi（OpenOCD TCP 后端端口 3241） |
| `XRobot-XRDAP-E8069093EF1C`、`93EF1C` | 序列号全称或末尾几位，先找 USB，再找串口、局域网 |

`xrdap list` 列出所有探针，同一块探针的几种连接放在一起，并给出 `-p` 的写法。

### 命令

| 命令 | 作用 |
|---|---|
| （不带命令） | 图形界面 |
| `list` | 列出 USB、串口、局域网上的探针 |
| `info [--no-target]` | 探针信息（DAP_Info、厂商扩展），连接目标读 DPIDR 和内核型号 |
| `vars [文字]` | 列出 ELF 里的全局变量 |
| `read 变量...` | 读变量 |
| `write 变量=值...` | 写变量并读回 |
| `watch 变量[@分频[:相位]]... [--period] [--csv] [--duration] [--no-overwrite] [--all]` | 探针端周期采样 |
| `halt`、`run`、`reset [--halt]` | 内核控制 |
| `wifi status`、`wifi set 热点 [密码]`、`wifi clear` | 探针 Wi-Fi |
| `share usbip [--listen]`、`share openocd [--listen]` | 把探针共享给别的调试工具 |

通用选项：`-p` 探针、`--elf` ELF、`--clock` SWD 时钟（默认 4M）、`--baud` 串口波特率（默认按串口芯片自动选）、`--keep-lines` 串口不动 DTR/RTS、`--timeout-ms` 响应超时、`--fake` 模拟探针。`xrdap 命令 -h` 看每个命令的完整参数。

### USB/IP 客户端

| 系统 | 安装 | 挂载 | 卸载 |
|---|---|---|---|
| Linux | 内核自带：`sudo modprobe vhci-hcd`；Ubuntu 的 `usbip` 命令在 `linux-tools-generic` 包里 | `sudo usbip attach -r <地址> -b 1-1` | `sudo usbip detach -p 0` |
| Windows | [usbip-win2](https://github.com/vadimgrn/usbip-win2) | `usbip attach -r <地址> -b 1-1` | `usbip detach -p 0` |

同一时间只服务一个客户端。挂载的调试工具退出后设备仍挂着，`usbip detach` 才算拔掉；共享停止时设备被拔掉。客户端离开时如果探针还在采样，`xrdap` 替它停掉。

### OpenOCD 版本

- 经 USB 或 USB/IP 使用探针时走 OpenOCD 的 USB 后端，正式版 0.12.0 就行。
- `cmsis-dap backend tcp`（`share openocd` 和 Wi-Fi 直连）不在任何正式版里（最新正式版 0.12.0 发布于 2023 年），要从 master 自己编译。Windows 上要包含 2026-08-15 的提交 e89e73839d53，更早的 master 在 Windows 上把“暂时没有数据”当错误，启动等十几秒并刷屏 `error reading header`。

### 从源码编译

```
cargo build --release -p xrdap-tool
```

生成 `target/release/xrdap`。Linux 上先装 `libgtk-3-dev libxkbcommon-dev libudev-dev`。

| 目录 | 内容 |
|---|---|
| `xrdap` | Rust 库，见 [xrdap/README.md](xrdap/README.md) |
| `xrdap-tool/src/cli` | 命令行：连接选项、各命令 |
| `xrdap-tool/src/gui` | 图形界面：后台线程、各面板、曲线 |

```
cargo test --workspace --all-features
```

测试用进程内的模拟探针，不需要硬件，覆盖帧编解码、厂商命令、采样、DWARF 解析、各个命令和共享。CI 还检查 `cargo fmt` 和 `cargo clippy`。

## 许可证

Apache-2.0，见 [LICENSE](LICENSE)。
