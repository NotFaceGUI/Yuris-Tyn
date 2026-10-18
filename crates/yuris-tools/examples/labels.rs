//! 标签普查:列出 YSLB 中名字含指定子串的标签(脚本号 + 目标 pc)。
//!
//! 用法:`labels <游戏目录> <子串…>`
//!
//! 用途:定位"哪段脚本负责某个玩法"(如标题、存档、回想),
//! 从**游戏自身的标签表**取真值,不靠猜脚本号。

use std::path::PathBuf;

use yuris_format::yslb::YslbTable;
use yuris_vm::host::{YpfScriptHost, is_script_entry};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: labels <游戏目录> <子串…>"));
    let needles: Vec<String> = args.map(|s| s.to_ascii_lowercase()).collect();

    let mut packs: Vec<PathBuf> = Vec::new();
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

    let mut host = YpfScriptHost::open(&pack).expect("打开脚本包");
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
        .expect("ysl.ybn");
    let data = host.read_entry(&name).expect("读 ysl.ybn");
    let t = YslbTable::from_bytes(&data).expect("解析 YSLB");
    println!("YSLB 标签总数 {}", t.labels().len());

    let mut hit = 0usize;
    for l in t.labels() {
        let n = String::from_utf8_lossy(&l.name).to_string();
        let low = n.to_ascii_lowercase();
        if needles.is_empty() || needles.iter().any(|k| low.contains(k)) {
            println!("s{:>5} pc{:<5} {}", l.script_id, l.target_pc, n);
            hit += 1;
            // 上限可经环境变量放宽(旧值 400 会静默截断 —— 实测 YSLB 有 2132 条,
            // 400 之后的关键标签如 MAC.* 族全部看不到)。
            let cap: usize = std::env::var("YURIS_LABELS_MAX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(usize::MAX);
            if hit >= cap {
                println!("…(截断 @{cap})");
                break;
            }
        }
    }
    println!("命中 {hit} 条");
}
