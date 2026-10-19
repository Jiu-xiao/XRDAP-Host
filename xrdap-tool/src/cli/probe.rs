//! 探针本身的命令：list、info、wifi，以及内核的停住、继续、复位。

use std::io::Write;

use xrdap::cortex::{self, AfterReset};
use xrdap::dap::{self, Probe};
use xrdap::discover;
use xrdap::wifi;

use super::connect::{connect_target, ConnOpts};
use super::{CliResult, CoreOp, WifiCmd};

pub fn cmd_list(out: &mut dyn Write) -> CliResult {
    let probes = discover::group(discover::find(&discover::Options::default()));
    if probes.is_empty() {
        writeln!(out, "没有找到探针（USB、串口、局域网都没有）")?;
        return Ok(());
    }
    write!(out, "{}", discover::table(&probes))?;
    Ok(())
}

pub fn cmd_info(
    probe: &mut Probe,
    conn: &ConnOpts,
    target: bool,
    out: &mut dyn Write,
) -> CliResult {
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
    if target {
        match probe.connect_swd(conn.clock) {
            Ok(t) => {
                writeln!(out, "目标：{}，SWD 时钟 {} Hz", t.describe(), conn.clock)?;
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
        WifiCmd::Status => writeln!(out, "{}", wifi::status(probe)?.describe())?,
        WifiCmd::Set { ssid, password } => {
            wifi::set(probe, ssid, password.as_deref().unwrap_or(""))?;
            writeln!(out, "已保存，探针重启后连接热点 {ssid}")?;
        }
        WifiCmd::Clear => {
            wifi::clear(probe)?;
            writeln!(out, "已清除，探针重启后不再连 Wi-Fi")?;
        }
    }
    Ok(())
}

/// 停住、继续或复位内核，打印之后的状态。
pub fn cmd_core(probe: &mut Probe, conn: &ConnOpts, op: CoreOp, out: &mut dyn Write) -> CliResult {
    connect_target(probe, conn)?;
    let s = match op {
        CoreOp::Halt => cortex::halt(probe)?,
        CoreOp::Run => cortex::resume(probe)?,
        CoreOp::Reset { halt: true } => cortex::reset(probe, AfterReset::Halt)?,
        CoreOp::Reset { halt: false } => cortex::reset(probe, AfterReset::Run)?,
    };
    writeln!(out, "内核：{}（DHCSR 0x{:08X}）", s.state.text(), s.dhcsr)?;
    Ok(())
}
