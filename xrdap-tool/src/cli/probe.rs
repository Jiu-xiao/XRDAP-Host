//! 探针本身的命令：list、info、wifi，以及内核的停住、继续、复位。

use std::io::Write;

use xrdap::addr::Addr;
use xrdap::cortex::{self, AfterReset};
use xrdap::dap::{self, Probe};
use xrdap::discover::{self, ProbeEntry};

use super::{CliResult, CoreOp, WifiCmd};

/// 列出探针。`ask_serial` 为 false 时不打开任何串口，只列出可能接着探针的串口。
pub fn cmd_list(ask_serial: bool, out: &mut dyn Write) -> CliResult {
    let probes = discover::group(discover::find(&discover::Options {
        ask_serial,
        ..Default::default()
    }));
    if probes.is_empty() {
        writeln!(out, "没有找到探针")?;
    }
    write!(out, "{}", table(&probes))?;
    let listed: Vec<&Addr> = probes
        .iter()
        .flat_map(|p| &p.links)
        .map(|l| &l.addr)
        .collect();
    let unchecked: Vec<_> = discover::serial_ports()
        .into_iter()
        .filter(|s| {
            !listed.contains(&&Addr::Serial {
                port: s.port.clone(),
            })
        })
        .collect();
    if !unchecked.is_empty() {
        if ask_serial {
            writeln!(out, "没有应答的串口：")?;
        } else {
            writeln!(
                out,
                "没有打开检查的串口（用 -p 串口名 连接，或 xrdap list --ask-serial 逐个打开询问）："
            )?;
        }
        for s in unchecked {
            writeln!(out, "    {}  {}", s.port, s.chip.name())?;
        }
    }
    Ok(())
}

/// 每块探针一行序列号和产品，下面每行一个连接和选它时 -p 的写法。
fn table(probes: &[ProbeEntry]) -> String {
    let mut s = String::new();
    for p in probes {
        s.push_str(&format!(
            "{}  {}
",
            p.serial.as_deref().unwrap_or("（没有序列号）"),
            p.product.as_deref().unwrap_or("")
        ));
        for l in &p.links {
            let note = l
                .note
                .as_deref()
                .map(|n| format!("（{n}）"))
                .unwrap_or_default();
            s.push_str(&format!(
                "    {}{note}    -p {}
",
                l.addr,
                l.addr.spec()
            ));
        }
    }
    s
}

/// 探针信息；`clock` 不为 None 时再按这个 SWD 时钟连接目标。
pub fn cmd_info(probe: &mut Probe, clock: Option<u32>, out: &mut dyn Write) -> CliResult {
    writeln!(out, "连接：{}", probe.describe())?;
    let packet = probe.query_packet_size()?;
    let id = probe.identity()?;
    let vendor_name = id.vendor.unwrap_or_default();
    let product = id.product.unwrap_or_default();
    let serial = id.serial.unwrap_or_else(|| "?".into());
    let fw = id.firmware.unwrap_or_else(|| "?".into());
    let proto = id.protocol.unwrap_or_else(|| "?".into());
    let count = probe
        .dap_info(dap::ID_PACKET_COUNT)
        .ok()
        .and_then(|d| d.first().copied())
        .unwrap_or(1);
    writeln!(
        out,
        "探针：{vendor_name} {product}，序列号 {serial}，固件 {fw}"
    )?;
    writeln!(
        out,
        "CMSIS-DAP：{proto}，包长 {packet} 字节，最多 {count} 个包在途"
    )?;
    match probe.vendor_info() {
        Ok(info) => {
            writeln!(
                out,
                "厂商扩展：版本 {}，功能 {}（0x{:02X}）",
                info.version,
                info.feature_names().join("、"),
                info.features
            )?;
            writeln!(
                out,
                "  每次最多 {} 个条目，最多 {} 级指针，采样缓冲 {} 字节",
                info.max_items, info.max_depth, info.ring_bytes
            )?;
            let calib = if info.access_us == 0 {
                "单次 SWD 访问耗时还没有标定（每次 SUB_SET 时按当前时钟标定）".to_string()
            } else {
                format!(
                    "单次 SWD 访问耗时 {} µs（上次 SUB_SET 时标定）",
                    info.access_us
                )
            };
            writeln!(
                out,
                "  探针时间 {:.6} s，{calib}",
                info.probe_time_us as f64 / 1e6
            )?;
        }
        Err(e) => writeln!(out, "厂商扩展：不可用（{e}）")?,
    }
    if let Some(clock) = clock {
        match xrdap::watch::connect(probe, clock) {
            Ok(t) => {
                writeln!(out, "目标：{}，SWD 时钟 {clock} Hz", t.describe())?;
                let core = cortex::info(probe).map(|i| i.describe());
                let state = cortex::status(probe);
                match (core, state) {
                    (Ok(c), Ok(st)) => writeln!(
                        out,
                        "内核：{c}，{}（DHCSR 0x{:08X}）",
                        st.state.text(),
                        st.dhcsr
                    )?,
                    (Err(e), _) | (_, Err(e)) => writeln!(out, "内核：读不到（{e}）")?,
                }
            }
            Err(e) => writeln!(out, "目标：连接失败（{e}）")?,
        }
    }
    Ok(())
}

pub fn cmd_wifi(probe: &mut Probe, op: &WifiCmd, out: &mut dyn Write) -> CliResult {
    match op {
        WifiCmd::Status { .. } => writeln!(out, "{}", probe.wifi_status()?.describe())?,
        WifiCmd::Set { ssid, password, .. } => {
            probe.wifi_set(ssid, password.as_deref().unwrap_or(""))?;
            writeln!(out, "已保存，探针重启后连接热点 {ssid}")?;
        }
        WifiCmd::Clear { .. } => {
            probe.wifi_clear()?;
            writeln!(out, "已清除，探针重启后不再连 Wi-Fi")?;
        }
    }
    Ok(())
}

/// 停住、继续或复位已连上的目标内核，打印之后的状态。
pub fn cmd_core(probe: &mut Probe, op: CoreOp, out: &mut dyn Write) -> CliResult {
    let s = match op {
        CoreOp::Halt => cortex::halt(probe)?,
        CoreOp::Run => cortex::resume(probe)?,
        CoreOp::Reset { halt: true } => cortex::reset(probe, AfterReset::Halt)?,
        CoreOp::Reset { halt: false } => cortex::reset(probe, AfterReset::Run)?,
    };
    writeln!(out, "内核：{}（DHCSR 0x{:08X}）", s.state.text(), s.dhcsr)?;
    Ok(())
}
