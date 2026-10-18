//! 只读探针:全语料检索「谁引用了变量 id X」—— 区分读(0x48)与引用/写(0x56/0x76),
//! 从而定位「某全局变量到底由哪段脚本赋值」。
//!
//! 用法: cargo run -p yuris-tools --release --example whowrites -- <游戏目录> <varid_dec> [最多]
use std::collections::BTreeMap;
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: whowrites <dir> <varid>"));
    let want: u16 = a.next().and_then(|s| s.parse().ok()).expect("变量 id(十进制)");
    let limit: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(40);
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
    let mut stat: BTreeMap<String, usize> = BTreeMap::new();
    let mut shown = 0usize;
    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = t.commands().get(g.command_type as usize).map(|c| c.name.clone())
                .unwrap_or_else(|| format!("cmd{}", g.command_type));
            let first = ctx.first_slots[gi];
            for (wi, w) in ctx.script.slots()[first..first + g.window_count as usize].iter().enumerate() {
                let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
                let Ok(ins) = yuris_script::decode_window(&b) else { continue };
                for i in &ins {
                    let hit = match &i.kind {
                        yuris_script::Insn::PushVar(r) if r.id == want => Some("读(0x48)"),
                        yuris_script::Insn::PushVarRef(r) if r.id == want => Some("引用(0x56)"),
                        yuris_script::Insn::PushVarIndexed(r) if r.id == want => Some("下标(0x76)"),
                        _ => None,
                    };
                    if let Some(k) = hit {
                        *stat.entry(format!("{k} in {name} 槽{}", w.tag & 0xff)).or_default() += 1;
                        if shown < limit {
                            let dump: Vec<String> = ins.iter().map(|x| match &x.kind {
                                yuris_script::Insn::PushInt(n) => format!("pushint({n})"),
                                yuris_script::Insn::PushVar(r) | yuris_script::Insn::PushVarRef(r)
                                | yuris_script::Insn::PushVarIndexed(r) => format!("var({},{})", r.space.prefix() as char, r.id),
                                k => k.mnemonic().to_string(),
                            }).collect();
                            println!("s{sid} g{gi} {name} 窗{wi}: {}", dump.join(" "));
                            shown += 1;
                        }
                    }
                }
            }
        }
    }
    println!("== 汇总");
    for (k, v) in &stat { println!("  ×{v:<6} {k}"); }
}
