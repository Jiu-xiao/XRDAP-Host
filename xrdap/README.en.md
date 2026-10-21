# xrdap (Rust library)

[中文](README.md)

Host-side Rust library for XRDAP probes: open a probe (USB, serial, TCP), read and write target memory by variable name, sample variables on the probe at a fixed period, control the target core, configure the probe's Wi-Fi, and share the probe with other debug tools. The command-line and GUI tool `xrdap` is built on this library. The byte formats between probe and host are in the XRDAP-Core [protocol document](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.en.md).

## Adding the dependency

```toml
[dependencies]
xrdap = { git = "https://github.com/Jiu-xiao/XRDAP-Host", branch = "main" }
```

| feature | Default | Content |
|---|---|---|
| `elf` | on | Read ELF symbols and DWARF types for access by variable name |

Building on Linux needs `libudev-dev` (USB and serial port enumeration).

## Usage by task

Every example below has a complete version in `examples/`; run it from the repository root, e.g. `cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5`. The examples take the probe from the environment variable `XRDAP_PROBE` (same syntax as `xrdap -p`) and pick the only probe when it is not set.

### Opening a probe, reading and writing variables

```rust
use xrdap::addr::Spec;
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> xrdap::Result<()> {
    // Spec::Auto picks the only probe; Spec::parse("COM3"), Spec::parse("192.0.2.10")
    // or Spec::parse("DDEEFF") also work
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

`open` is the only way to open a probe; the syntax is in the `addr` module. Automatic selection and serial-number lookup look at USB, the LAN and Espressif USB ports recognised from their USB descriptor, without opening other serial ports; serial ports are opened with DTR and RTS deasserted. `Probe::connect` queries INFO first, requires vendor extension protocol version 2, then connects the target; when another connection owns the probe it returns `Error::Busy` ("探针正被另一条连接使用"). The host stops only sampling it started. `Resolver::resolve` accepts forms such as `state.speed`, `motor[1].pid->out`, `*ptr`, `(uint32_t***)p` and `0x20000010:u32`; `resolve_all` expands structs and arrays into members. A failed access returns `Error::Item` with the item status and the variable name. Full example: `read_write`.

### Sampling on the probe

```rust
use xrdap::watch::{Sampler, Var};

// probe: a Probe with the target connected; targets: variables from resolve
let vars = targets
    .iter()
    .enumerate()
    .map(|(i, t)| Var { item: t.item(i as u16), div: 1, phase: None })
    .collect();
// Period 1000 µs; one SUB_START carries the whole configuration and starts sampling
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

Each frame carries the probe timestamp and one value per variable (`None` when not due, `Some(None)` when the read failed). `div` samples slow variables every few ticks; divided variables with `phase` None are staggered by `Sampler::start` to even out the bytes read per tick. With more variables than the item limit reported in the probe's INFO, `Sampler::start` returns an error before sending SUB_START. The probe does not estimate timing; `Stats::lag` judges from skipped ticks, dropped frames and buffer fill whether sampling keeps up, and `Lag::advice` gives the advice. When sampling stops on the probe (for example because the session ended), `sampling_on_probe()` is false and the remaining frames can still be read. Full example: `sample`.

### Sharing the probe with Keil, pyOCD, probe-rs, OpenOCD

```rust
use xrdap::addr::Spec;
use xrdap::open::{open, OpenOptions};
use xrdap::share::{Protocol, Share};

fn main() -> xrdap::Result<()> {
    let probe = open(&Spec::parse("COM3"), &OpenOptions::default())?;
    let share = Share::start(probe, Protocol::Usbip, "127.0.0.1:3240", |e| println!("{e:?}"))
        .map_err(|(_, e)| e)?;
    println!("attach: {}", share.attach_hint()); // usbip attach -r 127.0.0.1 -b 1-1
    std::io::stdin().read_line(&mut String::new()).ok();
    let _probe = share.stop(); // the attached side sees the device unplugged; the probe comes back
    Ok(())
}
```

`Protocol::Usbip` presents the probe as a USB CMSIS-DAP v2 device that every tool can use once a USB/IP client attaches it; `Protocol::OpenOcd` is OpenOCD's CMSIS-DAP TCP backend. Sharing runs in a background thread that owns the probe until `stop()`. DAP_TransferAbort has no response per the CMSIS-DAP specification and gets none while sharing either; when the probe fails, the current client is disconnected and the reason is reported through the callback. Full example: `share`.

### Finding probes

```rust
use xrdap::discover::{find, group, Options};

for p in group(find(&Options::default())) {
    for link in &p.links {
        println!("{:?}  {}  -p {}", p.serial, link.addr, link.addr.spec());
    }
}
```

`find` looks for USB probes, probes advertised over mDNS on the LAN and Espressif USB ports recognised from their USB descriptor, for about 1.5 s; the connections of one probe (by serial number) are merged into one entry. With `Options::ask_serial` it also opens the other candidate serial ports and asks DAP_Info. `discover::serial_ports` only reads the system's port list and USB descriptors. Full example: `list`.

### Core control and Wi-Fi

```rust
use xrdap::cortex::{self, AfterReset};

// probe: a Probe with the target connected
cortex::halt(probe)?;
cortex::reset(probe, AfterReset::Run)?;
println!("{}", cortex::status(probe)?.state.text());

let wifi = probe.wifi_status()?;
println!("{}\n{}", wifi.describe(), wifi.history());
probe.wifi_set("lab", "12345678")?; // the probe restarts afterwards
```

`cortex::reset` changes DEMCR.VC_CORERESET for the reset only and restores DEMCR at the end, on success or failure. `wifi_status` also reports what last cleared the credentials, the reset reason of this boot and whether NVS was rebuilt; `wifi_clear` sends the clear request with its confirm byte, `86 02 A5`. Full example: `wifi`.

### Testing

The library tests need no hardware. The workspace crate `xrdap-sim` simulates the probe and target from the CMSIS-DAP specification and the protocol document, and provides a `raw::transport::Transport` and the probe's TCP service; it is a dev-dependency only and is not published with the library.

## Modules

`Error`, `Result`, `Item`, `ItemResult`, `item_status_text` and `WifiStatus` are at the crate root.

| Module | Content |
|---|---|
| `addr` | Probe spec: one string telling which probe and how to reach it |
| `open` | Open a probe from a spec, or pick the only one |
| `discover` | Find probes on USB, serial ports and the LAN |
| `dap` | `Probe`: request/response pairing, CMSIS-DAP commands, SWD/ADIv5 setup, vendor command execution |
| `expr`, `types`, `elf` | Variable expressions, types, DWARF |
| `watch`, `csv` | Probe-side sampling, CSV records |
| `cortex` | Core state, halt, resume, reset |
| `share` | Share the probe as a USB/IP device or OpenOCD TCP backend |
| `raw` | Protocol layer: encoding and decoding of vendor commands 0x80 to 0x86, serial and TCP framing, three transports. It follows the protocol document and its interface is not kept stable |

| Example | Purpose |
|---|---|
| `list` | List the probes found, without opening serial ports |
| `read_write` | Read by variable name; with a third argument, write first and then read |
| `sample` | Sample a few variables at 1 ms for 1 s, print them and the statistics |
| `wifi` | Query or save the probe's Wi-Fi |
| `share` | Share the probe as a USB/IP device until Enter |

## License

Apache-2.0.
