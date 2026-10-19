//! Cortex-M 内核的状态和运行控制：读 CPUID、DHCSR，停住、继续、复位。
//!
//! 都通过厂商 READ/WRITE 访问系统控制空间，不用 DAP_Transfer，所以采样期间也能用。
//! 寄存器定义见 ARMv7-M / ARMv8-M 架构手册的 C1.6（Debug system registers）。

use std::time::{Duration, Instant};

use crate::dap::Probe;
use crate::vendor::{self, Item};
use crate::{Error, Result};

pub const CPUID: u32 = 0xE000_ED00;
pub const AIRCR: u32 = 0xE000_ED0C;
pub const DHCSR: u32 = 0xE000_EDF0;
pub const DEMCR: u32 = 0xE000_EDFC;

const DBGKEY: u32 = 0xA05F_0000;
const C_DEBUGEN: u32 = 1 << 0;
const C_HALT: u32 = 1 << 1;
const S_HALT: u32 = 1 << 17;
const S_SLEEP: u32 = 1 << 18;
const S_LOCKUP: u32 = 1 << 19;
const S_RESET_ST: u32 = 1 << 25;
const VECTKEY: u32 = 0x05FA_0000;
const SYSRESETREQ: u32 = 1 << 2;
const VC_CORERESET: u32 = 1 << 0;

/// 内核此刻的运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreState {
    Running,
    /// 执行了 WFI/WFE，在等中断
    Sleeping,
    Halted,
    /// 锁死：HardFault 里又出错
    Lockup,
}

impl CoreState {
    pub fn text(self) -> &'static str {
        match self {
            CoreState::Running => "运行中",
            CoreState::Sleeping => "睡眠（WFI/WFE）",
            CoreState::Halted => "已停住",
            CoreState::Lockup => "锁死（LOCKUP）",
        }
    }
}

/// 读一次 DHCSR 得到的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreStatus {
    pub dhcsr: u32,
    pub state: CoreState,
    /// 调试器打开了停机调试（C_DEBUGEN）
    pub debug_enabled: bool,
    /// 上次读 DHCSR 之后内核复位过（S_RESET_ST，读后清零）
    pub reset_seen: bool,
}

impl CoreStatus {
    pub fn from_dhcsr(dhcsr: u32) -> Self {
        let state = if dhcsr & S_LOCKUP != 0 {
            CoreState::Lockup
        } else if dhcsr & S_HALT != 0 {
            CoreState::Halted
        } else if dhcsr & S_SLEEP != 0 {
            CoreState::Sleeping
        } else {
            CoreState::Running
        };
        Self {
            dhcsr,
            state,
            debug_enabled: dhcsr & C_DEBUGEN != 0,
            reset_seen: dhcsr & S_RESET_ST != 0,
        }
    }
}

/// CPUID 寄存器：内核型号和修订号。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreInfo {
    pub cpuid: u32,
}

impl CoreInfo {
    /// 内核名字，例如 Cortex-M4；不认识的部件号返回 None。
    pub fn name(&self) -> Option<&'static str> {
        if self.cpuid >> 24 != 0x41 {
            return None;
        }
        Some(match (self.cpuid >> 4) & 0xFFF {
            0xC20 => "Cortex-M0",
            0xC60 => "Cortex-M0+",
            0xC21 => "Cortex-M1",
            0xC23 => "Cortex-M3",
            0xC24 => "Cortex-M4",
            0xC27 => "Cortex-M7",
            0xD20 => "Cortex-M23",
            0xD21 => "Cortex-M33",
            0xD22 => "Cortex-M55",
            0xD23 => "Cortex-M85",
            0xD24 => "Cortex-M52",
            0xD31 => "Cortex-M35P",
            _ => return None,
        })
    }

    /// 例如 "Cortex-M4 r0p1"；不认识时给出 CPUID 原值。
    pub fn describe(&self) -> String {
        let variant = (self.cpuid >> 20) & 0xF;
        let revision = self.cpuid & 0xF;
        match self.name() {
            Some(n) => format!("{n} r{variant}p{revision}"),
            None => format!("CPUID 0x{:08X}", self.cpuid),
        }
    }
}

/// 复位之后内核做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterReset {
    Run,
    /// 停在复位向量（第一条指令之前）
    Halt,
}

/// 读一个 32 位字。
pub fn read_u32(probe: &mut Probe, addr: u32) -> Result<u32> {
    let r = probe.read(&[Item::direct(0, 4, addr)])?;
    match r.first() {
        Some(r) if r.status == 0 && r.value.len() == 4 => {
            Ok(u32::from_le_bytes(r.value[..4].try_into().unwrap()))
        }
        Some(r) => Err(Error::Io(format!(
            "读 0x{addr:08X} 失败：{}",
            vendor::item_status_text(r.status, &[])
        ))),
        None => Err(Error::protocol("READ 没有返回结果")),
    }
}

/// 写一个 32 位字。
pub fn write_u32(probe: &mut Probe, addr: u32, value: u32) -> Result<()> {
    let r = probe.write(&[(Item::direct(0, 4, addr), value.to_le_bytes().to_vec())])?;
    match r.first() {
        Some(r) if r.status == 0 => Ok(()),
        Some(r) => Err(Error::Io(format!(
            "写 0x{addr:08X} 失败：{}",
            vendor::item_status_text(r.status, &[])
        ))),
        None => Err(Error::protocol("WRITE 没有返回结果")),
    }
}

pub fn info(probe: &mut Probe) -> Result<CoreInfo> {
    Ok(CoreInfo {
        cpuid: read_u32(probe, CPUID)?,
    })
}

pub fn status(probe: &mut Probe) -> Result<CoreStatus> {
    Ok(CoreStatus::from_dhcsr(read_u32(probe, DHCSR)?))
}

/// 等 DHCSR 满足条件，最多 timeout；超时返回最后一次的状态。
fn wait_for(
    probe: &mut Probe,
    timeout: Duration,
    done: impl Fn(&CoreStatus) -> bool,
) -> Result<CoreStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        let s = status(probe)?;
        if done(&s) || Instant::now() >= deadline {
            return Ok(s);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// 停住内核。返回停住后的状态；100 ms 内没停住时返回错误。
pub fn halt(probe: &mut Probe) -> Result<CoreStatus> {
    write_u32(probe, DHCSR, DBGKEY | C_DEBUGEN | C_HALT)?;
    let s = wait_for(probe, Duration::from_millis(100), |s| {
        s.state == CoreState::Halted
    })?;
    if s.state != CoreState::Halted {
        return Err(Error::Io(format!(
            "内核没有停住（DHCSR 0x{:08X}，{}）",
            s.dhcsr,
            s.state.text()
        )));
    }
    Ok(s)
}

/// 让停住的内核继续运行，保持 C_DEBUGEN（和 OpenOCD 的 resume 一样）。
pub fn resume(probe: &mut Probe) -> Result<CoreStatus> {
    write_u32(probe, DHCSR, DBGKEY | C_DEBUGEN)?;
    wait_for(probe, Duration::from_millis(100), |s| {
        s.state != CoreState::Halted
    })
}

/// 系统复位（AIRCR.SYSRESETREQ），复位后运行或停在复位向量。
///
/// 写 AIRCR 时目标正在复位，这次写入和紧接着的几次读可能收不到正常应答，
/// 所以不检查写入结果，之后在 500 ms 内反复读 DHCSR，直到看到 S_RESET_ST。
pub fn reset(probe: &mut Probe, after: AfterReset) -> Result<CoreStatus> {
    let demcr = read_u32(probe, DEMCR)?;
    match after {
        AfterReset::Halt => {
            write_u32(probe, DHCSR, DBGKEY | C_DEBUGEN)?;
            write_u32(probe, DEMCR, demcr | VC_CORERESET)?;
        }
        AfterReset::Run => write_u32(probe, DEMCR, demcr & !VC_CORERESET)?,
    }
    // 先读一次，清掉旧的 S_RESET_ST
    status(probe)?;
    let _ = write_u32(probe, AIRCR, VECTKEY | SYSRESETREQ);

    let deadline = Instant::now() + Duration::from_millis(500);
    let mut seen = false;
    let mut last = None;
    loop {
        match status(probe) {
            Ok(s) => {
                seen |= s.reset_seen;
                last = Some(s);
                let done = seen && (after == AfterReset::Run || s.state == CoreState::Halted);
                if done {
                    break;
                }
            }
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if after == AfterReset::Halt {
        // 只拦这一次复位：恢复 DEMCR 原来的 VC_CORERESET
        write_u32(probe, DEMCR, demcr)?;
    }
    let s = last.ok_or_else(|| Error::Io("复位后读不到 DHCSR".into()))?;
    if !seen {
        return Err(Error::Io(format!(
            "没有看到复位发生（DHCSR 0x{:08X}）",
            s.dhcsr
        )));
    }
    if after == AfterReset::Halt && s.state != CoreState::Halted {
        return Err(Error::Io(format!(
            "复位后内核没有停住（DHCSR 0x{:08X}）",
            s.dhcsr
        )));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_dhcsr() {
        let s = CoreStatus::from_dhcsr(0x0003_0003);
        assert_eq!(s.state, CoreState::Halted);
        assert!(s.debug_enabled);
        assert_eq!(CoreStatus::from_dhcsr(0x0001_0000).state, CoreState::Running);
        assert_eq!(CoreStatus::from_dhcsr(0x0005_0000).state, CoreState::Sleeping);
        assert_eq!(CoreStatus::from_dhcsr(0x000B_0001).state, CoreState::Lockup);
        assert!(CoreStatus::from_dhcsr(0x0201_0000).reset_seen);
    }

    #[test]
    fn decode_cpuid() {
        assert_eq!(CoreInfo { cpuid: 0x410F_C241 }.describe(), "Cortex-M4 r0p1");
        assert_eq!(CoreInfo { cpuid: 0x411F_C271 }.describe(), "Cortex-M7 r1p1");
        assert_eq!(CoreInfo { cpuid: 0x1234_5678 }.describe(), "CPUID 0x12345678");
    }
}
