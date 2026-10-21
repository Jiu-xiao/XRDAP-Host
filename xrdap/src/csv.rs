//! 采样记录的 CSV 格式，`xrdap watch --csv` 和图形界面的录制共用。
//!
//! 第一行是表头 `host_time,t,sample_tick,变量…`，之后每帧一行：`host_time` 是换算到电脑时钟的
//! Unix 秒，`t` 是相对第一帧的探针时间（秒），`sample_tick` 是探针的节拍序号；
//! 这一拍没有采的变量为空，读取失败为 `ERR`。
//!
//! The CSV format of sampling records, shared by `xrdap watch --csv` and the GUI recording.
//!
//! The first line is the header `host_time,t,sample_tick,variables...`; each frame then adds
//! one line. `host_time` is Unix seconds converted to the host clock, `t` is the probe time in
//! seconds relative to the first frame, and `sample_tick` is the probe's tick number. A
//! variable that was not sampled on this tick is empty; a failed read is `ERR`.

use crate::types::ValueType;
use crate::watch::HostFrame;

/// 需要时加引号：含逗号、引号或换行的字段。
///
/// Quote a field when needed: fields containing a comma, a quote or a newline.
pub(crate) fn field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// 表头行：`host_time,t,sample_tick` 加各变量名。
///
/// The header line: `host_time,t,sample_tick` followed by the variable names.
pub fn header<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut line = String::from("host_time,t,sample_tick");
    for n in names {
        line.push(',');
        line.push_str(&field(n));
    }
    line
}

/// 一个变量这一拍的格子内容：没采时为 None，读取失败为 `ERR`。
///
/// The cell of one variable on one tick: None when it was not sampled, `ERR` when the read
/// failed.
pub fn cell(ty: &ValueType, value: &Option<Option<Vec<u8>>>) -> Option<String> {
    match value {
        None => None,
        Some(None) => Some("ERR".into()),
        Some(Some(b)) => Some(ty.format_plain(b)),
    }
}

/// 一帧的一行。t 是相对第一帧的探针时间（秒）。
///
/// The line of one frame. `t` is the probe time in seconds relative to the first frame.
pub fn row(frame: &HostFrame, t: f64, cells: &[Option<String>]) -> String {
    let mut line = format!(
        "{:.6},{t:.6},{}",
        frame.host_unix_us as f64 / 1e6,
        frame.tick
    );
    for c in cells {
        line.push(',');
        if let Some(c) = c {
            line.push_str(c);
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_row() {
        assert_eq!(
            header(["a", "s.x,y", "q\""]),
            "host_time,t,sample_tick,a,\"s.x,y\",\"q\"\"\""
        );
        let f = HostFrame {
            tick: 7,
            probe_us: 0,
            host_unix_us: 1_500_000,
            values: vec![],
        };
        let ty = ValueType::unsigned(2);
        let cells = [
            cell(&ty, &Some(Some(vec![0x34, 0x12]))),
            cell(&ty, &None),
            cell(&ty, &Some(None)),
        ];
        assert_eq!(row(&f, 0.25, &cells), "1.500000,0.250000,7,4660,,ERR");
    }
}
