# Command line

[中文](cli.md)

This page is the reference of the host tool `xrdap`: commands and options, how probes and variables are written, the sampling output and counters, Wi-Fi configuration, and the clients and OpenOCD versions used when the probe is shared with other debug tools. The GUI uses the same probe and variable syntax and the same CSV format.

## Commands and options

Each command accepts only the options it uses, written after the command; `xrdap <command> -h` shows all parameters of that command.

| Command | Purpose | Options |
|---|---|---|
| (no command) | Opens the GUI | |
| `list` | Lists probes | `--ask-serial` |
| `info` | Probe information; connects the target to read DPIDR and the core type | probe options, `--clock`, `--no-target` |
| `vars [text]` | Lists the global variables in the ELF whose names contain the text | `--elf` (required) |
| `read var...` | Reads variables | probe options, `--clock`, `--elf` |
| `write var=value...` | Writes variables and reads them back | probe options, `--clock`, `--elf` |
| `watch var[@div[:phase]]...` | Periodic sampling on the probe | probe options, `--clock`, `--elf`, `--period`, `--csv`, `--duration`, `--no-overwrite`, `--all` |
| `halt`, `run`, `reset` | Halts, resumes, resets the target core | probe options, `--clock`; `reset` also `--halt` |
| `wifi status`, `wifi set ssid [password]`, `wifi clear` | The probe's Wi-Fi | probe options; `set` also `--password-stdin` |
| `share usbip`, `share openocd` | Shares the probe with other debug tools | probe options, `--listen` |

The probe options are `-p`, `--baud` and `--timeout-ms`.

| Option | Meaning | Default |
|---|---|---|
| `-p`, `--probe` | The probe, written as in the next section | The only probe |
| `--baud` | Serial baud rate, the same as the probe firmware's | Chosen by serial chip, see the next section |
| `--timeout-ms` | Timeout for one response, in milliseconds | 2000 |
| `--clock` | SWD clock, e.g. `4000000`, `4M`, `500k` | `4M` |
| `--elf` | The target program's ELF (built with `-g`), to look up variables and types by name | Raw addresses only |
| `--no-target` | `info` queries the probe only | Connects the target |
| `--halt` | `reset` stops at the reset vector, before the first instruction | Runs after reset |
| `--period` | Sampling period, e.g. `1000` (µs), `500us`, `2ms`, `2.5ms`, `1s` | `1ms` |
| `--csv` | Writes every frame to this CSV file | Prints to the terminal |
| `--duration` | Sampling time in seconds | Samples until Ctrl-C |
| `--no-overwrite` | Drops new frames when the buffer is full | Overwrites the oldest frame |
| `--all` | Prints every frame in the terminal | At most one line per 0.1 s |
| `--password-stdin` | Reads one line from standard input as the password | |
| `--listen` | Listening address of the share | `127.0.0.1:3240` (usbip), `127.0.0.1:3241` (openocd) |
| `--ask-serial` | `list` opens each serial port and sends DAP_Info | Lists the ports only |

`watch` and `share` run until Ctrl-C. The first Ctrl-C ends them normally: `watch` stops sampling and reads the frames left in the buffer, `share` disconnects the client; a second Ctrl-C exits at once.

## Probes

### `-p` forms

| `-p` | Meaning |
|---|---|
| omitted | The only probe |
| `usb`, `usb:<serial>` | USB |
| `COM3`, `/dev/ttyUSB0`, `serial:<port>` | Serial port |
| `192.0.2.10`, `192.0.2.10:3241`, `xrdap-aabbcc.local`, `tcp:<host>` | TCP: the probe's Wi-Fi, or `xrdap share openocd`; the port defaults to 3241 |
| `XRobot-XRDAP-AABBCCDDEEFF`, `DDEEFF` | Full serial number or its last digits, case-insensitive |

Without `-p`, and when looking up a serial number, `xrdap` searches USB, the LAN (mDNS service `_xrdap._tcp`) and the USB port built into Espressif chips (USB Serial/JTAG); a probe behind a USB-serial chip is selected by its port name. When one probe is found over several connections, USB is preferred, then serial, then TCP. `xrdap list` lists every connection with its `-p` form.

### Serial ports

The serial baud rate must match the probe firmware. Without `--baud` it follows the serial chip, tried in this order:

| Serial chip | Baud rate |
|---|---|
| WCH CH343 and similar, FTDI, other chips | 4000000, then 921600 |
| WCH CH340, CH341, Silicon Labs CP210x | 921600 (the classic ESP32 firmware) |

The USB port built into Espressif chips does not depend on the baud rate.

`xrdap` opens serial ports with DTR and RTS both deasserted, as Espressif's idf_monitor does: the auto-download circuit of ESP32 boards drives EN and GPIO0 from these lines. `xrdap list --ask-serial` opens each serial port and sends DAP_Info, and opening a port may reset a development board connected to it.

### One connection at a time

One connection (USB, serial or TCP) uses the probe at a time, and keeping to that is up to the user: the probe executes the commands of every connection, and two connections in use at once disturb each other's SWD state. While another connection is sampling, a command that connects the target gets `FF` and `xrdap` reports that the probe is sampling; when the connection that started sampling connects the target again, the probe ends that sampling. The rule in detail is in section 3.1 of XRDAP-Core's [`docs/protocol.en.md`](https://github.com/Jiu-xiao/XRDAP-Core/blob/main/docs/protocol.en.md).

## Variable syntax

Variables are written as `counter`, `state.speed`, `motor[1].pid->out`, `*ptr`, `p->q->x`, with type casts such as `(uint32_t***)p`, or as a raw address with a type, e.g. `0x20000010:u32`. Types are `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64`, `f32`, `f64`, `bool`, common C type names (`uint32_t`, `int`, `float` and others) and type names from the ELF, optionally followed by `*`. Variable names and the ELF's types come from the ELF given with `--elf`; without `--elf` only raw addresses are available.

`read` and `watch` expand structs and arrays into their members; each item of `write` targets one numeric variable. Values: integers in decimal, `0x` hexadecimal or `0b` binary, floating-point numbers as decimals, enumerations by name, and `bool` as `true`, `false`, `1` or `0`. After writing, `xrdap` reads the variable back and shows the value read.

## Sampling

### Divider and phase

The probe has one tick per period. A `watch` variable written as `var@div` is sampled every `div` ticks, and `var@div:phase` also sets the phase: the variable is sampled on the ticks where `(tick + phase) % div == 0`. The divider is 1 to 65535 and the phase is smaller than the divider; `xrdap` staggers the phases of divided variables without a phase. The number of variables in one sampling is limited by the probe; `xrdap info` shows the limit (每次最多 N 个条目, at most N items).

### Output

Without `--csv`, `watch` prints at most one line per 0.1 s: the probe time (seconds from the first frame), the tick number and the latest value of each variable; `--all` prints every frame. With `--csv`, every frame goes to the file, and the terminal shows the frames sampled, skipped ticks and dropped frames, updated once a second. At the end `watch` prints the frame count, the elapsed time and the counters below.

### CSV

`watch --csv` and 录制 CSV… (record CSV) in the GUI write the same format. The first line is the header `host_time,t,sample_tick,<variables...>`, then one line per frame:

| Column | Contents |
|---|---|
| `host_time` | Unix time converted to the host clock, in seconds |
| `t` | Probe time from the first frame of this sampling, in seconds |
| `sample_tick` | The probe's tick number |
| One column per variable | The value sampled on this tick; empty when the variable was not due, `ERR` when the read failed |

`host_time` is aligned from the host time before and after each SUB_READ and the probe time in its response; its error is half the round trip, which `watch` prints at the end.

### Counters

Whether sampling keeps its period is judged from run-time counters. `watch` prints them at the end, and the statistics line of the GUI shows them while sampling:

| Counter | Command line | GUI | Meaning |
|---|---|---|---|
| Skipped ticks | 探针跳过节拍 | 跳拍 | Ticks the probe had no time to sample and skipped |
| Dropped frames | 缓冲丢帧 | 探针丢帧 | Frames overwritten or dropped when the buffer was full |
| Buffer fill | | 缓冲 x/y 字节 | Bytes in the probe's buffer not yet read |
| Frame number gaps | 帧序号缺口 | 缺帧 | Gaps the host finds in the tick numbers: ticks with a due variable whose frame was not received; skipped ticks and dropped frames both cause them |
| Failed values | 读取失败的值 | 失败值 | Values the probe failed to read from the target, `ERR` in the CSV |

When skipped ticks grow, SWD cannot read one tick's variables in time, and `xrdap` advises a longer period, a divider on slow variables or fewer variables. When dropped frames grow or the buffer is more than three quarters full, the host reads too slowly or the link lacks bandwidth, and `xrdap` advises dividers or fewer variables. The command line gives this advice during sampling and at the end; the GUI shows it below the statistics line. Writing variables and reading the core state while sampling share SWD with the sampling and add skipped ticks as well.

## Wi-Fi

Wi-Fi is a feature of XRDAP-ESP: the probe joins an existing hotspot as a Wi-Fi client. `xrdap wifi` queries and changes the credentials over the probe's current connection.

```
xrdap wifi status -p COM3
xrdap wifi set -p COM3 SSID --password-stdin < password.txt
```

`status` shows the saved SSID, whether the probe is connected, its IP address and the signal strength (dBm), plus what last cleared the credentials (board button, serial console `wifi clear`, 0x86 clear command or NVS repair), the reset reason of this boot (power-on, software restart, watchdog, crash, brownout or other), and whether NVS was damaged and rebuilt at this boot.

`set` saves the SSID (1 to 32 bytes) and the password (8 to 63 bytes), e.g. `xrdap wifi set -p COM3 SSID PASSWORD`; the password is omitted for an open network. `--password-stdin` reads one line from standard input as the password, removes the line ending and keeps everything else, so the password stays out of the shell history and the process list; without a redirection, the password is typed after the command is entered and ended with Enter. PowerShell has no `<` redirection; there the command is `Get-Content password.txt | xrdap wifi set -p COM3 SSID --password-stdin`. `--password-stdin` and a password argument cannot be used together; over TCP (Wi-Fi) the password crosses the LAN in plaintext. `xrdap wifi clear -p COM3` clears the credentials; the request carries a confirm byte. After a successful `set` or `clear` the probe restarts about 0.5 s after its reply, and the connection is lost.

Once configured, `xrdap list` shows the probe's LAN address `xrdap-xxxxxx.local`, and `-p xrdap-xxxxxx.local` connects over Wi-Fi.

In the GUI, after connecting, Wi-Fi at the bottom of the connection area shows the current state; with 热点 (SSID) and 密码 (password) filled in, 保存并重启 (save and restart) saves them; 清除 (clear) clears the credentials and 刷新 (refresh) reads the state again. After 保存并重启 the GUI empties the password field.

## Use with other debug tools

### USB/IP

`xrdap share usbip` serves a USB/IP device on 127.0.0.1:3240 and prints the attach command. After a client attaches it, a USB CMSIS-DAP v2 probe with the product name `<probe product> CMSIS-DAP` appears, and Keil, pyOCD, probe-rs and OpenOCD use it as a USB probe; on Windows, probe-rs 0.31 does not see devices attached by usbip-win2.

| System | Install | Attach | Detach |
|---|---|---|---|
| Linux | Built into the kernel: `sudo modprobe vhci-hcd` first; on Ubuntu the `usbip` command is in `linux-tools-generic` | `sudo usbip attach -r <address> -b 1-1` | `sudo usbip detach -p 0` |
| Windows | [usbip-win2](https://github.com/vadimgrn/usbip-win2) | `usbip attach -r <address> -b 1-1` | `usbip detach -p 0` |

For example, on Windows a probe on a serial port is shared, then attached and used by OpenOCD in another terminal:

```
xrdap share usbip -p COM3
```

```
usbip attach -r 127.0.0.1 -b 1-1
openocd -f interface/cmsis-dap.cfg -f target/stm32f4x.cfg
```

One client is served at a time. After the debug tool exits the device stays attached until `usbip detach`; stopping the share (Ctrl-C, or unchecking 共享为 USB DAP in the GUI) unplugs it. When a client disconnects while the probe is sampling, `xrdap` stops sampling. When the probe fails (timeout, disconnect), `xrdap` disconnects that client, shows the reason and waits for the next client.

To serve another computer on the LAN, add `--listen 0.0.0.0:3240`; that computer runs `usbip attach -r <this computer's IP> -b 1-1`. With a port other than 3240 the attach command printed by `xrdap` carries `--tcp-port`.

共享为 USB DAP (share as USB DAP) in the GUI shares the connected probe on 127.0.0.1:3240. While sharing, the client uses the probe; unchecking it returns the probe to the GUI for reading, writing and sampling.

### OpenOCD's TCP backend

`xrdap share openocd` serves OpenOCD's CMSIS-DAP TCP backend on 127.0.0.1:3241 (or the `--listen` address) and prints the OpenOCD configuration. This command is usually for probes on a serial port; a Wi-Fi probe serves the same backend on port 3241 itself, and OpenOCD connects to the probe's address (`cmsis-dap tcp host <probe IP>`). To serve other computers on the LAN, add `--listen 0.0.0.0:3241`. The 127.0.0.1 printed by `xrdap` is the address of the local share:

```
xrdap share openocd -p COM3
```

OpenOCD is started in another terminal:

```
openocd -c "adapter driver cmsis-dap; cmsis-dap backend tcp; cmsis-dap tcp host 127.0.0.1; cmsis-dap tcp port 3241; transport select swd" -f target/stm32f4x.cfg
```

### OpenOCD versions

Over USB or USB/IP, OpenOCD uses its USB backend, and release 0.12.0 works. `cmsis-dap backend tcp` (`xrdap share openocd` and direct connections to Wi-Fi probes) is not in 0.12.0 and needs OpenOCD built from master. On Windows the build must include commit e89e73839d53: a build without it treats "no data yet" as an error on Windows, waits a dozen seconds at startup and repeats `error reading header`, and reads and writes may fail. Without such a build, the probe is shared with `xrdap share usbip`, attached, and reached through the USB backend.
