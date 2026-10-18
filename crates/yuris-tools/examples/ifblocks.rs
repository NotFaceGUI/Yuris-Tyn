//! 只读探针:列出脚本的 IF 块树(引擎模型:IF 恒 3 窗,`w1.len`=假目标、
//! `w2.len`=块尾目标,**两域存的是编译期组号而非字节长**)。
//!
//! 用途:执行 trace 只给「跳到哪」,给不出「为什么被跳过」;本探针把每个
//! IF 的条件首变量 + 真/假目标一次列全,便于定位「哪个条件把整段功能关掉」。
//!
//! 用法: cargo run -p yuris-tools --release --example ifblocks -- <游戏目录> <脚本号> [起] [止]
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: ifblocks <dir> <script> [from] [to]"));
    let sid: u16 = a.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let from: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let to: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
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
    let ctx = host.load(sid).expect("load");
    for (gi, g) in ctx.groups.iter().enumerate() {
        if gi < from || gi > to { continue }
        let name = t.commands().get(g.command_type as usize).map(|c| c.name.clone())
            .unwrap_or_else(|| format!("cmd{}", g.command_type));
        let first = ctx.first_slots[gi];
        let wins = &ctx.script.slots()[first..first + g.window_count as usize];
        if name == "IF" {
            let mut desc = String::from("<空>");
            if let Some(b) = ctx.script.window_bytes_pooled_copy(&wins[0]) {
                if let Ok(ins) = yuris_script::decode_window(&b) {
                    let parts: Vec<String> = ins.iter().map(|x| match &x.kind {
                        yuris_script::Insn::PushInt(n) => format!("{n}"),
                        yuris_script::Insn::PushVar(r) | yuris_script::Insn::PushVarRef(r)
                        | yuris_script::Insn::PushVarIndexed(r) => {
                            format!("{}[{}]", r.space.prefix() as char, r.id)
                        }
                        yuris_script::Insn::PushStr(s) =>
                            format!("{:?}", String::from_utf8_lossy(s)),
                        k => k.mnemonic().to_string(),
                    }).collect();
                    desc = parts.join(" ");
                }
            }
            let w1 = wins.get(1).map(|w| w.len).unwrap_or(0);
            let w2 = wins.get(2).map(|w| w.len).unwrap_or(0);
            println!("g{gi:<5} IF    假→{:<5} 尾→{:<5} 条件: {desc}", if w1 != 0 { w1 } else { w2 }, w2);
        } else if matches!(name.as_str(), "ELSE" | "IFBLEND" | "IFEND") {
            println!("g{gi:<5} {name}");
        }
    }
}
