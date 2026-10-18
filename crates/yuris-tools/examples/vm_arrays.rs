//! 数组维数探针:启动游戏 → 打印指定变量的声明维数(诊断数组越界)。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example vm_arrays -- <游戏目录> [@id|$id ...]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
use yuris_value::{VarRef, VarSpace};

/// 直接读包内 ysv.ybn,列该变量 id 的条目。
fn host_ysvr_entry(pack: &std::path::Path, id: u16) -> Option<String> {
    let mut host = YpfScriptHost::open(pack).ok()?;
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with("ysbin\\ysv.ybn"))?;
    let blob = host.read_entry(&name).ok()?;
    let t = yuris_format::ysvr::YsvrTable::from_bytes(&blob).ok()?;
    let mut out = Vec::new();
    for e in t.entries() {
        if e.var_id == id {
            out.push(format!(
                "script={} ty={} bounds={:?} init={:?}",
                e.script, e.ty, e.bounds, e.init
            ));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("\n"))
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: vm_arrays <游戏目录> [ids...]"));
    let want: Vec<String> = args.collect();

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
    let host = YpfScriptHost::open(&pack).expect("打开脚本包");
    let booted = Bootstrap::boot_with_host(host, None).expect("启动链");
    let vm = booted.vm;

    for spec in &want {
        if spec == "--declcheck" {
            // 对照「声明窗常量数组上界表达式」与「YSVR bounds」——
            // 判定 bounds = 表达式值 还是 表达式值+1。
            let mut host2 = YpfScriptHost::open(&pack).expect("重开包");
            let ysvr = {
                let name = host2
                    .entry_names()
                    .into_iter()
                    .find(|n| n.to_ascii_lowercase().ends_with("ysbin\\ysv.ybn"))
                    .expect("ysv.ybn");
                let blob = host2.read_entry(&name).expect("读 ysv");
                yuris_format::ysvr::YsvrTable::from_bytes(&blob).expect("解析 ysv")
            };
            let (mut eq, mut plus1, mut other) = (0usize, 0usize, 0usize);
            let mut examples = Vec::new();
            for sid in host2.script_ids().unwrap_or_default() {
                let Ok(ctx) = host2.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for w in wins {
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
                        let Ok(instrs) = yuris_script::decode_window(&b) else { continue };
                        // 常量折叠(与 consume_declarations_into 同规则)
                        let mut st: Vec<i64> = Vec::new();
                        let mut base = None;
                        for ins in &instrs {
                            match &ins.kind {
                                yuris_script::Insn::PushVar(r)
                                | yuris_script::Insn::PushVarRef(r)
                                | yuris_script::Insn::PushVarIndexed(r) => {
                                    base = Some((r.space.prefix(), r.id));
                                    st.push(0);
                                }
                                yuris_script::Insn::PushInt(n) => st.push(*n),
                                yuris_script::Insn::Binary(op) => {
                                    let (Some(rhs), Some(lhs)) = (st.pop(), st.pop()) else { break };
                                    st.push(match op {
                                        yuris_script::BinOp::Add => lhs.wrapping_add(rhs),
                                        yuris_script::BinOp::Sub => lhs.wrapping_sub(rhs),
                                        yuris_script::BinOp::Mul => lhs.wrapping_mul(rhs),
                                        _ => 0,
                                    });
                                }
                                yuris_script::Insn::ArrayLoad { .. } => break,
                                _ => break,
                            }
                        }
                        let (Some(expr), Some((_p, id))) = (st.last().copied(), base) else {
                            continue;
                        };
                        let Some(e) = ysvr.entries().iter().find(|e| e.var_id == id) else {
                            continue;
                        };
                        let Some(&b0) = e.bounds.first() else { continue };
                        if expr as u32 == b0 {
                            eq += 1;
                        } else if (expr + 1) as u32 == b0 {
                            plus1 += 1;
                            if examples.len() < 8 {
                                examples.push(format!("@{id} expr={expr} ysvr={b0}"));
                            }
                        } else {
                            other += 1;
                        }
                    }
                }
            }
            println!("表达式值 == YSVR bounds: {eq} 处");
            println!("表达式值+1 == YSVR bounds: {plus1} 处");
            println!("都不等: {other} 处");
            for e in &examples {
                println!("  例: {e}");
            }
            continue;
        }
        if let Some(hex) = spec.strip_prefix("--ysvr=") {
            // YSVR 表里该变量 id 的条目(边界/初值真值来源)
            let id = u16::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(0);
            let name = host_ysvr_entry(&pack, id);
            match name {
                Some(s) => println!("{s}"),
                None => println!("YSVR 无 @{id} 条目"),
            }
            continue;
        }
        let (space, hex) = match spec.chars().next() {
            Some('@') => (VarSpace::At, &spec[1..]),
            Some('$') => (VarSpace::Dollar, &spec[1..]),
            _ => (VarSpace::At, spec.as_str()),
        };
        let Ok(id) = u16::from_str_radix(hex.trim_start_matches("0x"), 16) else {
            println!("{spec}: 解析失败");
            continue;
        };
        let r = VarRef { space, id };
        match vm.store().array_dims(&r) {
            Some(d) => println!("{spec} → dims {d:?}"),
            None => match vm.store().get_opt(&r) {
                Some(v) => println!("{spec} → 标量 {v:?}"),
                None => println!("{spec} → 未声明"),
            },
        }
    }
}
