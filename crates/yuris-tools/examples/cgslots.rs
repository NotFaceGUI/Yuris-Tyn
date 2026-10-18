//! 只读探针:CG 的**槽位名 → 序号**表 + **语料实际用法计数**(取证,不猜)。
//!
//! 用途:渲染对齐(SDK 手册的 `cg/cg.html`)要实装 `T`/`TID`/`TA`(过渡)、
//! `MID`(外部遮罩)、`SX`/`SY`(图层组尺寸)、`MODE`(混合模式)——
//! 但**先要证明本作到底用不用**,以及它在**本作 YSCM 里的序号**是多少
//! (逐版本漂移,不得硬编码)。
//!
//! 输出两段:
//! 1. `ysbin\ysc.ybn` 里 `CG`/`CGACT`/`CGINFO` 的槽位表(序号 = 名);
//! 2. 全语料扫一遍,统计每个槽名出现的**调用次数**(含取值样例),
//!    以及每条 CG 调用带的槽数分布。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cgslots -- <游戏目录> [样例上限]

use std::collections::BTreeMap;

use yuris_format::ystb::CommandSlot;
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn find_boot(host: &mut YpfScriptHost, suffix_lower: &str) -> Option<Vec<u8>> {
    let n = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(suffix_lower))?;
    host.read_entry(&n).ok()
}

/// 常量取值:窗内容解出的指令恰为一条 `PushInt` 时返回它。
fn const_int(script: &yuris_format::ystb::YstbFile, w: &CommandSlot) -> Option<i64> {
    let b = script.window_bytes_pooled_copy(w)?;
    let ins = yuris_script::decode_window(&b).ok()?;
    match ins.as_slice() {
        [yuris_script::Instruction {
            kind: yuris_script::Insn::PushInt(n),
            ..
        }] => Some(*n),
        _ => None,
    }
}

fn const_str(script: &yuris_format::ystb::YstbFile, w: &CommandSlot) -> Option<String> {
    let b = script.window_bytes_pooled_copy(w)?;
    let ins = yuris_script::decode_window(&b).ok()?;
    match ins.as_slice() {
        [yuris_script::Instruction {
            kind: yuris_script::Insn::PushStr(s),
            ..
        }] => Some(String::from_utf8_lossy(s).into_owned()),
        _ => None,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: cgslots <dir> [max_samples]"));
    let max_samples: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);

    // ---- 脚本包 = 含 ysbin\yst*.ybn 的那一个 ----
    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension()
                .map(|x| x.eq_ignore_ascii_case("ypf"))
                .unwrap_or(false)
            {
                packs.push(p);
            }
        }
    }
    packs.sort();
    let script_pack = packs
        .into_iter()
        .find(|p| {
            let Ok(mut h) = YpfScriptHost::open(p) else {
                return false;
            };
            h.entry_names()
                .iter()
                .any(|n| is_script_entry(n.as_bytes()))
        })
        .expect("没有可用的脚本包");

    let mut host = YpfScriptHost::open(&script_pack).expect("打开脚本包");
    let ids = host.script_ids().expect("script ids");

    // ---- 1) 槽位表 ----
    let ysc = find_boot(&mut host, "ysc.ybn").expect("ysbin\\ysc.ybn");
    let table = yuris_format::yscm::YscmTable::from_bytes(&ysc).expect("YSCM");
    println!("YSCM v{} · 命令 {} 条", table.version(), table.commands().len());
    for want in ["CG", "CGACT", "CGINFO", "CGEND", "CLIP", "CLIPACT"] {
        let Some(c) = table.command(&want) else {
            println!("  {want}: **不在本作 YSCM**(该版本无此命令)");
            continue;
        };
        let names: Vec<String> = c
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{i}:{}", p.name))
            .collect();
        println!("  {want} ({}) => {}", c.params.len(), names.join(" "));
    }

    // 槽名 → 序号(按 CG 的表;语料计数用)
    let cg = table.command("CG").expect("CG");
    let slot_of: BTreeMap<String, usize> = cg
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), i))
        .collect();
    let want_slots = [
        "ID", "FILE", "E", "A", "T", "TID", "TA", "X", "Y", "Z", "SX", "SY", "MODE", "MID", "ZP",
    ];

    // ---- 2) 语料扫描 ----
    println!("\n扫描 {} 个脚本…", ids.len());
    let mut hit: BTreeMap<&str, usize> = want_slots.iter().map(|s| (*s, 0usize)).collect();
    let mut samples: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut n_calls = 0usize;
    let mut slot_hist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut file_only_attr: BTreeMap<String, usize> = BTreeMap::new();

    // CGACT 槽名(动画/特效族)+ 计数
    let act = table.command("CGACT").expect("CGACT");
    let act_of: BTreeMap<usize, String> = act
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p.name.clone()))
        .collect();
    let mut act_hit: BTreeMap<String, usize> = BTreeMap::new();
    let mut act_samples: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut act_calls = 0usize;
    let mut act_pair: BTreeMap<String, usize> = BTreeMap::new();

    for sid in ids {
        let Ok(ctx) = host.load(sid) else { continue };
        let script = &ctx.script;
        for (pc, g) in ctx.groups.iter().enumerate() {
            let cname = table
                .command_at(g.command_type as usize)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            if cname.eq_ignore_ascii_case("CGACT") {
                act_calls += 1;
                let first = ctx.first_slots[pc];
                let ws: Vec<CommandSlot> =
                    script.slots()[first..first + g.window_count as usize].to_vec();
                let mut id = String::from("?");
                let mut sets: Vec<String> = Vec::new();
                for w in &ws {
                    let wslot = (w.tag & 0xff) as usize;
                    let Some(sn) = act_of.get(&wslot) else { continue };
                    *act_hit.entry(sn.clone()).or_insert(0) += 1;
                    let v = const_int(script, w)
                        .map(|v| v.to_string())
                        .or_else(|| const_str(script, w).map(|s| s.trim_matches('"').to_string()))
                        .unwrap_or_else(|| "<expr>".into());
                    if sn == "ID" {
                        id = v.clone();
                    }
                    if sn.starts_with("SET") {
                        sets.push(format!("{sn}={v}"));
                    }
                    let e = act_samples.entry(sn.clone()).or_default();
                    if e.len() < 6 && !e.iter().any(|x| x == &v) {
                        e.push(v);
                    }
                }
                let key = if sets.is_empty() {
                    format!("{id} <无 SET>")
                } else {
                    format!("{id} {}", sets.join("+"))
                };
                *act_pair.entry(key).or_insert(0) += 1;
                continue;
            }
            // 只认 CG(不认 CGINFO)—— 槽名同源,但计数要分开
            if !cname.eq_ignore_ascii_case("CG") {
                continue;
            }
            n_calls += 1;
            let first = ctx.first_slots[pc];
            let ws: Vec<CommandSlot> =
                script.slots()[first..first + g.window_count as usize].to_vec();
            *slot_hist.entry(ws.len()).or_insert(0) += 1;
            for w in &ws {
                let wslot = (w.tag & 0xff) as usize;
                let Some(sn) = slot_of.iter().find(|(_, &i)| i == wslot).map(|(n, _)| n.as_str())
                else {
                    continue;
                };
                *hit.entry(sn).or_default() += 1;
                let s = const_int(script, w)
                    .map(|v| v.to_string())
                    .or_else(|| const_str(script, w).map(|s| format!("\"{s}\"")))
                    .unwrap_or_else(|| "<expr>".into());
                let e = samples.entry(sn).or_default();
                if e.len() < max_samples && !e.iter().any(|x| x == &s) {
                    e.push(s);
                }
            }
            // 「有 FILE 且只有 FILE」= 纯纹理替换;这类调用若带属性则是属性设置
            if let Some(fw) = ws.iter().find(|w| (w.tag & 0xff) as usize == slot_of["FILE"]) {
                if let Some(f) = const_str(script, fw) {
                    *file_only_attr.entry(f).or_default() += 1;
                }
            }
        }
    }
    println!("
CGACT 调用总数 {act_calls}");
    let mut ah: Vec<(&String, &usize)> = act_hit.iter().collect();
    ah.sort_by(|a, b| b.1.cmp(a.1));
    println!("CGACT 槽名命中(降序):");
    for (k, v) in ah.iter().take(30) {
        let ex = act_samples.get(*k).map(|s| s.join(" | ")).unwrap_or_default();
        let ex = if ex.len() > 90 { format!("{}…", &ex[..90]) } else { ex };
        println!("  {k:<12} {v:>6}   {ex}");
    }
    let mut ap: Vec<(&String, &usize)> = act_pair.iter().collect();
    ap.sort_by(|a, b| b.1.cmp(a.1));
    println!("CGACT 调用形状(前 20,ID/ID2 + SET*):");
    for (k, v) in ap.iter().take(20) {
        println!("  {v:>5}  {k}");
    }

    println!("
CG 调用总数 {n_calls}");
    println!("每条 CG 的槽数分布: {:?}", slot_hist);
    println!("\n槽名                 序号  出现次数  样例");
    for s in want_slots {
        let idx = slot_of.get(s).map(|i| i.to_string()).unwrap_or("-".into());
        let n = hit[s];
        let ex = samples
            .get(s)
            .map(|v| v.join(" | "))
            .unwrap_or_default();
        let ex = if ex.len() > 110 { format!("{}…", &ex[..110]) } else { ex };
        println!("  {s:<8} {idx:>6}  {n:>8}  {ex}");
    }
}
