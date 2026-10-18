//! 只读探针:打印**游戏自身** YSCM 里若干命令的参数名表(下标 = 槽位号)。
//!
//! 用途:窗口 `tag & 0xff` 是槽位号,而槽位号逐版本漂移 —— 参数名必须
//! 从游戏自己的 `ysbin\ysc.ybn` 取,不得硬编码。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example params -- <游戏目录> [命令名...]

use yuris_vm::host::{is_script_entry, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: params <dir> [cmd...]"));
    let want: Vec<String> = args.map(|s| s.to_ascii_uppercase()).collect();

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
    let mut host = YpfScriptHost::open(&pack).expect("打开脚本包");
    let blob = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok())
        .expect("ysc.ybn");
    let t = yuris_format::yscm::YscmTable::from_bytes(&blob).expect("解析 YSCM");
    println!("YSCM v{} 命令 {} 条", t.version(), t.commands().len());
    for (i, c) in t.commands().iter().enumerate() {
        if !want.is_empty() && !want.contains(&c.name.to_ascii_uppercase()) {
            continue;
        }
        println!("--- idx {i} {} ({} 参)", c.name, c.params.len());
        for (s, p) in c.params.iter().enumerate() {
            if p.name.is_empty() {
                continue;
            }
            println!("    槽 {s:3}  {:<12} ty=0x{:x}", p.name, p.ty);
        }
    }
}
