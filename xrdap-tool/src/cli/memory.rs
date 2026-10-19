//! 按变量名访问目标内存：vars、read、write。

use std::io::Write;

use xrdap::dap::Probe;
use xrdap::expr::Target;
use xrdap::vendor::{self, Item};

use super::connect::{load_symbols, ConnOpts};
use super::text::{ljust, name_width};
use super::CliResult;

pub fn cmd_vars(conn: &ConnOpts, pattern: Option<&str>, out: &mut dyn Write) -> CliResult {
    if conn.elf.is_none() {
        return Err("要用 --elf 指定 ELF 文件".into());
    }
    let syms = load_symbols(conn)?;
    let mut vars: Vec<_> = syms
        .vars
        .values()
        .flatten()
        .filter(|v| pattern.is_none_or(|p| v.name.contains(p)))
        .collect();
    vars.sort_by_key(|v| (v.addr, v.name.clone()));
    for v in vars {
        let size = syms
            .size_of(v.ty)
            .map(|s| format!("{s} 字节"))
            .unwrap_or_else(|| "大小未知".into());
        writeln!(
            out,
            "0x{:08X}  {:<24} {}，{}",
            v.addr,
            v.name,
            syms.name_of(v.ty),
            size
        )?;
    }
    Ok(())
}

pub fn cmd_read(probe: &mut Probe, targets: &[Target], out: &mut dyn Write) -> CliResult {
    let items: Vec<Item> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| t.item(i as u16))
        .collect();
    let results = probe.read(&items)?;
    let w = name_width(&targets.iter().collect::<Vec<_>>());
    for (t, r) in targets.iter().zip(results) {
        if r.status == 0 {
            writeln!(out, "{} = {}", ljust(&t.name, w), t.ty.format(&r.value))?;
        } else {
            writeln!(
                out,
                "{}   读取失败：{}（地址 {}）",
                ljust(&t.name, w),
                vendor::item_status_text(r.status, &t.chain),
                t.location()
            )?;
        }
    }
    Ok(())
}

pub fn cmd_write(
    probe: &mut Probe,
    list: &[(Target, String, Vec<u8>)],
    out: &mut dyn Write,
) -> CliResult {
    let items: Vec<(Item, Vec<u8>)> = list
        .iter()
        .enumerate()
        .map(|(i, (t, _, b))| (t.item(i as u16), b.clone()))
        .collect();
    let results = probe.write(&items)?;
    let back = probe.read(&items.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>())?;
    let w = name_width(&list.iter().map(|(t, _, _)| t).collect::<Vec<_>>());
    let mut failed = 0;
    for (((t, text, _), r), b) in list.iter().zip(results).zip(back) {
        if r.status == 0 {
            let readback = if b.status == 0 {
                t.ty.format(&b.value)
            } else {
                format!("读回失败：{}", vendor::item_status_text(b.status, &t.chain))
            };
            writeln!(out, "{} ← {text}，读回 {readback}", ljust(&t.name, w))?;
        } else {
            failed += 1;
            writeln!(
                out,
                "{}   写入失败：{}（地址 {}）",
                ljust(&t.name, w),
                vendor::item_status_text(r.status, &t.chain),
                t.location()
            )?;
        }
    }
    if failed > 0 {
        return Err(format!("{failed} 个变量写入失败").into());
    }
    Ok(())
}
