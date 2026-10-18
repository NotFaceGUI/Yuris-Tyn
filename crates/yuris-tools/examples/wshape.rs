//! 命令的**窗口形态普查**:对每个命令名统计 组数 / 窗口数分布 / 窗口(tag,len)形态。
//!
//! 用法:`wshape <游戏目录> [命令名…]`
//!
//! 用途:在语义未取证时,用「同名命令在全语料里的窗口形态是否一致」来判定
//! 该命令是**无参**、**有参**还是**带空窗口标记**——这是源码层
//! (`return[]` vs `return`)与字节码层对应关系的直接证据。

use std::collections::BTreeMap;
use std::path::PathBuf;

use yuris_vm::host::{ScriptHost, YpfScriptHost, is_script_entry};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: wshape <游戏目录> [命令名…]"));
    let want: Vec<String> = args.map(|s| s.to_ascii_uppercase()).collect();

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

    // 命令名表:从游戏自身 ysc.ybn 读(逐版本漂移,不硬编码)。
    let ysc = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok())
        .expect("ysc.ybn");
    let t = yuris_format::yscm::YscmTable::from_bytes(&ysc).expect("解析 YSCM");

    let mut groups: BTreeMap<String, u64> = BTreeMap::new();
    let mut wc_hist: BTreeMap<(String, u8), u64> = BTreeMap::new();
    let mut shape: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut tag_hist: BTreeMap<(String, u32), u64> = BTreeMap::new();

    let ids = host.script_ids().expect("script ids");
    for sid in ids {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = t
                .commands()
                .get(g.command_type as usize)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("<idx{}>", g.command_type));
            if !want.is_empty() && !want.iter().any(|w| w == &name) {
                continue;
            }
            *groups.entry(name.clone()).or_default() += 1;
            *wc_hist.entry((name.clone(), g.window_count)).or_default() += 1;
            let first = ctx.first_slots[gi];
            let mut desc = String::new();
            for w in &ctx.script.slots()[first..first + g.window_count as usize] {
                desc.push_str(&format!("[tag=0x{:x} len={}]", w.tag, w.len));
                if g.window_count == 1 && w.len == 0 {
                    *tag_hist.entry((name.clone(), w.tag)).or_default() += 1;
                }
            }
            if desc.is_empty() {
                desc.push_str("(无窗口)");
            }
            *shape.entry((name.clone(), desc)).or_default() += 1;
        }
    }

    for (name, n) in groups.iter() {
        println!("== {name}  组数={n}");
        let mut wh: Vec<_> = wc_hist.iter().filter(|((nm, _), _)| nm == name).collect();
        wh.sort_by_key(|((_, wc), _)| *wc);
        for ((_, wc), c) in wh {
            println!("   wc={wc}: {c}");
        }
        let mut shapes: Vec<_> = shape.iter().filter(|((nm, _), _)| nm == name).collect();
        shapes.sort_by(|a, b| b.1.cmp(a.1));
        for ((_, s), c) in shapes.iter().take(8) {
            println!("     形态×{c}: {s}");
        }
        let mut th: Vec<_> = tag_hist.iter().filter(|((nm, _), _)| nm == name).collect();
        th.sort_by_key(|((_, t), _)| *t);
        if !th.is_empty() {
            let max = th.iter().map(|((_, t), _)| *t).max().unwrap();
            println!("     空窗 tag 直方图(共 {} 种, max=0x{max:x}):", th.len());
            for ((_, t), c) in th.iter().take(40) {
                print!("0x{t:x}:{c} ");
            }
            println!();
        }
    }
}
