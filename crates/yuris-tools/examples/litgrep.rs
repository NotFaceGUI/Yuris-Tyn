//! 只读探针:在**全部脚本**的窗口里按子串检索字符串字面量(不执行 VM)。
//!
//! 用途:游戏把 CG 路径写成运行期拼接(如 `"cgsys/" & ... & "_bt3"`),
//! 直方图看不出归属 —— 本探针给出「哪段脚本的哪个组、哪条窗」含该字面量,
//! 从而定位「谁负责建某块画面」。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example litgrep -- <游戏目录> <子串> [脚本号上限]

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: litgrep <dir> <needle>"));
    let needle = args.next().unwrap_or_default().to_ascii_lowercase();
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(200);

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

    let ysc = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok())
        .expect("ysc.ybn");
    let t = yuris_format::yscm::YscmTable::from_bytes(&ysc).expect("YSCM");

    let mut hits = 0usize;
    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = t
                .commands()
                .get(g.command_type as usize)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("cmd{}", g.command_type));
            let first = ctx.first_slots[gi];
            for (wi, w) in ctx.script.slots()[first..first + g.window_count as usize]
                .iter()
                .enumerate()
            {
                let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
                let Ok(ins) = yuris_script::decode_window(&b) else { continue };
                for i in &ins {
                    if let yuris_script::Insn::PushStr(s) = &i.kind {
                        let sv = String::from_utf8_lossy(s).into_owned();
                        if sv.to_ascii_lowercase().contains(&needle) {
                            hits += 1;
                            if hits <= limit {
                                println!(
                                    "s{sid} g{gi} {name} 槽{} 窗{wi}: {sv:?}",
                                    w.tag & 0xff
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    println!("== 命中 {hits} 处(上限 {limit})");
}
