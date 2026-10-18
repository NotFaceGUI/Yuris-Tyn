//! YPF 条目探针:按名字子串列出索引条目(flag/uncomp/comp/off)+ 数据区首字节魔数。
//!
//! 用途:`read_image_bytes` 之类「按类型码放行」的判据失配时,看**数据侧**到底
//! 是什么(真实魔数)以及**索引侧**记的类型码是多少 —— 两边对照才不猜。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example ypf_entries -- <游戏目录> <名字子串> [最多条数]

use std::io::{Read, Seek, SeekFrom};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: ypf_entries <dir> <substr>"));
    let needle = args.next().unwrap_or_default().to_ascii_lowercase();
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);

    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) {
                packs.push(p);
            }
        }
    }
    packs.sort();

    for pack in packs {
        let Ok((idx, dialect, score)) = yuris_format::ypf::YpfIndex::from_path_auto(&pack) else {
            continue;
        };
        println!(
            "== {} 方言 {dialect:?} 自洽 {:.1}% 条目 {}",
            pack.display(),
            score * 100.0,
            idx.entries.len()
        );
        let mut f = match std::fs::File::open(&pack) {
            Ok(f) => f,
            Err(e) => {
                println!("   打开失败 {e}");
                continue;
            }
        };
        let mut shown = 0usize;
        // flag 直方图(看清该包到底有哪些类型码)
        let mut hist: std::collections::BTreeMap<u8, usize> = std::collections::BTreeMap::new();
        for e in &idx.entries {
            *hist.entry(e.flag).or_default() += 1;
        }
        let hs: Vec<String> = hist.iter().map(|(k, v)| format!("0x{k:02x}={v}")).collect();
        println!("   flag 分布: {}", hs.join(" "));

        // `=03` 形式 = 按 flag 过滤(看清某类型码下到底是什么内容)
        let flag_only: Option<u8> = needle
            .strip_prefix('=')
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        for e in &idx.entries {
            let name = String::from_utf8_lossy(&e.name).to_ascii_lowercase();
            if let Some(fv) = flag_only {
                if e.flag != fv {
                    continue;
                }
            } else if !needle.is_empty() && !name.contains(&needle) {
                continue;
            }
            let mut magic = [0u8; 8];
            let mg = if f.seek(SeekFrom::Start(e.offset as u64)).is_ok()
                && f.read_exact(&mut magic).is_ok()
            {
                magic.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
            } else {
                "<读失败>".into()
            };
            let asci: String = magic
                .iter()
                .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
                .collect();
            println!(
                "   flag=0x{:02x} uncomp={} comp={} off={} 魔数=[{mg}] {asci}  {}",
                e.flag,
                e.uncompressed_len,
                e.compressed_len,
                e.offset,
                String::from_utf8_lossy(&e.name)
            );
            shown += 1;
            if shown >= limit {
                break;
            }
        }
        if shown == 0 {
            println!("   （无名字匹配 {needle}）");
        }
    }
}
