# xrdap (Rust library)

[中文](README.md)

The host-side Rust library for XRDAP probes. It opens a probe (USB, serial, Wi-Fi), reads and writes target memory by variable name, makes the probe sample variables at a fixed period, controls the target core, configures the probe's Wi-Fi and shares the probe with other debug tools. The command-line and GUI tool `xrdap` is built on it.

## Adding the dependency

```toml
[dependencies]
xrdap = { git = "https://github.com/Jiu-xiao/XRDAP-Host", tag = "v0.5.1" }
```

| feature | Default | Content |
|---|---|---|
| `usb` | on | CMSIS-DAP v2 USB transport |
| `elf` | on | Reads ELF symbols and DWARF types, access by variable name |
| `discover` | on | Probe discovery: USB, serial (asks each port for DAP_Info), LAN (mDNS) |
| `fake` | off | In-process simulated probe, for tests and demos |

On Linux, building `usb` and `discover` needs `libudev-dev`.

## Usage by scenario

Each example below has a full version in `examples/`. Run it from the repository root, for example `cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5`. The probe is chosen by the environment variable `XRDAP_PROBE` (same form as `xrdap -p`). If it is not set, the only probe is selected automatically.

### Opening a probe, reading and writing variables

```rust
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> xrdap::Result<()> {
    // None: pick the only probe automatically; Some("COM7"), Some("xrdap-93ef1c.local"), Some("93EF1C") also work
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

`Resolver::resolve` accepts forms such as `state.speed`, `motor[1].pid->out`, `*ptr`, `(uint32_t***)p` and `0x20000010:u32`. `resolve_all` expands structures and arrays into members. Full example: `read_write`.

### Sampling periodically on the probe

```rust
use xrdap::vendor::SubItem;
use xrdap::watch::Sampler;

// probe: a Probe already connected to the target; targets: variables from resolve
let items = targets
    .iter()
    .enumerate()
    .map(|(i, t)| SubItem { item: t.item(i as u16), div: 1, phase: 0 })
    .collect();
// Period 1000 µs; if the probe cannot keep up it returns StartError::Rejected, which carries the estimated time per frame
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

Each frame carries the probe timestamp and the value of each variable (`None` when not due, `Some(None)` when the read failed). `div` and `phase` make slow variables sample only every few ticks. `vendor::assign_phases` staggers the phases of the variables to even out the bytes read per tick. Full example: `sample`.

### Sharing the probe with Keil, pyOCD, probe-rs, OpenOCD

```rust
use xrdap::open::{open, OpenOptions};
use xrdap::share::{Protocol, Share};

fn main() -> xrdap::Result<()> {
    let probe = open(Some("COM7"), &OpenOptions::default())?;
    let share = Share::start(probe, Protocol::Usbip, "127.0.0.1:3240", |e| println!("{e:?}"))
        .map_err(|(_, e)| e)?;
    println!("挂载：{}", share.attach_hint()); // usbip attach -r 127.0.0.1 -b 1-1
    std::io::stdin().read_line(&mut String::new()).ok();
    let _probe = share.stop(); // the attached side sees the device unplugged; the probe is handed back
    Ok(())
}
```

`Protocol::Usbip` emulates the probe as a USB CMSIS-DAP v2 device. After attaching with a USB/IP client, all tools can use it. `Protocol::OpenOcd` is OpenOCD's CMSIS-DAP TCP backend. Sharing runs on a background thread, and the probe belongs to it until `stop()`. Full example: `share`.

### Finding probes

```rust
let probes = xrdap::discover::group(xrdap::discover::find(&Default::default()));
print!("{}", xrdap::discover::table(&probes));
```

It searches USB, serial and the LAN in about 1.5 seconds and merges the connections of one probe (by serial number). Full example: `list`.

### Controlling the core, configuring Wi-Fi

```rust
use xrdap::cortex::{self, AfterReset};

// probe: a Probe already connected to the target
cortex::halt(probe)?;
cortex::reset(probe, AfterReset::Run)?;
println!("{}", cortex::status(probe)?.state.text());

println!("{}", xrdap::wifi::status(probe)?.describe());
xrdap::wifi::set(probe, "lab", "password")?; // the probe restarts afterwards
```

Full example: `wifi`.

### Testing without hardware

With the `fake` feature, `xrdap::fake::FakeProbe` simulates the probe and target in process (memory, DHCSR, sampling). It can fill memory from an ELF:

```rust
let fake = xrdap::fake::FakeProbe::new();
let probe = xrdap::dap::Probe::new(Box::new(fake.clone()));
```

## Modules

| Module | Content |
|---|---|
| `open` | Open a probe from one string, or choose automatically |
| `discover` | Find probes on USB, serial and the LAN |
| `dap` | `Probe`: CMSIS-DAP commands, SWD/ADIv5 initialization, vendor commands, `identity()` |
| `expr`, `types`, `elf` | Variable expressions, types, DWARF |
| `watch`, `csv` | Probe-side sampling, CSV recording |
| `cortex` | Core state, halt, resume, reset |
| `wifi` | Query, set and clear the probe's Wi-Fi |
| `share` | Share the probe as a USB/IP device or an OpenOCD TCP backend |
| `vendor`, `frame`, `transport` | Vendor command encoding, serial and TCP frames, the three transports |
| `fake` | Simulated probe (feature `fake`) |

The byte formats of the vendor commands are in [docs/protocol.en.md of XRDAP-ESP](https://github.com/Jiu-xiao/XRDAP-ESP/blob/main/docs/protocol.en.md).

| Example | Action |
|---|---|
| `list` | List probes on USB, serial and the LAN |
| `read_write` | Read by variable name; with a third argument, write first and then read |
| `sample` | Sample a few variables at a 1 ms period for 1 second, print and summarize |
| `wifi` | Query or save the probe's Wi-Fi |
| `share` | Share the probe as a USB/IP device; press Enter to stop |

## License

Apache-2.0.
