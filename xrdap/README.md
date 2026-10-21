# xrdap（Rust 库）

[English](README.en.md)

XRDAP 探针的电脑端 Rust 库：打开探针（USB、串口、Wi-Fi），按变量名读写目标内存，让探针按固定周期采样变量，控制目标内核，配置探针 Wi-Fi，把探针共享给别的调试工具。命令行和图形界面工具 `xrdap` 就建立在它上面。

## 加入依赖

```toml
[dependencies]
xrdap = { git = "https://github.com/Jiu-xiao/XRDAP-Host", tag = "v0.5.2" }
```

| feature | 默认 | 内容 |
|---|---|---|
| `usb` | 开 | CMSIS-DAP v2 USB 传输 |
| `elf` | 开 | 读 ELF 符号和 DWARF 类型，按变量名访问 |
| `discover` | 开 | 找探针：USB、串口（挨个问 DAP_Info）、局域网（mDNS） |
| `fake` | 关 | 进程内的模拟探针，测试和演示用 |

Linux 上编译 `usb` 和 `discover` 要 `libudev-dev`。

## 按场景使用

下面的例子在 `examples/` 里都有完整版本，在仓库根目录运行，例如 `cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5`。探针用环境变量 `XRDAP_PROBE` 指定（写法同 `xrdap -p`），不设时自动选唯一的那块。

### 打开探针，读写变量

```rust
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> xrdap::Result<()> {
    // None：只有一块探针时自动选；也可以写 Some("COM7")、Some("xrdap-93ef1c.local")、Some("93EF1C")
    let mut probe = open(None, &OpenOptions::default())?;
    probe.connect_swd(4_000_000)?;

    let mut syms = xrdap::elf::load(std::path::Path::new("app.elf"))?;
    let mut resolver = Resolver::new(&mut syms);
    let kp = resolver.resolve("motor.kp")?;
    println!("{} = {}", kp.name, kp.read_text(&mut probe)?);
    kp.write(&mut probe, &kp.ty.parse("1.5")?)?;
    Ok(())
}
```

`Resolver::resolve` 接受 `state.speed`、`motor[1].pid->out`、`*ptr`、`(uint32_t***)p`、`0x20000010:u32` 这类写法；`resolve_all` 把结构体和数组展开成成员。完整示例：`read_write`。

### 让探针周期采样

```rust
use xrdap::vendor::SubItem;
use xrdap::watch::Sampler;

// probe：已连接目标的 Probe；targets：resolve 得到的变量
let items = targets
    .iter()
    .enumerate()
    .map(|(i, t)| SubItem { item: t.item(i as u16), div: 1, phase: 0 })
    .collect();
// 周期 1000 µs；探针算不过来时返回 StartError::Rejected，里面有估计的单帧耗时
let mut sampler = Sampler::start(probe, items, 1000, false)
    .map_err(|e| xrdap::Error::protocol(format!("{e:?}")))?;
for _ in 0..100 {
    let (frames, _remaining) = sampler.poll()?;
    for f in frames {
        println!("{} µs: {:?}", f.probe_us, f.values);
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
}
sampler.stop()?;
```

每帧带探针的时间戳和每个变量的值（不到期为 `None`，读失败为 `Some(None)`）。`div` 和 `phase` 让慢变量每几拍才采一次。`vendor::assign_phases` 把各变量的相位错开，平均每拍读的字节数。完整示例：`sample`。

### 把探针共享给 Keil、pyOCD、probe-rs、OpenOCD

```rust
use xrdap::open::{open, OpenOptions};
use xrdap::share::{Protocol, Share};

fn main() -> xrdap::Result<()> {
    let probe = open(Some("COM7"), &OpenOptions::default())?;
    let share = Share::start(probe, Protocol::Usbip, "127.0.0.1:3240", |e| println!("{e:?}"))
        .map_err(|(_, e)| e)?;
    println!("挂载：{}", share.attach_hint()); // usbip attach -r 127.0.0.1 -b 1-1
    std::io::stdin().read_line(&mut String::new()).ok();
    let _probe = share.stop(); // 对挂载的一方来说设备被拔掉，探针交回来
    Ok(())
}
```

`Protocol::Usbip` 把探针模拟成 USB CMSIS-DAP v2 设备，用 USB/IP 客户端挂载后所有工具都能用；`Protocol::OpenOcd` 是 OpenOCD 的 CMSIS-DAP TCP 后端。共享在后台线程里进行，探针归它所有，直到 `stop()`。完整示例：`share`。

### 找探针

```rust
let probes = xrdap::discover::group(xrdap::discover::find(&Default::default()));
print!("{}", xrdap::discover::table(&probes));
```

找遍 USB、串口、局域网，约 1.5 秒，同一块探针（按序列号）的几种连接合在一起。完整示例：`list`。

### 控制内核、配 Wi-Fi

```rust
use xrdap::cortex::{self, AfterReset};

// probe：已连接目标的 Probe
cortex::halt(probe)?;
cortex::reset(probe, AfterReset::Run)?;
println!("{}", cortex::status(probe)?.state.text());

println!("{}", xrdap::wifi::status(probe)?.describe());
xrdap::wifi::set(probe, "lab", "password")?; // 探针随后重启
```

完整示例：`wifi`。

### 没有硬件时测试

开 `fake` feature 后，`xrdap::fake::FakeProbe` 在进程内模拟探针和目标（内存、DHCSR、采样），可以按 ELF 填内存：

```rust
let fake = xrdap::fake::FakeProbe::new();
let probe = xrdap::dap::Probe::new(Box::new(fake.clone()));
```

## 模块

| 模块 | 内容 |
|---|---|
| `open` | 按一个字符串打开探针，或自动选择 |
| `discover` | 找 USB、串口、局域网上的探针 |
| `dap` | `Probe`：CMSIS-DAP 命令、SWD/ADIv5 初始化、厂商命令、`identity()` |
| `expr`、`types`、`elf` | 变量表达式、类型、DWARF |
| `watch`、`csv` | 探针端采样、CSV 记录 |
| `cortex` | 内核状态和停住、继续、复位 |
| `wifi` | 探针 Wi-Fi 的查询、设置、清除 |
| `share` | 把探针共享成 USB/IP 设备或 OpenOCD TCP 后端 |
| `vendor`、`frame`、`transport` | 厂商命令编码、串口和 TCP 帧、三种传输 |
| `fake` | 模拟探针（feature `fake`） |

厂商命令的字节格式见 [XRDAP-Core 的 docs/protocol.md](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.md)。

| 示例 | 作用 |
|---|---|
| `list` | 列出 USB、串口、局域网上的探针 |
| `read_write` | 按变量名读；给出第三个参数时先写再读 |
| `sample` | 1 ms 周期采样几个变量 1 秒，打印并统计 |
| `wifi` | 查询或保存探针的 Wi-Fi |
| `share` | 把探针共享成 USB/IP 设备，回车停止 |

## 许可证

Apache-2.0。
