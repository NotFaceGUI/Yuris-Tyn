//! 只读探针：从任意 YPF 封包内读出 `ysbin\ysc.ybn` 并打印命令参数表。
//!
//! 用途：跨版本核对参数**槽号**（窗口 `tag & 0xff` = 槽号）。参数名跨版本
//! 稳定、槽号会漂移，因此实现必须按名字解析参数；本探针给出某个发行版本
//! 的实际槽序。
//!
//! 用法：
//!   cargo run -p yuris-format --example ysc_params -- <pack.ypf> [命令名...]

use std::io::{Read, Seek, SeekFrom};

use yuris_format::ypf::YpfIndex;
use yuris_format::yscm::YscmTable;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = std::path::PathBuf::from(args.next().expect("usage: ysc_params <pack.ypf> [cmd...]"));
    let want: Vec<String> = args.map(|s| s.to_ascii_uppercase()).collect();

    let (idx, dialect, score) = YpfIndex::from_path_auto(&path).expect("解析 YPF 索引");
    eprintln!("[ysc_params] dialect={dialect:?} score={score:.3} entries={}", idx.entries.len());
    let entry = idx
        .entries
        .iter()
        .find(|e| {
            let low = String::from_utf8_lossy(&e.name).to_ascii_lowercase();
            low.ends_with(r"ysbin\ysc.ybn") || low.ends_with("ysbin/ysc.ybn")
        })
        .expect("包内没有 ysbin\\ysc.ybn");

    let mut f = std::fs::File::open(&path).expect("open pack");
    f.seek(SeekFrom::Start(u64::from(entry.offset))).expect("seek");
    let mut raw = vec![0u8; entry.compressed_len as usize];
    f.read_exact(&mut raw).expect("read entry");
    let blob = if raw.starts_with(&[0x78]) {
        use flate2::read::ZlibDecoder;
        let mut out = Vec::with_capacity(entry.uncompressed_len as usize);
        ZlibDecoder::new(&raw[..]).read_to_end(&mut out).expect("zlib");
        out
    } else {
        raw
    };

    let table = YscmTable::from_bytes(&blob).expect("解析 YSCM");
    println!("YSCM v{} 命令 {} 条", table.version(), table.commands().len());
    for (i, cmd) in table.commands().iter().enumerate() {
        if !want.is_empty() && !want.contains(&cmd.name.to_ascii_uppercase()) {
            continue;
        }
        println!("--- idx {i} {} ({} 参)", cmd.name, cmd.params.len());
        for (slot, p) in cmd.params.iter().enumerate() {
            if p.name.is_empty() {
                continue;
            }
            println!("    槽 {slot:3}  {:<12} ty=0x{:x}", p.name, p.ty);
        }
    }
}
