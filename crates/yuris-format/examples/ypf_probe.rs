//! 只读探针:YPF 索引解析 + name_key 打分扫描。
//!
//! 用途:跨版本语料核验(如 <game-b> / YPF v494 是否与本仓库 v500 解析器兼容)。
//! 用法:
//!   cargo run -p yuris-format --example tmp_ypf_probe -- <path> [key_hex]
//!   key 省略时:先试 0xC9,再按「名字可打印率」对 256 个 key 打分取最优。

use yuris_format::ypf::YpfIndex;

/// 名字字节可打印率(ASCII 可打印 + SJIS 双字节区)。
fn score_names(raw: &[u8], k: u8) -> f32 {
    let mut ok = 0usize;
    let mut total = 0usize;
    let mut i = 0usize;
    while i < raw.len() {
        if raw[i] == 0 {
            break;
        }
        let c = raw[i] ^ k;
        total += 1;
        if (0x20..0x7f).contains(&c) {
            ok += 1;
            i += 1;
            continue;
        }
        // SJIS 双字节:前导 0x81-0x9F / 0xE0-0xEF
        if (0x81..=0x9f).contains(&c) || (0xe0..=0xef).contains(&c) {
            ok += 1;
            i += 2;
            continue;
        }
        i += 1;
    }
    if total == 0 {
        0.0
    } else {
        ok as f32 / total as f32
    }
}

fn first_name_raw(path: &std::path::Path) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).unwrap();
    f.seek(SeekFrom::Start(0x24)).unwrap();
    let mut buf = vec![0u8; 64];
    f.read_exact(&mut buf).unwrap();
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(end);
    buf
}

fn report(idx: &YpfIndex, path: &std::path::Path, k: u8) {
    let mut f = std::fs::File::open(path).unwrap();
    use std::io::{Read, Seek, SeekFrom};
    let _ = f.seek(SeekFrom::Start(0x24));
    let mut buf = vec![0u8; 1 << 20];
    let n = f.read(&mut buf).unwrap();
    buf.truncate(n);
    println!("  首条名字原文(裸 decode, 不过边界校验):");
    println!("    raw = {:02x?}", &buf[..buf.iter().position(|&b| b == 0).unwrap_or(32).min(buf.len())]);
    let dec: Vec<u8> = buf[..buf.iter().position(|&b| b == 0).unwrap_or(32).min(buf.len())]
        .iter()
        .map(|&b| b ^ k)
        .collect();
    println!("    dec = {:?}", String::from_utf8_lossy(&dec));

    println!("  parsed entries: {} / header {}", idx.entries.len(), idx.header.file_count);
    for e in idx.entries.iter().take(10) {
        println!(
            "    name={:?} flag={:#04x} uncomp={} comp={} off=0x{:x}",
            String::from_utf8_lossy(&e.name),
            e.flag,
            e.uncompressed_len,
            e.compressed_len,
            e.offset
        );
    }
    let hit: Vec<String> = idx
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.name).to_string())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.contains("ysc") || l.contains("ysv") || l.contains("ysl") || l.contains("yse") || l.ends_with(".ybn")
        })
        .take(15)
        .collect();
    println!("  ysc/ysv/ysl/yse/.ybn 命中 {} 条:", hit.len());
    for n in hit {
        println!("      {n}");
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: tmp_ypf_probe <path> [key_hex]");
    let path = std::path::Path::new(&path);
    let explicit = args
        .next()
        .map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).unwrap());

    let raw = first_name_raw(path);
    if explicit.is_none() {
        println!("=== 首条名字 raw ({} B) ===", raw.len());
        let mut scored: Vec<(f32, u8, String)> = (0u16..=255)
            .map(|k| {
                let k = k as u8;
                let s = score_names(&raw, k);
                let dec: String = raw.iter().map(|&b| (b ^ k) as char).collect();
                (s, k, dec)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        for (s, k, dec) in scored.iter().take(8) {
            let printable: String = dec
                .chars()
                .map(|c| if c.is_ascii_graphic() || c == ' ' { c } else { '.' })
                .collect();
            println!("  key=0x{k:02X} score={s:.3} dec={printable:?}");
        }
    } else {
        let k = explicit.unwrap();
        println!("=== explicit key 0x{k:02X} ===");
        match YpfIndex::from_path(path, k) {
            Ok(idx) => {
                println!(
                    "   version={} count={} parsed={} first_data_off=0x{:x}",
                    idx.header.version,
                    idx.header.file_count,
                    idx.entries.len(),
                    idx.header.first_data_off
                );
                report(&idx, path, k);
            }
            Err(e) => println!("   parse error: {e}"),
        }
    }
}
