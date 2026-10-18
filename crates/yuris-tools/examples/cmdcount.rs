//! 只读探针:全语料命令使用计数(判定「本作用哪条命令承担某职责」)。
//! 用法: cargo run -p yuris-tools --release --example cmdcount -- <游戏目录> [名字...]
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: cmdcount <dir> [names...]"));
    let want: Vec<String> = a.map(|s| s.to_ascii_uppercase()).collect();
    let mut packs = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) { packs.push(p); }
        }
    }
    packs.sort();
    let pack = packs.iter().find(|p| yuris_format::ypf::YpfIndex::from_path_auto(p)
        .map(|(idx,_,_)| idx.entries.iter().any(|e| is_script_entry(&e.name))).unwrap_or(false))
        .cloned().expect("脚本包");
    let mut host = YpfScriptHost::open(&pack).expect("open");
    let ysc = host.entry_names().into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok()).expect("ysc");
    let t = yuris_format::yscm::YscmTable::from_bytes(&ysc).expect("YSCM");
    let mut cnt = std::collections::BTreeMap::<String, usize>::new();
    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        for g in ctx.groups.iter() {
            let n = t.commands().get(g.command_type as usize).map(|c| c.name.clone())
                .unwrap_or_else(|| format!("cmd{}", g.command_type));
            *cnt.entry(n).or_default() += 1;
        }
    }
    for (n, c) in &cnt {
        if want.is_empty() || want.iter().any(|w| n.to_ascii_uppercase() == *w) {
            println!("{c:>8}  {n}");
        }
    }
}
