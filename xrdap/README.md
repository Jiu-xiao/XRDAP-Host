# xrdap（Rust 库）

[English](README.en.md)

XRDAP 探针的电脑端 Rust 库：打开探针（USB、串口、TCP），按变量名读写目标内存，由探针按固定周期采样变量，控制目标内核，配置探针 Wi-Fi，把探针共享给其他调试工具。命令行和图形界面工具 `xrdap` 建立在这个库之上。探针与主机之间的字节格式见 XRDAP-Core 的[协议文档](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md)。

## 加入依赖

```toml
[dependencies]
xrdap = { git = "https://github.com/Jiu-xiao/XRDAP-Host", branch = "main" }
```

| feature | 默认 | 内容 |
|---|---|---|
| `elf` | 开 | 读 ELF 符号和 DWARF 类型，按变量名访问 |

Linux 上编译需要 `libudev-dev`（USB 和串口枚举）。

## 按场景使用

以下例子在 `examples/` 中都有完整版本，在仓库根目录运行，例如 `cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5`。示例用环境变量 `XRDAP_PROBE` 指定探针（写法同 `xrdap -p`），不设置时自动选择唯一的探针。

### 打开探针，读写变量

```rust
use xrdap::addr::Spec;
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> xrdap::Result<()> {
    // Spec::Auto 在只有一块探针时选中它；也可以写 Spec::parse("COM3")、
    // Spec::parse("192.0.2.10")、Spec::parse("DDEEFF")
    let mut probe = open(&Spec::Auto, &OpenOptions::default())?;
    probe.connect(4_000_000)?;

    let mut syms = xrdap::elf::load(std::path::Path::new("app.elf"))?;
    let mut resolver = Resolver::new(&mut syms);
    let kp = resolver.resolve("motor.kp")?;
    println!("{} = {}", kp.name, kp.read_text(&mut probe)?);
    kp.write(&mut probe, &kp.ty.parse("1.5")?)?;
    Ok(())
}
```

`open` 是打开探针的唯一入口，写法见 `addr` 模块。自动选择和按序列号查找只看 USB、局域网和按 USB 描述符认出的乐鑫 USB 口，不打开其他串口；串口在打开时 DTR、RTS 都保持无效。`Probe::connect` 先查 INFO，厂商扩展的协议版本必须是 2，再连接目标；另一条连接占着探针时返回 `Error::Busy`（“探针正被另一条连接使用”）。主机只停止自己开始的采样。`Resolver::resolve` 接受 `state.speed`、`motor[1].pid->out`、`*ptr`、`(uint32_t***)p`、`0x20000010:u32` 这类写法；`resolve_all` 把结构体和数组展开为成员。读写失败时返回 `Error::Item`，其中带条目状态和变量名。完整示例：`read_write`。

### 由探针周期采样

```rust
use xrdap::watch::{Sampler, Var};

// probe：已连接目标的 Probe；targets：resolve 得到的变量
let vars = targets
    .iter()
    .enumerate()
    .map(|(i, t)| Var { item: t.item(i as u16), div: 1, phase: None })
    .collect();
// 周期 1000 µs；一条 SUB_START 带全部配置并开始采样
let mut sampler = Sampler::start(probe, vars, 1000, false)?;
for _ in 0..100 {
    let (frames, _remaining) = sampler.poll()?;
    for f in frames {
        println!("{} µs: {:?}", f.probe_us, f.values);
    }
    if let Some(lag) = sampler.stats().lag() {
        eprintln!("{}", lag.advice());
    }
    std::thread::sleep(sampler.poll_interval());
}
sampler.stop()?;
```

每帧带探针的时间戳和每个变量的值（不到期为 `None`，读取失败为 `Some(None)`）。`div` 使慢变量每几拍采一次；`phase` 为 `None` 的分频变量由 `Sampler::start` 错开相位，使每拍读取的字节数平均。变量数超过探针 INFO 报告的上限时，`Sampler::start` 在发出 SUB_START 之前返回错误。探针不估计耗时；`Stats::lag` 按跳拍、丢帧和缓冲余量判断采样跟不跟得上，`Lag::advice` 给出建议。探针上的采样停止（例如会话结束）后，`sampling_on_probe()` 为 false，缓冲里剩下的帧照样可以读完。完整示例：`sample`。

### 把探针共享给 Keil、pyOCD、probe-rs、OpenOCD

```rust
use xrdap::addr::Spec;
use xrdap::open::{open, OpenOptions};
use xrdap::share::{Protocol, Share};

fn main() -> xrdap::Result<()> {
    let probe = open(&Spec::parse("COM3"), &OpenOptions::default())?;
    let share = Share::start(probe, Protocol::Usbip, "127.0.0.1:3240", |e| println!("{e:?}"))
        .map_err(|(_, e)| e)?;
    println!("挂载：{}", share.attach_hint()); // usbip attach -r 127.0.0.1 -b 1-1
    std::io::stdin().read_line(&mut String::new()).ok();
    let _probe = share.stop(); // 对挂载的一方相当于拔掉设备，探针交回调用方
    Ok(())
}
```

`Protocol::Usbip` 把探针模拟为 USB CMSIS-DAP v2 设备，USB/IP 客户端挂载后各调试工具都能使用；`Protocol::OpenOcd` 是 OpenOCD 的 CMSIS-DAP TCP 后端。共享在后台线程中进行，探针归该线程所有，直到 `stop()`。DAP_TransferAbort 按 CMSIS-DAP 规范没有响应，共享时也不回复；探针读写出错时断开当前客户端并经回调报告原因。完整示例：`share`。

### 查找探针

```rust
use xrdap::discover::{find, group, Options};

for p in group(find(&Options::default())) {
    for link in &p.links {
        println!("{:?}  {}  -p {}", p.serial, link.addr, link.addr.spec());
    }
}
```

`find` 查找 USB 探针、局域网上经 mDNS 广播的探针和按 USB 描述符认出的乐鑫 USB 口，约 1.5 秒；同一块探针（按序列号）的几种连接合并为一项。`Options::ask_serial` 为 true 时还逐个打开其他候选串口询问 DAP_Info。`discover::serial_ports` 只读系统的串口列表和 USB 描述符。完整示例：`list`。

### 控制内核、配置 Wi-Fi

```rust
use xrdap::cortex::{self, AfterReset};

// probe：已连接目标的 Probe
cortex::halt(probe)?;
cortex::reset(probe, AfterReset::Run)?;
println!("{}", cortex::status(probe)?.state.text());

let wifi = probe.wifi_status()?;
println!("{}\n{}", wifi.describe(), wifi.history());
probe.wifi_set("lab", "12345678")?; // 探针随后重启
```

`cortex::reset` 在复位期间临时修改 DEMCR.VC_CORERESET，结束时（成功或失败）恢复原值。`wifi_status` 还报告最近一次清除热点参数的来源、本次启动的复位原因和 NVS 是否重建过；`wifi_clear` 发送带确认字节的清除请求 `86 02 A5`。完整示例：`wifi`。

### 测试

库的测试不需要硬件。工作区中的 `xrdap-sim` 依据 CMSIS-DAP 规范和协议文档模拟探针与目标，提供 `Transport` 实现和与探针相同的 TCP 服务；它只作为 dev-dependency 使用，不随库发布。

## 模块

| 模块 | 内容 |
|---|---|
| `addr` | 探针说明：一个字符串说明哪块探针、怎么连 |
| `open` | 按探针说明打开探针，或自动选择 |
| `discover` | 查找 USB、串口、局域网上的探针 |
| `dap` | `Probe`：请求与响应配对、CMSIS-DAP 命令、SWD/ADIv5 初始化、厂商命令的执行 |
| `vendor` | 厂商命令 0x80 到 0x86 的编码和解码 |
| `expr`、`types`、`elf` | 变量表达式、类型、DWARF |
| `watch`、`csv` | 探针端采样、CSV 记录 |
| `cortex` | 内核状态和停住、继续、复位 |
| `share` | 把探针共享为 USB/IP 设备或 OpenOCD TCP 后端 |
| `frame`、`transport` | 串口和 TCP 帧、三种传输 |

| 示例 | 作用 |
|---|---|
| `list` | 列出找到的探针，不打开串口 |
| `read_write` | 按变量名读取；给出第三个参数时先写入再读取 |
| `sample` | 以 1 ms 周期采样几个变量 1 秒，打印并统计 |
| `wifi` | 查询或保存探针的 Wi-Fi |
| `share` | 把探针共享为 USB/IP 设备，按回车停止 |

## 许可证

Apache-2.0。
