//! 反汇编探针:按脚本号 + 组区间打印命令组的命令名与窗口字节。
//!
//! 用途:成功路径上"卡在某个 pc"时,看清那几组到底是什么
//! (循环体?等待?子程序?)。不猜版本:命令名来自游戏自身 YSCM。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example disasm -- <游戏目录> <脚本号> <起始组> [结束组]

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next().expect("usage: disasm <游戏目录> <脚本号> <起始组> [结束组]"),
    );
    let script_id: u16 = args.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let from: usize = args.next().and_then(|s| s.parse().ok()).expect("起始组");
    let to: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(from + 40);

    // 找脚本包
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
    // 命令名表:从 ysc.ybn 读(通用)
    let profile = load_profile(&mut host);
    let ctx = host.load(script_id).expect("加载脚本");
    println!(
        "=== s{script_id} 共 {} 组 (显示 [{from},{to})) ===",
        ctx.groups.len()
    );
    for gi in from..to.min(ctx.groups.len()) {
        let g = ctx.groups[gi];
        let name = profile
            .as_ref()
            .and_then(|p| p.name_of(g.command_type as u16))
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("cmd{:#04x}", g.command_type));
        let first = ctx.first_slots[gi];
        let wins = &ctx.script.slots()[first..first + g.window_count as usize];
        // 逐窗标出**槽名**(来自游戏自身 YSCM 的参数表)。槽号逐版本漂移,
        // 名字才是真值;有了它 `CG[TID="BTNTR" T=16]` 一眼可读。
        let pnames: Vec<String> = profile
            .as_ref()
            .map(|p| p.params_of(g.command_type as u16).to_vec())
            .unwrap_or_default();
        let hexes: Vec<String> = wins
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let b = ctx.script.window_bytes_pooled_copy(w).unwrap_or_default();
                let t = b.len().min(24);
                let sn = pnames
                    .get(i)
                    .map(|s| format!("{s}:"))
                    .unwrap_or_default();
                format!(
                    "{sn}t{:x}/l{}{}",
                    w.tag,
                    w.len,
                    b[..t]
                        .iter()
                        .map(|x| format!(" {x:02x}"))
                        .collect::<String>()
                )
            })
            .collect();
        // 原始 part1 4 字节(核验 param 位置;声明/调用类命令的维数位图)
        let raw: Vec<String> = ctx.script.part1()[gi * 4..gi * 4 + 4]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!(
            "g{gi:5} {name:16} w{} p{}(0x{:04x}) [{}] | {}",
            g.window_count,
            g.param,
            g.param,
            raw.join(" "),
            hexes.join("  ||  ")
        );
    }
}

/// 读游戏自身 ysc.ybn 建名字表(与 boot 同源;这里只要 name↔index)。
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
    Some(yuris_core::CommandProfile::from_names(
        names,
        version,
        name,
    ))
}
