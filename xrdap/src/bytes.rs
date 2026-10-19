//! 小端整数读取。调用方先检查过长度，越界按切片索引的规则 panic。

pub(crate) fn le_u16(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

pub(crate) fn le_u32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// 6 字节（探针的微秒时间戳）
pub(crate) fn le_u48(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b[..6].copy_from_slice(&d[at..at + 6]);
    u64::from_le_bytes(b)
}

pub(crate) fn le_u64(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[at..at + 8]);
    u64::from_le_bytes(b)
}
