//! 只读探针:打印游戏自身 YSCM 的「序号 → 命令名」全表(诊断用)。
//! 用法: cargo run -p yuris-tools --release --example cmdlist -- <游戏目录>
use yuris_vm::host::{is_script_entry, YpfScriptHost};
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: cmdlist <dir>"));
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
    let pack = packs.iter().find(|p| {
        yuris_format::ypf::YpfIndex::from_path_auto(p)
            .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
            .unwrap_or(false)
    }).cloned().expect("脚本包");
    let mut host = YpfScriptHost::open(&pack).expect("open");
    let blob = host.entry_names().into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok()).expect("ysc");
    let t = yuris_format::yscm::YscmTable::from_bytes(&blob).expect("YSCM");
    for (i, c) in t.commands().iter().enumerate() {
        println!("{i:3} {}", c.name);
    }
}
