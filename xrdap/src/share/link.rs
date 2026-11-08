//! 共享服务的客户端连接：读操作可被另一个线程的停止标志打断。
//!
//! Windows 上从别的线程 shutdown 套接字叫不醒阻塞中的 recv，所以读超时设为 [`STOP_POLL`]，
//! 每次超时醒来看一眼停止标志。
//!
//! Client connection of a share: reads can be interrupted by a stop flag set from another
//! thread.
//!
//! On Windows, shutting a socket down from another thread does not wake a blocked receive, so
//! the read timeout is set to [`STOP_POLL`] and each timeout wakes up to check the stop flag.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 停止标志多久看一次
const STOP_POLL: Duration = Duration::from_millis(100);

pub(crate) struct Link<'a> {
    stream: TcpStream,
    stop: &'a AtomicBool,
}

impl<'a> Link<'a> {
    pub fn new(stream: TcpStream, stop: &'a AtomicBool) -> io::Result<Self> {
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(STOP_POLL))?;
        Ok(Self { stream, stop })
    }

    /// 停止标志已置位。
    ///
    /// The stop flag is set.
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// 读满 `buf`。超时醒来时已读到的数据保留，接着读，不会把一帧拆坏。停止时返回
    /// `Interrupted`，对方关闭时返回 `UnexpectedEof`。
    ///
    /// Fill `buf`. Data already read is kept when a timeout wakes the read, and reading
    /// continues, so a frame is not split. Returns `Interrupted` on stop and `UnexpectedEof`
    /// when the peer closes.
    pub fn read_full(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut got = 0;
        while got < buf.len() {
            match self.stream.read(&mut buf[got..]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => got += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    if self.stopped() {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.stream.write_all(data)
    }
}

/// 对方发来的数据格式不对
pub(crate) fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// 正常结束：对方断开或被停止
pub(crate) fn is_normal_end(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::Interrupted
    )
}
