//! 库里所有操作的错误。说明文字直接给用户看。
//!
//! Errors of every operation in the library. Messages are written to be shown to users directly.

use std::fmt;

/// 库里所有操作的错误。
///
/// Errors of every operation in the library.
#[derive(Debug)]
pub enum Error {
    /// 传输层读写失败（串口、USB、TCP）。
    ///
    /// The transport (serial, USB, TCP) failed.
    Io(String),
    /// 等待响应超时。
    ///
    /// No response in time.
    Timeout(String),
    /// 探针的响应格式不对。
    ///
    /// The probe's response is malformed.
    Protocol(String),
    /// 厂商命令返回了非 0 的整体状态；`text` 是这个命令对该状态的说明。
    ///
    /// A vendor command returned a non-zero overall status; `text` explains it for that command.
    Status {
        /// 命令字节。
        ///
        /// The command byte.
        cmd: u8,
        /// 整体状态。
        ///
        /// The overall status.
        status: u8,
        /// 该命令对这个状态的说明。
        ///
        /// The explanation of the status for this command.
        text: &'static str,
    },
    /// 读写一个条目失败：探针返回的条目状态不是 0（协议文档 3.2 节）。
    ///
    /// Reading or writing one item failed: its item status is not 0 (protocol document,
    /// section 3.2).
    Item {
        /// 条目状态。
        ///
        /// The item status.
        status: u8,
        /// 对该状态的说明。
        ///
        /// The explanation of the status.
        text: String,
    },
    /// 目标那边的问题：SWD 没有应答、内核没有按要求停住或复位。
    ///
    /// A problem on the target side: no SWD answer, or the core did not halt or reset.
    Target(String),
    /// 探针正在被占用：在采样，只接受 DAP_Info 和厂商命令（协议文档 3.1 节）。
    ///
    /// The probe is busy sampling and accepts only DAP_Info and vendor commands (protocol
    /// document, section 3.1).
    Busy(String),
    /// 表达式、符号或参数有问题。
    ///
    /// A bad expression, symbol or argument.
    Usage(String),
    /// 没能选定一块探针：一块都没找到，或者找到不止一块。
    ///
    /// No single probe could be chosen: none found, or more than one.
    Select(String),
}

/// 库里的结果类型。
///
/// The library's result type.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// 构造 [`Error::Usage`]。
    ///
    /// Construct an [`Error::Usage`].
    pub fn usage(msg: impl Into<String>) -> Self {
        Error::Usage(msg.into())
    }

    /// 构造 [`Error::Protocol`]。
    ///
    /// Construct an [`Error::Protocol`].
    pub fn protocol(msg: impl Into<String>) -> Self {
        Error::Protocol(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(msg)
            | Error::Timeout(msg)
            | Error::Protocol(msg)
            | Error::Target(msg)
            | Error::Busy(msg)
            | Error::Usage(msg)
            | Error::Select(msg)
            | Error::Item { text: msg, .. } => f.write_str(msg),
            Error::Status { cmd, status, text } => {
                write!(f, "命令 0x{cmd:02X} 返回状态 0x{status:02X}：{text}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::TimedOut {
            Error::Timeout(format!("等待响应超时：{e}"))
        } else {
            Error::Io(e.to_string())
        }
    }
}
