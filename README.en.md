[中文](README.md)

# XRDAP-Host

Host-side tools for [XRDAP](https://github.com/Jiu-xiao/XRDAP-ESP) debug probes (ESP32-S3, ESP32-C3), written in Rust for Windows and Linux. They connect to a probe over USB, serial or TCP, read and write target MCU memory by variable name, let the probe sample variables at a fixed period and record them as CSV, halt, resume and reset the target core, and configure the probe's Wi-Fi. Variable addresses and types come from the DWARF debug information in the target program's ELF.

| Way to use | Name | Purpose |
|---|---|---|
| Rust crate | `xrdap` | Protocol, the three links (USB, serial, TCP), probe discovery and DWARF parsing, callable from other programs |
| Command line | `xrdap` | Lists probes, reads and writes variables, samples periodically and exports CSV, controls the core, configures Wi-Fi |
| GUI | `xrdap-gui` | Searches ELF variables into a watch table, plots live curves, writes variables while sampling, records CSV |
| Serial-to-TCP bridge | `xrdap-forward` | Lets OpenOCD use a serial-connected probe through `cmsis-dap backend tcp` (the S3 on-board serial port, the C3 USB port) |

## Download and build

Each version on [Releases](../../releases) provides the following files, plus a checksum file `SHA256SUMS`:

| File | Content |
|---|---|
| `xrdap-windows-x86_64.exe`, `xrdap-linux-x86_64` | Command line |
| `xrdap-gui-windows-x86_64.exe`, `xrdap-gui-linux-x86_64` | GUI |
| `xrdap-forward-windows-x86_64.exe`, `xrdap-forward-linux-x86_64` | Serial-to-TCP bridge |

Building from source requires a Rust toolchain:

```
git clone https://github.com/Jiu-xiao/XRDAP-Host.git
cd XRDAP-Host
cargo build --release
```

The programs are placed in `target/release/`.

Building on Linux requires the development packages first (Ubuntu):

```
sudo apt install libudev-dev libgtk-3-dev libxkbcommon-dev
```

Using a probe over USB on Linux requires a udev rule that opens the device permissions. The probe's VID:PID is `1D50:6199`. Create `/etc/udev/rules.d/99-xrdap.rules` with a single line:

```
SUBSYSTEM=="usb", ATTRS{idVendor}=="1d50", ATTRS{idProduct}=="6199", MODE="0666"
```

Then reload the rules and replug the probe:

```
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Choosing a probe

The command line selects a probe with `--probe` (short form `-p`). The crate's `open()` accepts the same string.

| Form | Meaning |
|---|---|
| omitted | Auto-select the only probe |
| `usb`, `usb:<serial>` | USB (CMSIS-DAP v2) |
| `COM35`, `/dev/ttyUSB0`, `serial:<port>` | Serial port |
| `192.168.0.108`, `192.168.0.108:3241`, `xrdap-93ef1c.local`, `tcp:<host>` | TCP, default port 3241 |
| `XRobot-XRDAP-E8069093EF1C`, `93EF1C` | Look up by serial number (full or trailing characters, case-insensitive); when a probe has several links, the first of USB, serial, TCP is used |

`--usb[=<serial>]`, `--serial <port>` and `--tcp <address[:port]>` are equivalent to the matching `--probe` forms. Only one of `--probe`, `--usb`, `--serial` and `--tcp` may be given.

Without a probe argument, the selection runs in this order:

1. With exactly one USB probe, it is used directly; serial ports and the LAN are not scanned.
2. Otherwise serial ports and the LAN (mDNS service `_xrdap._tcp`) are scanned together for about 1.5 seconds. Only common USB-to-serial chips (WCH CH34x, Silicon Labs CP210x, FTDI) and the Espressif built-in USB Serial/JTAG are tried; a `DAP_Info` request is sent to each, and a port that answers counts as a probe.
3. USB, serial and TCP links with the same serial number merge into one probe. Exactly one probe is used; none is an error; several is an error that includes a list in `xrdap list` format.

`xrdap list` prints every probe, the links available for each, and the matching `--probe` form. Output with two probes:

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

(The program prints these labels in Chinese: 串口 = serial, TCP link lines show the mDNS host name in parentheses.) In this setup `xrdap info --probe 93EF1C` selects the USB link of the second probe, and `xrdap info --probe COM116` selects the serial link of the first.

Opening a serial port makes the USB-to-serial chip on S3 boards reset the probe once; xrdap waits up to 10 seconds for the probe to start. The Espressif built-in USB Serial/JTAG port resets the chip through DTR/RTS, and xrdap leaves these two lines unchanged automatically. For other serial ports that need the same handling, add `--keep-lines`.

## Command line: xrdap

Variable expressions are resolved from the DWARF information in the ELF (compile with `-g`); `--elf <file>` names the ELF. Struct members, array indices, pointers and casts are accepted, for example `state.speed`, `motor[1].pid->out`, `head->next->value`, `(uint32_t***)p`. Without an ELF, an address and a type are written directly, for example `0x20000010:u32`; the type is one of `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64`, `f32`, `f64`.

The connection options apply to every command:

| Option | Purpose |
|---|---|
| `-p, --probe <probe>` | Select the probe, see the previous section |
| `--elf <file>` | ELF of the target program |
| `--clock <Hz>` | SWD clock, for example `4000000`, `4M`, `500k`; default 4M |
| `--baud <baud>` | Serial baud rate, must match the probe firmware; default 4000000 |
| `--keep-lines` | Leave DTR/RTS unchanged after opening the serial port |
| `--timeout-ms <ms>` | Timeout for one response; default 2000 |
| `--fake` | Use the built-in simulated probe, no hardware needed; with `--elf` the simulated memory is filled from the ELF |

The examples use the test program [g431-test.c](xrdap/tests/fixtures/g431-test.c) and its ELF. After `cd xrdap/tests/fixtures` from the repository root, `g431-test.elf` can be referenced directly. The read, write, sampling and core-control commands need a probe connected to a board running this program, or `--fake` for the simulated probe.

### list: list probes

```
xrdap list
```

The output is shown in the previous section.

### info: probe and target information

```
> xrdap info --probe 93EF1C --no-target
连接：USB XRobot-XRDAP-E8069093EF1C
探针：XRobot XR-DAP-S3，序列号 XRobot-XRDAP-E8069093EF1C，固件 0.2.0
CMSIS-DAP：2.0.0，包长 1024 字节，最多 4 个包在途
厂商扩展：版本 1，功能 READ、WRITE、采样、8 字节访问（0x1F）
  每次最多 32 个条目，最多 8 级指针，采样缓冲 16384 字节
  探针时间 929.113669 s，单次 SWD 访问耗时 26 µs（上次 SUB_SET 时标定）
```

The lines report the link, the probe name, serial number and firmware version, the CMSIS-DAP version and packet size, the vendor extension features, and the probe time. Without `--no-target`, xrdap also connects to the target and appends a target line (DPIDR and SWD clock) and a core line (core model, run state, DHCSR).

### vars: list global variables in the ELF

```
> xrdap --elf g431-test.elf vars cnt
0x20000008  cnt_a                    uint32_t，4 字节
0x2000000C  cnt_b                    uint32_t，4 字节
0x20000010  cnt64                    uint64_t，8 字节
```

The text argument filters by name; without it all variables are listed. Each line shows address, name, type and size in bytes. `vars` does not connect to a probe.

### read: read variables

`chain_p1` is a three-level pointer chain and `null_p1` is a null pointer:

```
> xrdap --elf g431-test.elf read v_u16 "***(uint32_t***)chain_p1" "*null_p1"
v_u16                    = 4660 (0x1234)
***(uint32_t***)chain_p1 = 3405691582 (0xCAFEBABE)
*null_p1                   读取失败：第 1 级指针 null_p1 为空（NULL）（地址 [0x20000038]）
```

Values are formatted by DWARF type: signed integers, floats, enum names and pointers each use their own format. Structs and arrays are expanded into their members. The probe accesses memory in 1, 2, 4 or 8 bytes, so reading a bit-field member is an error that also gives the address form of the containing byte (for example `0x20000043:u8`). The last line above is the failure message: pointer level 1, `null_p1`, is NULL, at address `[0x20000038]`.

### write: write variables

```
> xrdap --elf g431-test.elf write v_u8=0x5A
v_u8 ← 0x5A，读回 90 (0x5A)
```

Integers are decimal or `0x` hexadecimal, floats are written as decimals, and enums by name. Several `variable=value` pairs can be given; each is read back once after writing (the output shows the value read back). If any write fails, the command exits with an error.

### watch: periodic sampling

```
> xrdap --elf g431-test.elf watch cnt_a cnt_b@10 v_u32@100 --period 1ms --csv log.csv
采样 3 个变量，周期 1000 µs，估计每帧耗时 163 µs，一帧最多 23 字节
  cnt_b 每 10 拍采一次，相位 1
  v_u32 每 100 拍采一次，相位 0
写入 log.csv，按 Ctrl-C 停止
```

The output reports 3 variables sampled with a period of 1000 µs, an estimated 163 µs per frame, at most 23 bytes per frame, the divider and phase of each divided variable, and the CSV file; Ctrl-C stops sampling.

| Option | Purpose |
|---|---|
| `--period <period>` | Sampling period: `1000` (µs), `500us`, `2ms`; default 1ms |
| `--csv <file>` | Write every frame to a CSV file |
| `--duration <seconds>` | Stop after this many seconds; without it sampling runs until Ctrl-C |
| `--all` | Print every frame in the terminal; by default at most one line every 0.1 seconds |
| `--no-overwrite` | Drop new frames when the probe's sample buffer is full; by default the oldest frames are overwritten |

`variable@divider` samples once every N ticks, and `variable@divider:phase` fixes the phase. When no phase is given, xrdap staggers the phases of the divided variables so that they avoid the same tick as far as possible. At most 32 variables can be sampled at once.

The CSV has one row per frame. The `host_time` column is Unix time on the computer's clock, converted from the SUB_READ round-trip time; `t` is the probe time relative to the first frame; `sample_tick` is the probe's tick number. A variable not sampled on that tick is empty, and a failed read is `ERR`:

```
host_time,t,sample_tick,cnt_a,cnt_b,v_u32
1792345704.807567,0.000000,0,0,,2309737967
1792345704.808567,0.001000,1,1,,
1792345704.809567,0.002000,2,2,,
```

Without `--csv`, the terminal prints columns for time, tick and the latest value of each variable. For example, sampling for 5 seconds:

```
xrdap --elf g431-test.elf watch cnt_a cnt_b@10 --period 1ms --duration 5
```

After Ctrl-C or the `--duration` time, xrdap stops sampling, drains the buffer, and prints the frame count, the skipped ticks reported by the probe, the frames dropped from the buffer, the gaps in frame numbers and the number of failed values. When the period is shorter than the probe's estimated frame time, the probe rejects the configuration, and xrdap prints the estimate and a usable period.

While sampling, the probe accepts only `DAP_Info`, `DAP_HostStatus` and vendor commands and answers every other command with `0xFF`, so sampling cannot run at the same time as OpenOCD debugging.

### halt, run, reset: core control

```
xrdap halt
xrdap run
xrdap reset
xrdap reset --halt
```

`halt` halts the core, `run` resumes a halted core, `reset` performs a system reset (AIRCR.SYSRESETREQ) and runs, and `reset --halt` stops at the reset vector after the reset. Each command prints the core state, for example with the simulated probe:

```
> xrdap --fake halt
内核：已停住（DHCSR 0x00030003）
```

(The line reads "core: halted".) These commands access DHCSR and AIRCR through vendor commands and also work while sampling. `xrdap info` shows the target core model and run state.

### wifi: configure the probe's Wi-Fi

The configuration goes through the DAP link itself; no separate serial terminal is needed. Firmware 0.2.0 or later is required.

```
> xrdap wifi status
热点 lab，已连接，IP 192.168.0.108，信号 -25 dBm
```

The line reads: hotspot `lab`, connected, IP 192.168.0.108, signal -25 dBm.

```
xrdap wifi set lab 12345678
xrdap wifi set open-hotspot
xrdap wifi clear
```

`wifi set <hotspot> [password]` saves the hotspot parameters. The hotspot name is at most 32 bytes, the password 8 to 63 bytes, and an open network has no password. After saving, the probe restarts after about 0.5 seconds and connects to that hotspot, which ends the current connection. `wifi clear` removes the hotspot parameters, and the probe restarts without joining Wi-Fi. `wifi status` does not show the password.

## GUI: xrdap-gui

```
xrdap-gui
```

![xrdap-gui: two charts, normalization, hover readout](docs/images/xrdap-gui.png)

The window has the connection and variable settings on the left, the control bar and watch table at the top of the middle, and the plot area in the rest. The interface text is Chinese; the quoted labels below are translated.

- Found probes ("找到的探针"): USB, serial and LAN probes are searched on startup, one link per row; clicking a row fills in the connection parameters. "重新查找" (search again) repeats the search.
- Connection: choose USB, serial, TCP or simulated (a simulated probe, no hardware), fill in the parameters and the SWD clock, and press "连接" (connect). USB and serial have a "刷新" (refresh) button that lists the devices again.
- Core state: after connecting, the core model and run state (running, sleeping, halted, lockup) are shown and refreshed every 0.5 seconds, with buttons for halt/resume, reset, and reset-and-halt. They also work while sampling.
- ELF: selects the ELF with `-g` debug information.
- Add variables: search the global variable list and press `+` to add to the watch table, or type an expression such as `motor[1].pid->out` in the expression box and press "添加" (add). Structs and arrays expand into members. The "只看自己的变量" (own variables only) checkbox hides variables from HAL, CMSIS, RTOS, middleware and the C library, judged by source file path.
- Control bar: sampling period, start/stop, read once, record CSV, plot window length, normalize, pause following, clear plot. While sampling it shows the frame rate and the counts of probe-dropped frames, skipped ticks, missing frames and failed reads; any non-zero count turns into the warning color. When the target is halted, a note says the variables do not change. When the period is shorter than the probe's estimated frame time, the probe rejects it and the GUI replaces the period with the suggested value.
- Watch table: one row per variable, with a divider, a plot checkbox, the chart number, and the current value (briefly highlighted when it changes). A value typed into the write box is written on Enter or on "写" (write) and read back once; this also works while sampling. Newly added pointers are not plotted by default.
- Plots: variables can be split over 1 to 4 charts whose time axes are linked. Normalization scales each curve to the same height using its minimum and maximum in the window. Hovering shows the value of every curve at that time; a click places cursor A, Shift+click places cursor B, and the readout shows Δt and the values at both. Each curve is decimated to the screen width with min/max reduction, so spikes survive, and the last 120 seconds are kept. After pausing the follow mode, the plot can be dragged and zoomed while data keeps arriving.
- CSV: the format is the same as `xrdap watch --csv`. "录制 CSV…" (record CSV) is available while sampling and records from the moment the file is chosen.

First use:

1. Run `xrdap-gui` and click the wanted link under the found probes.
2. Press "连接" (connect); the core model and run state appear.
3. Select the target program's ELF in the ELF section.
4. Type a variable name in the search box and press `+` to add it to the watch table.
5. Enter a sampling period in the period box (for example `1ms`) and press "开始采样" (start sampling); the curves start scrolling.
6. Type a value into a write box and press Enter to modify a variable; press "录制 CSV…" and choose a file to start recording.

The connection kind, ELF path, watch table, period, window length and normalization are saved on exit and restored on the next start. The interface needs a Chinese font on the system (Microsoft YaHei or SimHei on Windows, Noto Sans CJK or WenQuanYi on Linux); otherwise Chinese text appears as boxes.

## Rust crate

Add a git dependency to `Cargo.toml`:

```toml
[dependencies]
xrdap = { git = "https://github.com/Jiu-xiao/XRDAP-Host" }
```

All crate features are enabled by default:

| Feature | Content |
|---|---|
| `usb` | CMSIS-DAP v2 USB bulk transport |
| `elf` | Reading ELF symbols and DWARF types |
| `discover` | Probe discovery: serial ports are queried one by one, the LAN uses mDNS (depends on `usb`) |

Quick start: open a probe, connect to the SWD target and read a variable by name.

```rust
use xrdap::expr::Resolver;
use xrdap::open::{open, OpenOptions};

fn main() -> xrdap::Result<()> {
    // Auto-select the only probe; Some("COM35"), Some("192.168.0.108"), Some("93EF1C") also work
    let mut probe = open(None, &OpenOptions::default())?;
    probe.connect_swd(4_000_000)?;

    let mut syms = xrdap::elf::load(std::path::Path::new("app.elf"))?;
    let speed = Resolver::new(&mut syms).resolve("motor.speed")?;
    println!("{} = {}", speed.name, speed.read_text(&mut probe)?);
    Ok(())
}
```

`Target` provides `read` (returns bytes), `read_text` (formats by type) and `write` (writes bytes, obtainable from text with `target.ty.parse("1.5")`). Module overview:

| Module | Content |
|---|---|
| `open` | Open a probe from one string, or auto-select |
| `discover` | Find probes on USB, serial ports and the LAN |
| `dap` | CMSIS-DAP commands, SWD/ADIv5 setup, vendor commands |
| `expr`, `types`, `elf` | Variable expressions, types, DWARF |
| `watch`, `csv` | Probe-side sampling, CSV records |
| `cortex` | Core state, halt, resume, reset |
| `wifi` | Query, set and clear the probe's Wi-Fi |
| `vendor`, `frame`, `transport` | Vendor command encoding, serial and TCP framing, the three transports |
| `fake` | In-process simulated probe for tests |

The byte formats of the vendor commands are in [docs/protocol.md of XRDAP-ESP](https://github.com/Jiu-xiao/XRDAP-ESP/blob/main/docs/protocol.md).

`xrdap/examples/` holds four examples, run from the repository root. The read/write, sampling and Wi-Fi examples take the probe from the environment variable `XRDAP_PROBE` (same forms as `--probe`); when unset, the only probe is auto-selected.

| Example | Purpose | Command |
|---|---|---|
| `list` | Lists the probes on USB, serial ports and the LAN | `cargo run -p xrdap --example list` |
| `read_write` | Reads a variable by name; with a third argument, writes it first and then reads | `cargo run -p xrdap --example read_write -- app.elf motor.kp 1.5` |
| `sample` | Makes the probe sample variables at a 1 ms period, fetches 1 second of data, prints one line per 100 frames and the statistics | `cargo run -p xrdap --example sample -- app.elf motor.speed motor.current` |
| `wifi` | Queries the probe's Wi-Fi status; with a hotspot name and password, saves them | `cargo run -p xrdap --example wifi` |

Selecting a probe, for example:

```
XRDAP_PROBE=COM35 cargo run -p xrdap --example read_write -- app.elf motor.speed
```

## xrdap-forward: serial to TCP

```
xrdap-forward --serial COM35
```

| Option | Default | Purpose |
|---|---|---|
| `--serial` | none, required | Serial port of the probe, for example `COM35` or `/dev/ttyUSB0` |
| `--baud` | 4000000 | Serial baud rate, matching the firmware |
| `--listen` | `127.0.0.1:3241` | TCP address for OpenOCD to connect to |
| `--timeout-ms` | 2000 | Timeout for one request to get a response (ms) |
| `--keep-lines` | off | Leave DTR/RTS unchanged after opening the port |
| `--verbose` | off | Print every forwarded request and response |

Opening the port makes DTR/RTS of the USB-to-serial chip reset the ESP32 once. xrdap-forward queries the probe repeatedly until it has started (up to 10 seconds), prints the DAP packet size the probe reports, and then starts listening. An ESP32-C3 on its built-in USB Serial/JTAG does not have this reset; add `--keep-lines`. Only one OpenOCD is served at a time.

OpenOCD connects to 127.0.0.1 with the TCP backend:

```tcl
adapter driver cmsis-dap
cmsis-dap backend tcp
cmsis-dap tcp host 127.0.0.1
cmsis-dap tcp port 3241
transport select swd
source [find target/stm32g4x.cfg]
adapter speed 4000
```

On Windows, the TCP backend of Espressif's OpenOCD v0.12.0-esp32-20260831 has a bug (it treats "no data yet" as an error), so two more lines are needed:

```tcl
cmsis-dap quirk enable
cmsis-dap tcp min_timeout 10
```

The cost is that OpenOCD sends one packet at a time, which lowers throughput to about one third. OpenOCD upstream has fixed this (commit e89e73839d53); with a build containing that commit the two lines can be removed. They are not needed on Linux.

## Tests

```
cargo test --release --workspace
```

The tests include an in-process simulated probe and need no hardware. They cover frame encoding and decoding, item encoding, sample-frame decoding, DWARF parsing, and core halt and reset. CI also checks `cargo fmt` and `cargo clippy`.

## License

Apache-2.0, see [LICENSE](LICENSE).
