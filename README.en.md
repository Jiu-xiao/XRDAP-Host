# XRDAP-Host

[中文](README.md)

`xrdap` is the host tool for the [XRDAP](https://github.com/Jiu-xiao/XRDAP-ESP) debug probes, for Windows and Linux. It is a single executable: double-clicking it or running it without arguments opens the GUI, and running it with a command works as a command-line tool.

It provides:

- Reading and writing target MCU memory by variable name, sampling variables on the probe at a fixed period, live plots and CSV export. Variable addresses and types come from the debug information in the ELF.
- Sharing the probe with Keil, pyOCD, probe-rs and OpenOCD, so a probe connected over a serial port also works as a standard USB CMSIS-DAP probe.
- Halting, resuming and resetting the target core; configuring the probe's Wi-Fi.

To call these functions from your own program, use the Rust library [`xrdap`](xrdap/README.en.md). The protocol between probe and host is described in the XRDAP-Core [protocol document](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.en.md).

## Download

Download the file for your platform from [Releases](../../releases), rename it to `xrdap` (`xrdap.exe` on Windows) and put it on PATH:

| Platform | File |
|---|---|
| Windows x86-64 | `xrdap-windows-x86_64.exe` |
| Linux x86-64 | `xrdap-linux-x86_64` |

Checksums are in `SHA256SUMS` on the same page. The GUI on Linux needs a CJK font (Noto Sans CJK or WenQuanYi). Access to the USB probe as a normal user needs this udev rule:

```
SUBSYSTEM=="usb", ATTR{idVendor}=="1d50", ATTR{idProduct}=="6199", MODE="0666"
```

## Choosing a probe

Every command that opens a probe takes `-p`; it can be omitted when there is only one probe. `xrdap list` lists the probes found and the `-p` form of each connection:

```
xrdap list
xrdap list --ask-serial
```

`list` shows USB probes, probes advertised over mDNS on the LAN, and the USB port built into Espressif chips (USB Serial/JTAG of the ESP32-C3), recognised from its USB descriptor. Other serial ports are listed by name and serial chip without being opened; with `--ask-serial` each of them is opened and asked for DAP_Info, which may reset a development board connected to it.

| `-p` | Meaning |
|---|---|
| omitted | Use the only probe (searched on USB, the LAN and Espressif USB ports) |
| `usb`, `usb:<serial>` | USB |
| `COM3`, `/dev/ttyUSB0`, `serial:<port>` | Serial port. The baud rate follows the serial chip: CH343 and similar try 4000000 first, CP2102 and CH340 use 921600, other chips try both; `--baud` sets it |
| `192.0.2.10`, `192.0.2.10:3241`, `xrdap-aabbcc.local`, `tcp:<host>` | TCP (the probe's Wi-Fi, or `xrdap share openocd`) |
| `XRobot-XRDAP-AABBCCDDEEFF`, `DDEEFF` | Full serial number or its last digits |

One connection (USB, serial or TCP) uses the probe at a time; commands from another connection get a busy reply, which `xrdap` shows as 探针正被另一条连接使用 (the probe is used by another connection), while DAP_Info is still answered. `xrdap` stops only sampling it started. The probe firmware's vendor extension must be protocol version 2 (see the protocol document); a version 1 probe needs a firmware update first.

Serial ports are opened with DTR and RTS both deasserted, as Espressif's idf_monitor does; the auto-download circuit of ESP32 boards drives EN and GPIO0 from these lines. A probe behind a USB-serial chip is selected by its port name.

## Usage by task

### Plotting variables and changing parameters while running

1. Run `xrdap` without arguments. Click a row under 找到的探针 (found probes) to fill the 探针 (probe) field (or type it, same syntax as `-p`), then click 连接 (connect). The GUI shows the core type and its run state.
2. Choose the target program's ELF (built with `-g`) in the ELF field. The ELF is loaded in the background.
3. Search the variable list and click `+` to add a variable to the watch table, or type an expression such as `motor[1].pid->out` and click 添加 (add). Structs and arrays expand into their members.
4. Enter `1ms` as the period and click 开始采样 (start sampling); the plot starts scrolling.
5. Type a value into a watch row and press Enter to change the variable; click 录制 CSV… (record CSV) to start recording.

The probe times the sampling, so the period is not limited by USB or Wi-Fi round trips. The probe does not estimate timing; keeping up is judged from run-time counters: skipped ticks mean SWD cannot keep up, dropped frames or a nearly full buffer mean reading cannot keep up, and the GUI then shows advice below the statistics (longer period, larger divider or fewer variables). Plots can be split across 1 to 4 charts with linked time axes; hovering shows values, and click and Shift+click place cursors A and B. The probe, ELF, watch table and other settings are saved on exit. The ELF cannot be reloaded while sampling.

### Reading and writing variables, CSV export on the command line

```
xrdap vars --elf app.elf motor
xrdap read --elf app.elf motor.speed motor.state
xrdap read 0x20000010:u32
xrdap write --elf app.elf motor.kp=1.5 mode=RUN
xrdap watch --elf app.elf motor.speed motor.current@10 --period 1ms --csv log.csv --duration 10
xrdap watch -p COM3 --elf app.elf motor.speed@5:2 --period 500us --no-overwrite --all
```

`vars` lists the global variables in the ELF whose names contain the given text. `read` reads once; `write` writes and reads back; `watch` samples on the probe at a fixed period: `var@10` samples every 10th tick, `var@5:2` also sets the phase, and without `--duration` sampling runs until Ctrl-C. `--no-overwrite` drops new frames when the buffer is full (by default the oldest frame is overwritten), and `--all` prints every frame. Each `--csv` row is one frame: host time, probe time, tick number, then one column per variable, empty when not due and `ERR` when the read failed.

Variable syntax: `counter`, `state.speed`, `motor[1].pid->out`, `*ptr`, `p->q->x`, `(uint32_t***)p`, or a raw address `0x20000010:u32`. Without `--elf` only raw addresses are available.

### Debugging with Keil, pyOCD, probe-rs, OpenOCD

| Probe connection | Method |
|---|---|
| ESP32-S3 on USB | The probe is a USB CMSIS-DAP probe; use it directly |
| Probe on a serial port (ESP32-C3, classic ESP32 and others) | `xrdap share usbip`, then attach over USB/IP, see below |
| Wi-Fi | The probe serves USB/IP itself; attach directly, see [XRDAP-ESP](https://github.com/Jiu-xiao/XRDAP-ESP/blob/main/README.en.md#23-over-wi-fi-with-keil-pyocd-probe-rs-openocd) |

Share a probe on a serial port as a USB probe:

```
xrdap share usbip -p COM3
```

Attach in another terminal (on Linux run `sudo modprobe vhci-hcd` first and prefix the command with `sudo`; on Windows install [usbip-win2](https://github.com/vadimgrn/usbip-win2) first):

```
usbip attach -r 127.0.0.1 -b 1-1
```

A `<product> CMSIS-DAP` device appears, and debug tools use it as a USB probe, for example:

```
openocd -f interface/cmsis-dap.cfg -f target/stm32f4x.cfg
```

Ctrl-C in the `xrdap` terminal stops sharing; to the debug tool the probe is unplugged. In the GUI, checking 共享为 USB DAP (share as USB DAP) after connecting does the same, and unchecking it returns the probe to the GUI. To serve another computer on the LAN add `--listen 0.0.0.0:3240`; that computer runs `usbip attach -r <this host's IP> -b 1-1`.

### OpenOCD with a probe on a serial port (without USB/IP)

```
xrdap share openocd -p COM3
xrdap share openocd -p COM3 --listen 0.0.0.0:3241
```

`xrdap` serves OpenOCD's CMSIS-DAP TCP backend on 127.0.0.1:3241 (or the `--listen` address) and prints the OpenOCD configuration:

```
openocd -c "adapter driver cmsis-dap; cmsis-dap backend tcp; cmsis-dap tcp host 127.0.0.1; cmsis-dap tcp port 3241; transport select swd" -f target/stm32f4x.cfg
```

The TCP backend is only in OpenOCD master, see "OpenOCD version".

### Configuring the probe's Wi-Fi

```
xrdap wifi status
xrdap wifi set -p COM3 lab 12345678
xrdap wifi set -p COM3 lab
xrdap wifi set -p COM3 lab --password-stdin < password.txt
xrdap wifi clear -p COM3
```

`status` shows the saved access point, whether it is connected, the IP address and the signal strength, plus what last cleared the credentials (board button, serial console, 0x86 clear or NVS repair), the reset reason of this boot and whether NVS was rebuilt at this boot. `set` saves the SSID and password (omit the password for an open network) and `clear` removes them (the request carries a confirm byte). `--password-stdin` reads one line from standard input as the password, keeping it out of the shell history and the process list; over TCP (Wi-Fi) the password crosses the LAN in plaintext. After `set` or `clear` the probe restarts in about 0.5 s and the connection is lost. Once configured, `xrdap list` shows the probe's LAN address (`xrdap-xxxxxx.local`), and `-p xrdap-xxxxxx.local` connects over Wi-Fi.

In the GUI, after connecting, expand Wi-Fi at the bottom of the connection area: it shows the current state, and 保存并重启 (save and restart) or 清除 (clear) apply the SSID and password entered.

### Halting, resuming and resetting the target

```
xrdap info
xrdap info --no-target
xrdap halt
xrdap run
xrdap reset
xrdap reset --halt --clock 1M
```

`info` shows the probe information (DAP_Info, vendor extension INFO) and connects the target to read DPIDR and the core type; `--no-target` queries the probe only. `reset --halt` stops before the first instruction after reset, and DEMCR is restored when the reset ends. `--clock` sets the SWD clock (default 4M). The GUI buttons 停住/继续 (halt/resume), 复位 (reset) and 复位并停住 (reset and halt) do the same and also work while sampling.

## Reference

### Commands and options

Each command accepts only the options it uses, written after the command; `xrdap <command> -h` shows all parameters.

| Command | Purpose | Options |
|---|---|---|
| (no command) | GUI | |
| `list` | List probes | `--ask-serial` |
| `info` | Probe and target information | probe options, `--clock`, `--no-target` |
| `vars [text]` | List global variables in the ELF | `--elf` (required) |
| `read var...` | Read variables | probe options, `--clock`, `--elf` |
| `write var=value...` | Write variables and read back | probe options, `--clock`, `--elf` |
| `watch var[@div[:phase]]...` | Probe-side periodic sampling | probe options, `--clock`, `--elf`, `--period`, `--csv`, `--duration`, `--no-overwrite`, `--all` |
| `halt`, `run`, `reset` | Core control | probe options, `--clock`; `reset` also `--halt` |
| `wifi status`, `wifi set ssid [password]`, `wifi clear` | Probe Wi-Fi | probe options; `set` also `--password-stdin` |
| `share usbip`, `share openocd` | Share the probe with other debug tools | probe options, `--listen` |

Probe options: `-p` probe, `--baud` serial baud rate (default from the serial chip), `--timeout-ms` timeout for one response (default 2000).

### USB/IP client

| System | Install | Attach | Detach |
|---|---|---|---|
| Linux | Built into the kernel: `sudo modprobe vhci-hcd`; on Ubuntu the `usbip` command is in `linux-tools-generic` | `sudo usbip attach -r <address> -b 1-1` | `sudo usbip detach -p 0` |
| Windows | [usbip-win2](https://github.com/vadimgrn/usbip-win2) | `usbip attach -r <address> -b 1-1` | `usbip detach -p 0` |

One client is served at a time. After the debug tool exits the device stays attached until `usbip detach`; stopping the share unplugs it. When a client disconnects while the probe is sampling, `xrdap` stops sampling. When the probe fails (timeout, disconnect), `xrdap` disconnects the client and shows the reason.

### OpenOCD version

- Over USB or USB/IP, OpenOCD uses its USB backend; release 0.12.0 works.
- `cmsis-dap backend tcp` (`share openocd` and direct Wi-Fi) is not in 0.12.0 and needs a build from master. On Windows the build must include commit e89e73839d53; earlier master treats "no data yet" as an error on Windows, waits a dozen seconds at startup and repeats `error reading header`.

### Building from source

```
cargo build --release -p xrdap-tool
```

This produces `target/release/xrdap`. On Linux install `libgtk-3-dev libxkbcommon-dev libudev-dev` first.

| Directory | Content |
|---|---|
| `xrdap` | Rust library, see [xrdap/README.en.md](xrdap/README.en.md) |
| `xrdap-tool/src/cli` | Command line: option groups, commands |
| `xrdap-tool/src/gui` | GUI: worker thread, panels, plots |
| `xrdap-sim` | Probe simulator for tests, not shipped |

```
cargo test --workspace
```

The tests need no hardware. `xrdap-sim` simulates the probe and target from the CMSIS-DAP specification and the protocol document and serves the probe's TCP framing; the tests cover framing, vendor commands, sampling, DWARF parsing, every command and sharing. CI also checks `cargo fmt` and `cargo clippy`.

## License

Apache-2.0, see [LICENSE](LICENSE).
