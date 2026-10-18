//! YSTB 结构自洽性体检:逐脚本验证加载器假设的**两条恒等式**
//! (来自 `docs/engine/command-layer.md` §1,样本 302/302 零失败):
//!
//! 1. `part1_len == 4 × G`(G = header[8] 的命令组数)
//! 2. `Σ(count_i × 12) == command_len`(窗口表恰好被组表用尽)
//!
//! 这两条任一失败 ⇒ **组表/窗口表对不齐**,后续「命令名、窗口数、窗口内容」
//! 全部是错位的 —— 表现出来的就是「明明叫 G_FLT 的组却长得像 IF」这类
//! 无法解释的形态。反之两条都过,则脚本解码层可以排除。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example ystb_check -- <游戏目录>

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: ystb_check <游戏目录>"));

    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
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
    let ids = host.script_ids().expect("脚本号列表");
    println!("脚本 {} 个", ids.len());

    let (mut ok1, mut bad1) = (0usize, 0usize);
    let (mut ok2, mut bad2) = (0usize, 0usize);
    let mut shown = 0usize;
    for sid in ids {
        let Ok(ctx) = host.load(sid) else { continue };
        // ctx.script 已解析;用其 header + 组表重算两条恒等式
        let g_count = ctx.groups.len();
        // 1) part1 长度 == 4G:解析器按 4 字节步进读组表,若不为 4 倍数它读不出整数倍
        //    —— 直接用它给出的组数 × 4 与自身比较是恒真的,故改为检查**窗口表是否用尽**。
        let sum_windows: usize =
            ctx.groups.iter().map(|g| g.window_count as usize).sum();
        let total_slots = ctx.script.slots().len();
        if sum_windows == total_slots {
            ok1 += 1;
        } else {
            bad1 += 1;
            if shown < 10 {
                println!(
                    "  窗口表不闭合 s{sid}: Σ窗口数 {sum_windows} != 槽位表 {total_slots} (组数 {g_count})"
                );
                shown += 1;
            }
        }
        // 2) 每个窗口的 offset+len 必须落在池内可解(池 = content‖part4)
        let mut overrun = 0usize;
        for (gi, g) in ctx.groups.iter().enumerate() {
            let first = ctx.first_slots[gi];
            for w in &ctx.script.slots()[first..first + g.window_count as usize] {
                if w.len == 0 {
                    continue;
                }
                if ctx.script.window_bytes_pooled_copy(w).is_none() {
                    overrun += 1;
                }
            }
        }
        if overrun == 0 {
            ok2 += 1;
        } else {
            bad2 += 1;
            if shown < 20 {
                println!("  窗口越界 s{sid}: {overrun} 个窗口读不出池内容");
                shown += 1;
            }
        }
    }
    println!("--- 窗口表闭合:{ok1} 通过 / {bad1} 失败 ---");
    println!("--- 窗口池可达:{ok2} 通过 / {bad2} 失败 ---");
}
