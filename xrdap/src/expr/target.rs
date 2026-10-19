//! 解析好的变量：厂商条目、访问位置，以及按类型读写目标内存。

use crate::dap::Probe;
use crate::types::ValueType;
use crate::vendor::{self, Item};
use crate::{Error, Result};

/// 解析好的一个变量：可以直接变成厂商条目。
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// 表达式的写法，输出时用作名字
    pub name: String,
    pub ap: u8,
    pub base: u32,
    pub derefs: Vec<i16>,
    pub final_off: i16,
    pub ty: ValueType,
    /// 各级指针的表达式，第 k 个对应第 k 级解引用，用于错误提示
    pub chain: Vec<String>,
}

impl Target {
    pub fn item(&self, id: u16) -> Item {
        Item {
            ap: self.ap,
            id,
            size: self.ty.size,
            base: self.base,
            derefs: self.derefs.clone(),
            final_off: self.final_off,
        }
    }

    /// 不经过指针时的地址。
    pub fn direct_addr(&self) -> Option<u32> {
        self.derefs.is_empty().then_some(self.base)
    }

    /// 位置的简短说明：`0x20000010`；经过指针时 `[x]` 表示读 x 处的指针，
    /// 例如 `[[0x20000034]+4]+8`。
    pub fn location(&self) -> String {
        let off = |d: i64| match d {
            0 => String::new(),
            d if d > 0 => format!("+{d}"),
            d => format!("{d}"),
        };
        let mut s = format!("0x{:08X}", self.base);
        for &d in &self.derefs {
            s = format!("[{s}{}]", off(d as i64));
        }
        s + &off(self.final_off as i64)
    }

    /// 读这个变量的当前值（`ty.size` 字节，小端）。探针要先连上目标（[`Probe::connect_swd`]）。
    pub fn read(&self, probe: &mut Probe) -> Result<Vec<u8>> {
        let r = probe.read(&[self.item(0)])?;
        match r.into_iter().next() {
            Some(v) if v.status == vendor::STATUS_OK => Ok(v.value),
            Some(v) => Err(self.item_error(v.status)),
            None => Err(Error::protocol("READ 没有返回结果")),
        }
    }

    /// 读出并按类型格式化，例如 `1.5`、`42 (0x2A)`、`RUN (1)`。
    pub fn read_text(&self, probe: &mut Probe) -> Result<String> {
        Ok(self.ty.format(&self.read(probe)?))
    }

    /// 写入 `ty.size` 字节的小端值（可以用 `ty.parse("1.5")` 从文字得到）。
    pub fn write(&self, probe: &mut Probe, value: &[u8]) -> Result<()> {
        let r = probe.write(&[(self.item(0), value.to_vec())])?;
        match r.into_iter().next() {
            Some(v) if v.status == vendor::STATUS_OK => Ok(()),
            Some(v) => Err(self.item_error(v.status)),
            None => Err(Error::protocol("WRITE 没有返回结果")),
        }
    }

    fn item_error(&self, status: u8) -> Error {
        Error::Io(format!(
            "{}：{}",
            self.name,
            vendor::item_status_text(status, &self.chain)
        ))
    }
}
