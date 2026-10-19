use std::fmt;

/// 库里所有操作的错误。说明文字直接给用户看。
#[derive(Debug)]
pub enum Error {
    /// 传输层读写失败（串口、USB、TCP）。
    Io(String),
    /// 等待响应超时。
    Timeout(String),
    /// 探针的响应格式不对。
    Protocol(String),
    /// 厂商命令返回了非 0 的整体状态。
    Status { cmd: u8, status: u8 },
    /// 表达式、符号或参数有问题。
    Usage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn usage(msg: impl Into<String>) -> Self {
        Error::Usage(msg.into())
    }

    pub fn protocol(msg: impl Into<String>) -> Self {
        Error::Protocol(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(msg) | Error::Timeout(msg) | Error::Protocol(msg) | Error::Usage(msg) => {
                f.write_str(msg)
            }
            Error::Status { cmd, status } => write!(
                f,
                "命令 0x{cmd:02X} 返回状态 0x{status:02X}：{}",
                crate::vendor::status_text(*status)
            ),
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
