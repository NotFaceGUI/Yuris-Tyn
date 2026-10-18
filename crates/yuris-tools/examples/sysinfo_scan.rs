//! 一次性取证探针(boot 重构诊断):扫描全部脚本中指定命令的**每次调用**,
//! 按窗口解码(`yuris_script::decode_window`)打印「槽号 + 指令序列」,
//! 用来把未实装命令的语义面收窄到「真实出现过的调用形态」。
//!
//! 例:`sysinfo_scan <游戏目录> SYSTEMINFO SEQICON SEQLOCK SEPICON SEQUNLOCK`
//!
//! 输出:`s<脚本> g<组> <命令> [槽:指令, 槽:指令…]`

use std::path::PathBuf;

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: sysinfo_scan <dir> <命令名…>"));
    let needles: Vec<String> = args
        .map(|s| s.to_ascii_uppercase())
        .flat_map(|s| s.split(',').map(|x| x.to_string()).collect::<Vec<_>>())
        .collect();

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
    let profile = load_profile(&mut host);

    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        let slots = ctx.script.slots();
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = profile
                .as_ref()
                .and_then(|p| p.name_of(g.command_type as u16))
                .map(|s| s.to_uppercase())
                .unwrap_or_default();
            if !needles.iter().any(|n| *n == name) {
                continue;
            }
            let first = ctx.first_slots[gi];
            let wins = &slots[first..first + g.window_count as usize];
            let mut parts: Vec<String> = Vec::new();
            for w in wins {
                let bytes = ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .unwrap_or_default();
                let decoded = match yuris_script::decode_window(&bytes) {
                    Ok(list) => format!("{:?}", list),
                    Err(e) => format!("<decode err: {e}>"),
                };
                parts.push(format!("{}:{}", w.tag & 0xff, decoded));
            }
            println!("s{sid:4} g{gi:5} {name:18} {}", parts.join("  ||  "));
        }
    }
}

/// 读游戏自身 ysc.ybn 建名字表(与 disasm 探针同源)。
fn load_profile(host: &mut YpfScriptHost) -> Option<yuris_core::CommandProfile> {
    let suffix = "ysbin\\ysc.ybn";
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(suffix))?;
    let blob = host.read_entry(&name).ok()?;
    let table = yuris_format::yscm::YscmTable::from_bytes(&blob).ok()?;
    let version = table.version();
    let names: Vec<String> = table.commands().iter().map(|c| c.name.clone()).collect();
    Some(yuris_core::CommandProfile::from_names(names, version, name))
}
