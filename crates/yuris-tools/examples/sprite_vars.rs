//! 只读探针:**精灵/效果子系统的状态模型取证**(成果 137 前置)。
//!
//! ## 为什么需要它
//!
//! 本作全部视觉走 `MAC.* → es.SP.* → es.EF.TASK.MAIN → CG/CGACT` 这条链。
//! 实测:`s66`(效果脚本)四秒执行 4 万余组、`es.SP.CG.SET` 区跑过、
//! `es.SP.DRAW.POST` 也跑过,但 **`es.SP.DRAW` 执行 0 次** ——
//! 它的两个调用点(pc954/pc957)也 0 次,而外层循环转了约 200 次。
//! 结论只能有一个:**精灵队列恒空**。
//!
//! 这是**状态模型**问题,不是"缺一条命令":脚本读写一套队列变量,
//! 我们没有建模 ⇒ 控制流照跑、但一个精灵都不产出。
//!
//! ## 本探针做什么
//!
//! 静态分析 `es.EF.TASK.MAIN` 与 `es.SP.*` 函数体,按**读写模式**把
//! 涉及的变量 id 分类:
//!   - 只被写一次、多处读 → **队列容量 / 游标**(状态标量)
//!   - 被 `aload` 带下标读写 → **队列数组**(元素槽)
//!   - 递增/递减形态 → **计数器**
//!
//! 输出:变量 id → (读次数, 写次数, 是否带下标, 出现于哪些 pc)。
//! 这是"队列长什么样"的**静态证据**;下一步再按它建模。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example sprite_vars -- <游戏目录>

use std::collections::BTreeMap;

use yuris_format::ystb::{CommandGroup, CommandSlot, YstbFile};
use yuris_vm::host::{ScriptHost, YpfScriptHost};

/// 变量引用:`(空间前缀, id, 是否带下标)`。
type VarRef = (u8, u16, bool);

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: sprite_vars <dir>"));

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
            let Ok(h) = YpfScriptHost::open(p) else { return false };
            h.entry_names()
                .iter()
                .any(|e| e.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
        })
        .cloned()
        .expect("脚本包");

    let mut host = YpfScriptHost::open(&pack).expect("打开包");

    // ---- 1) 从 YSLB 找 es.* 相关脚本号与 pc 区间 ----
    let yslb = {
        let name = host
            .entry_names()
            .into_iter()
            .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
            .expect("ysl.ybn");
        let d = host.read_entry(&name).expect("读 ysl.ybn");
        yuris_format::yslb::YslbTable::from_bytes(&d).expect("解析 YSLB")
    };
    // 目标标签
    let want: Vec<(&str, &str)> = vec![
        ("es.EF.TASK", "效果任务入口"),
        ("es.EF.TASK.MAIN", "效果主循环"),
        ("es.SP.DRAW", "精灵绘制"),
        ("es.SP.CG.SET", "精灵 CG 设置"),
        ("MAC.EV", "MAC.EV 门面"),
    ];
    let mut targets: Vec<(String, u16, u32)> = Vec::new();
    for (label, desc) in &want {
        if let Some(i) = yslb.find(label.as_bytes()) {
            let l = &yslb.labels()[i];
            println!("{label:20} = s{} pc{}   ({desc})", l.script_id, l.target_pc);
            targets.push((label.to_string(), l.script_id, l.target_pc));
        }
    }

    // ---- 2) 逐目标:载脚本、扫描指定区间内的组,统计变量读写 ----
    for (label, sid, pc) in &targets {
        let Ok(ctx) = host.load(*sid) else { continue };
        println!("\n===== {label} (s{sid} pc{pc})=====");
        // 扫描 pc 起 700 组(或到脚本尾)—— 函数体的近似边界。
        let from = *pc as usize;
        let ngroups = ctx.script.groups().map(|g| g.len()).unwrap_or(0);
        let to = (from + 700).min(ngroups);
        let stats = scan(&ctx.script, from, to);
        report(&stats);
    }
}

/// 变量读写统计。
#[derive(Default, Clone, Copy)]
struct Stat {
    reads: u32,
    writes: u32,
    indexed_reads: u32,
    indexed_writes: u32,
}

fn scan(f: &YstbFile, from: usize, to: usize) -> BTreeMap<VarRef, Stat> {
    let mut m: BTreeMap<VarRef, Stat> = BTreeMap::new();
    let Ok(groups) = f.groups() else { return m };
    let slots = f.slots();
    let mut first_idx = 0usize;
    for gi in 0..groups.len() {
        if gi < from {
            first_idx += groups[gi].window_count as usize;
            continue;
        }
        if gi >= to {
            break;
        }
        let g: CommandGroup = groups[gi];
        for wi in 0..g.window_count as usize {
            let Some(w): Option<&CommandSlot> = slots.get(first_idx + wi) else { continue };
            let Some(b) = f.window_bytes_pooled_copy(w) else { continue };
            collect(&b, &mut m);
        }
        first_idx += g.window_count as usize;
    }
    m
}

/// 在表达式字节里找变量引用形态。
///
/// **编码结构**(`yuris-script/src/insn.rs` 的解码器为权威):
/// ```text
/// [opcode:u8][operand_len:u16 LE][operand…]
/// ```
/// 变量族(opcode `0x48` 取值 / `0x56` 引用 / `0x76` 下标形式)的
/// operand = `[space:u8][id:u16 LE]`(共 3 字节)。
/// 下标形态:先压下标值、再压引用、最后 `0x29`(ArrayLoad)—— 因此
/// `0x29` 出现在引用**之后**(见 `yuris-script/src/eval.rs` 的槽位模型)。
/// 本扫描按同一结构走,不做完整译码 —— 足够把队列变量筛出来。
fn collect(b: &[u8], m: &mut BTreeMap<VarRef, Stat>) {
    let mut i = 0usize;
    while i + 5 < b.len() {
        let op = b[i];
        let is_read = op == 0x48;
        let is_ref = op == 0x56 || op == 0x76;
        if is_read || is_ref {
            // 长度字段必须为 3(变量操作数恒 3 字节)—— 不符则不是变量指令
            let len = u16::from_le_bytes([b[i + 1], b[i + 2]]);
            if len == 3 {
                let space = b[i + 3];
                let id = u16::from_le_bytes([b[i + 4], b[i + 5]]);
                // 紧随其后的指令是不是 0x29(ArrayLoad)= 带下标
                let indexed = b.get(i + 6) == Some(&0x29);
                let e = m.entry((space, id, indexed)).or_default();
                if is_read {
                    if indexed {
                        e.indexed_reads += 1;
                    } else {
                        e.reads += 1;
                    }
                } else if indexed {
                    e.indexed_writes += 1;
                } else {
                    e.writes += 1;
                }
                i += 6;
                continue;
            }
        }
        i += 1;
    }
}

fn report(m: &BTreeMap<VarRef, Stat>) {
    // 按"总引用数"排序;带下标的排前(数组 = 队列的可能形态)
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by(|a, b| {
        let ta = a.1.indexed_reads + a.1.indexed_writes;
        let tb = b.1.indexed_reads + b.1.indexed_writes;
        tb.cmp(&ta).then_with(|| {
            let sa = a.1.reads + a.1.writes;
            let sb = b.1.reads + b.1.writes;
            sb.cmp(&sa)
        })
    });
    println!("  {:<6} {:>8} {:>8} {:>9} {:>9}   形态", "空间", "读", "写", "带下标读", "带下标写");
    for ((pfx, id, idx), st) in v.iter().take(28) {
        let space = match pfx {
            b'@' => "@",
            b'$' => "$",
            b'`' => "`",
            b'#' => "#",
            _ => "?",
        };
        let shape = if *idx && (st.indexed_writes > 0 || st.indexed_reads > 0) {
            "**数组(疑似队列槽)**"
        } else if st.writes == 1 && st.reads > 3 {
            "只写一次多处读(疑似容量/游标)"
        } else if st.reads == 0 && st.writes > 0 {
            "只写"
        } else {
            ""
        };
        println!(
            "  {space}{id:<5} {:>8} {:>8} {:>9} {:>9}   {shape}",
            st.reads, st.writes, st.indexed_reads, st.indexed_writes
        );
    }
}
