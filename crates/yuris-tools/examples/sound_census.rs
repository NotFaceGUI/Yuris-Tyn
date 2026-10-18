//! 只读探针：扫描全语料 `SOUND` 命令组，打印每个参数槽的字面量/变量形态。
//!
//! 用途：确认 `FILE=` 在真实脚本里是「裸名」还是「相对路径」，以及各参数
//! （FADE/LOOP/VOLUME…）的实际取值分布，为播放器侧资源解析与音频后端
//! 参数映射提供证据。
//!
//! 用法：
//!   cargo run -p yuris-tools --release --example sound_census -- <游戏目录> [上限]

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next().expect("usage: sound_census <游戏目录> [上限]"),
    );
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(80);

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

    // YSCM 参数表（名字 → 槽）。
    let blob = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok())
        .expect("ysc.ybn");
    let yscm = yuris_format::yscm::YscmTable::from_bytes(&blob).expect("解析 YSCM");
    let sound = yscm.command("SOUND").expect("YSCM 无 SOUND");
    let names: Vec<&str> = sound.params.iter().map(|p| p.name.as_str()).collect();
    println!("SOUND 参数槽: {}", names.join(" / "));

    let ids = host.script_ids().expect("脚本号列表");
    let mut shown = 0usize;
    let mut total = 0usize;
    for sid in ids {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let Some(name) = yscm.command_at(g.command_type as usize).map(|c| c.name.as_str())
            else {
                continue;
            };
            if name != "SOUND" {
                continue;
            }
            total += 1;
            if shown >= limit {
                continue;
            }
            let first = ctx.first_slots[gi];
            let wins = &ctx.script.slots()[first..first + g.window_count as usize];
            let mut parts: Vec<String> = Vec::new();
            for w in wins {
                if w.len == 0 {
                    continue;
                }
                let slot = (w.tag & 0xff) as usize;
                let pname = names.get(slot).copied().unwrap_or("?");
                let Some(bytes) = ctx.script.window_bytes_pooled_copy(w) else {
                    parts.push(format!("{slot}:{pname}=<pool?>"));
                    continue;
                };
                let desc = match yuris_script::decode_window(&bytes) {
                    Ok(insns) if insns.len() == 1 => match &insns[0].kind {
                        yuris_script::Insn::PushStr(s) => {
                            let (text, _, _) = encoding_rs::SHIFT_JIS.decode(s);
                            format!("\"{}\"", text.replace('\u{0}', ""))
                        }
                        yuris_script::Insn::PushInt(v) => v.to_string(),
                        yuris_script::Insn::PushFloat(v) => format!("{v}"),
                        yuris_script::Insn::PushVar(r) => format!("var@{}", r.id),
                        other => other.mnemonic().to_string(),
                    },
                    Ok(insns) => format!("expr({} insns)", insns.len()),
                    Err(e) => format!("<decode err {e}>"),
                };
                parts.push(format!("{slot}:{pname}={desc}"));
            }
            println!("s{sid} g{gi}: {}", parts.join("  "));
            shown += 1;
        }
    }
    println!("--- SOUND 组共 {total} 个，显示 {shown} 个 ---");
}
