//! 只读探针:CG / CGACT 的**槽位用法**取证 —— 显隐到底由哪个槽决定。
//!
//! 起因:播放器把「CG 命令带 FILE」当成「该对象可见」,实机出现整屏
//! `cgsys/config/*` 按钮底图叠在一起。判定「哪些 CG 真的上屏」需要
//! 引擎侧语义,而 CG 的 MODE(槽 41@v494)与 CGACT 的 SET(槽 9@v494)
//! 是仅有的两个候选来源 —— 本探针把**语料里实际出现过的取值**列出来,
//! 供与引擎二进制对拍,不猜。
//!
//! 槽位号从**游戏自身 YSCM**取(逐版本漂移),不硬编码。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cgmode -- <游戏目录> [样例上限]

use std::collections::BTreeMap;

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn find_boot(host: &mut YpfScriptHost, suffix_lower: &str) -> Option<Vec<u8>> {
    let n = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(suffix_lower))?;
    host.read_entry(&n).ok()
}

/// 常量取值:窗内容解出的指令恰为一条 `PushInt` 时返回它。
fn const_int(script: &yuris_format::ystb::YstbFile, w: &yuris_format::ystb::CommandSlot) -> Option<i64> {
    let b = script.window_bytes_pooled_copy(w)?;
    let ins = yuris_script::decode_window(&b).ok()?;
    match ins.as_slice() {
        [yuris_script::Instruction { kind: yuris_script::Insn::PushInt(n), .. }] => Some(*n),
        _ => None,
    }
}

fn const_str(script: &yuris_format::ystb::YstbFile, w: &yuris_format::ystb::CommandSlot) -> Option<String> {
    let b = script.window_bytes_pooled_copy(w)?;
    let ins = yuris_script::decode_window(&b).ok()?;
    match ins.as_slice() {
        [yuris_script::Instruction { kind: yuris_script::Insn::PushStr(s), .. }] => {
            Some(String::from_utf8_lossy(s).into_owned())
        }
        _ => None,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: cgmode <dir> [max_samples]"));
    let max_samples: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);

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

    // ---- 槽位号:来自游戏自身 YSCM ----
    let ysc = find_boot(&mut host, r"ysbin\ysc.ybn").expect("ysc.ybn");
    let t = yuris_format::yscm::YscmTable::from_bytes(&ysc).expect("解析 YSCM");
    let slot_of = |cmd: &str, param: &str| -> Option<u8> {
        let c = t.commands().iter().find(|c| c.name.eq_ignore_ascii_case(cmd))?;
        c.params.iter().position(|p| p.name.eq_ignore_ascii_case(param)).map(|i| i as u8)
    };
    let cg_file = slot_of("CG", "FILE").expect("CG.FILE");
    let cg_mode = slot_of("CG", "MODE").expect("CG.MODE");
    let cg_id = slot_of("CG", "ID").expect("CG.ID");
    let act_set = slot_of("CGACT", "SET").expect("CGACT.SET");
    let act_set2 = slot_of("CGACT", "SET2");
    let act_id = slot_of("CGACT", "ID").expect("CGACT.ID");
    println!(
        "YSCM v{}  CG.ID={cg_id} CG.FILE={cg_file} CG.MODE={cg_mode}  \
         CGACT.ID={act_id} CGACT.SET={act_set} CGACT.SET2={act_set2:?}",
        t.version()
    );

    let ids = host.script_ids().expect("script ids");
    let mut cg_total = 0usize;
    let mut cg_with_file = 0usize;
    let mut cg_with_mode = 0usize;
    let mut mode_hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut set_hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for sid in ids {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = t
                .commands()
                .get(g.command_type as usize)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            if !name.eq_ignore_ascii_case("CG") && !name.eq_ignore_ascii_case("CGACT") {
                continue;
            }
            let first = ctx.first_slots[gi];
            let wins = &ctx.script.slots()[first..first + g.window_count as usize];
            let mut got_id: Option<String> = None;
            let mut got_file: Option<String> = None;
            let mut got_mode: Option<String> = None;
            let mut got_set: Option<String> = None;
            for w in wins {
                let slot = (w.tag & 0xff) as u8;
                if name.eq_ignore_ascii_case("CG") {
                    if slot == cg_id {
                        got_id = const_str(&ctx.script, w);
                    } else if slot == cg_file {
                        got_file = const_str(&ctx.script, w).or_else(|| Some("<expr>".into()));
                    } else if slot == cg_mode {
                        got_mode = const_int(&ctx.script, w)
                            .map(|n| n.to_string())
                            .or_else(|| Some("<expr>".into()));
                    }
                } else if slot == act_set {
                    got_set = const_int(&ctx.script, w)
                        .map(|n| n.to_string())
                        .or_else(|| Some("<expr>".into()));
                }
            }
            if name.eq_ignore_ascii_case("CG") {
                cg_total += 1;
                if got_file.is_some() {
                    cg_with_file += 1;
                }
                if got_mode.is_some() {
                    cg_with_mode += 1;
                    *mode_hist.entry(got_mode.clone().unwrap()).or_default() += 1;
                }
                if got_file.is_some() && samples.len() < max_samples {
                    samples.push(format!(
                        "  s{sid} g{gi} id={:?} FILE={:?} MODE={:?}",
                        got_id.as_deref().unwrap_or("-"),
                        got_file.as_deref().unwrap_or("-"),
                        got_mode.as_deref().unwrap_or("<无槽>")
                    ));
                }
            } else if let Some(s) = got_set {
                *set_hist.entry(s).or_default() += 1;
            }
        }
    }

    println!("\n== CG 组 {cg_total} 个:带 FILE 的 {cg_with_file} 个,带 MODE 槽的 {cg_with_mode} 个 ==");
    println!("== CG.MODE 取值直方图 ==");
    for (k, v) in &mode_hist {
        println!("  {k:>8}  ×{v}");
    }
    println!("== CGACT.SET 取值直方图 ==");
    for (k, v) in &set_hist {
        println!("  {k:>8}  ×{v}");
    }
    println!("\n== 前 {max_samples} 个带 FILE 的 CG ==");
    for s in &samples {
        println!("{s}");
    }
}
