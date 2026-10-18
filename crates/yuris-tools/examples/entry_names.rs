//! 只读探针:按目录前缀列出条目名并**按 CP932 正确解码**(通用)。
//!
//! 用途:包内条目名是 CP932;`ypf_entries` 用 lossy UTF-8 显示会变替换字符,
//! 无法判断「这个日文名到底是什么」。本探针对名字做真正的 CP932 解码。
//!
//! 用法:
//!   cargo run --release -p yuris-tools --example entry_names -- <游戏目录> <前缀> [上限]

use yuris_vm::host::{is_script_entry, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: entry_names <dir> <prefix> [limit]"));
    let prefix = args.next().unwrap_or_default().to_ascii_lowercase();
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) {
                packs.push(p);
            }
        }
    }
    packs.sort();
    let pack = packs
        .iter()
        .find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        })
        .cloned()
        .expect("找不到脚本包");
    // **关键**:必须走 `YpfIndex` 拿**原始字节名** —— `YpfScriptHost::entry_names()`
    // 返回的是 lossy UTF-8(CP932 日文已变成替换字符,无法再还原)。
    let (idx, _, _) = yuris_format::ypf::YpfIndex::from_path_auto(&pack).expect("解析索引");
    let mut n = 0usize;
    for e in &idx.entries {
        let low = String::from_utf8_lossy(&e.name).to_ascii_lowercase();
        if !low.contains(&prefix) {
            continue;
        }
        let (dec, _, _) = encoding_rs::SHIFT_JIS.decode(&e.name);
        println!("[{:>5} B] {dec}", e.uncompressed_len);
        n += 1;
        if limit > 0 && n >= limit {
            break;
        }
    }
    eprintln!("[entry_names] 命中 {n} 条(前缀 {prefix:?})");
}
