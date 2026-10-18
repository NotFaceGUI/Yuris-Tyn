//! 只读探针:从第三方 YPF(如 <game-b> / v494)抽取并解析 YSCM 命令字典。
//!
//! 用途:跨版本命令表对照 —— 判定该游戏引擎版本的命令名列表。
//! 用法:
//!   cargo run -p yuris-tools --example tmp_yscm_dump -- <ypf> <name_key_hex> [名字子串]
//!
//! 低内存:只用 `YpfReader`(索引 + 按需 seek),不整载封包。

use yuris_format::ypf::YpfReader;
use yuris_format::yscm::YscmTable;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: tmp_yscm_dump <ypf> <key_hex> [substr]");
    let key = u8::from_str_radix(
        args.next().unwrap_or_else(|| "ff".into()).trim_start_matches("0x"),
        16,
    )
    .unwrap();
    let substr = args.next().unwrap_or_else(|| "ysc".into());

    let mut r = YpfReader::open(std::path::Path::new(&path), key).expect("open ypf");
    let (version, n_entries, header_count, hits): (u32, usize, u32, Vec<(usize, Vec<u8>, u8, u32, u32, u32)>) = {
        let idx = r.index();
        let hits = idx
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let s = String::from_utf8_lossy(&e.name).to_ascii_lowercase();
                s.contains(&substr.to_ascii_lowercase())
            })
            .map(|(i, e)| {
                (
                    i,
                    e.name.clone(),
                    e.flag,
                    e.uncompressed_len,
                    e.compressed_len,
                    e.offset,
                )
            })
            .collect();
        (
            idx.header.version,
            idx.entries.len(),
            idx.header.file_count,
            hits,
        )
    };
    println!("version={version} entries={n_entries} header={header_count}");
    println!("--- {substr} 命中 {} 条 ---", hits.len());
    for (i, n, ..) in hits.iter().take(60) {
        println!("  [{i}] {:?}", String::from_utf8_lossy(n));
    }

    // 精确名 → 解析
    for (i, n, flag, uncomp, comp, off) in &hits {
        let low = String::from_utf8_lossy(n).to_ascii_lowercase();
        if !low.ends_with("ysc.ybn") {
            continue;
        }
        println!("=== 解析 [{i}] {:?} ===", String::from_utf8_lossy(n));
        println!("  flag={flag:#x} uncomp={uncomp} comp={comp} off={off:#x}");
        match r.read(n) {
            Ok(blob) => {
                println!("  blob {} bytes, magic={:?}", blob.len(), &blob[..4.min(blob.len())]);
                match YscmTable::from_bytes(&blob) {
                    Ok(t) => {
                        println!(
                            "  YSCM version={} unknown={} commands={} params={} tail_off=0x{:x} tail_len={}",
                            t.version(),
                            t.unknown,
                            t.commands().len(),
                            t.param_total(),
                            t.tail_offset,
                            t.tail.len()
                        );
                        let names: Vec<&str> = t.commands().iter().map(|c| c.name.as_str()).collect();
                        println!("  names[0..12] = {:?}", &names[..12.min(names.len())]);
                        // 供对照:全量名列表(按序号)
                        println!("  --- 全量(序号:名) ---");
                        for (i, c) in t.commands().iter().enumerate() {
                            println!("  {i}\t{}", c.name);
                        }
                    }
                    Err(e) => println!("  YSCM parse error: {e}"),
                }
            }
            Err(e) => println!("  read error: {e}"),
        }
    }
}
