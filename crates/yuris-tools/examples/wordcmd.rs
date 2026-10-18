//! 只读探针:全语料列出 `WORD` 命令组及其**裸窗字节**(按 SJIS 解释)。
//! 用途:WORD 的参数窗是裸字面量而非字节码(PROGRESS 已记),需要语料多样性
//! 才能判语义。
//! 用法: cargo run -p yuris-tools --release --example wordcmd -- <游戏目录> [上限]
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: wordcmd <dir> [limit]"));
    let limit: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(60);
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
    let mut n = 0usize;
    let mut hist: std::collections::BTreeMap<String, usize> = Default::default();
    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = t.commands().get(g.command_type as usize).map(|c| c.name.clone())
                .unwrap_or_default();
            if name != "WORD" { continue }
            let first = ctx.first_slots[gi];
            let wins = &ctx.script.slots()[first..first + g.window_count as usize];
            let mut parts: Vec<String> = Vec::new();
            for w in wins {
                let b = ctx.script.window_bytes_pooled_copy(w);
                let s = match b {
                    Some(b) => format!("tag=0x{:x} off={} len={} bytes=[{}] sjis={:?}",
                        w.tag, w.offset, w.len,
                        b.iter().take(24).map(|c| format!("{c:02x}")).collect::<Vec<_>>().join(" "),
                        String::from_utf8_lossy(&b).chars().take(30).collect::<String>()),
                    None => "<池越界>".into(),
                };
                parts.push(s);
            }
            *hist.entry(format!("wc={} {}", g.window_count, parts.join(" | ").chars().take(60).collect::<String>())).or_default() += 1;
            n += 1;
            if n <= limit { println!("s{sid} g{gi} {}", parts.join(" | ")); }
        }
    }
    println!("== WORD 组共 {n} 个;去重形态 {} 种", hist.len());
    for (k, v) in hist.iter().take(30) { println!("  ×{v:<4} {k}"); }
}
