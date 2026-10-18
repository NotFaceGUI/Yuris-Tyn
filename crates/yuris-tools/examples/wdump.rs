//! 单组窗口全解探针:打印某组每个窗口的完整指令序列(真解码)。
//!
//! 用途:字节/标签推断不可靠时取**精确**的窗口内容 —— 例如判定
//! 「声明组的窗 0 到底是什么」(左值?表达式?多变量列表?)。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example wdump -- <游戏目录> <脚本号> <组号> [组数]

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: wdump <dir> <script> <group>"));
    let sid: u16 = args.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let g0: usize = args.next().and_then(|s| s.parse().ok()).expect("组号");
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);

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

    // 命令名表(游戏自身 YSCM)
    let blob = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with("ysbin\\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok())
        .expect("ysc.ybn");
    let t = yuris_format::yscm::YscmTable::from_bytes(&blob).expect("解析 YSCM");
    let name_of = |i: u16| t.commands().get(i as usize).map(|c| c.name.clone());

    let ctx = host.load(sid).expect("载入脚本");
    println!("s{sid} 共 {} 组", ctx.groups.len());
    for gi in g0..(g0 + n).min(ctx.groups.len()) {
        let g = &ctx.groups[gi];
        let first = ctx.first_slots[gi];
        let wins = &ctx.script.slots()[first..first + g.window_count as usize];
        println!(
            "== g{gi} cmd={} ({}) wc={} gparam=0x{:04x} part1=[{}]",
            g.command_type,
            name_of(g.command_type as u16).unwrap_or_else(|| "?".into()),
            g.window_count,
            g.param,
            "" 
        );
        for (wi, w) in wins.iter().enumerate() {
            println!("  窗{wi} tag=0x{:x} B0={} offset={} len={}", w.tag, w.tag & 0xff, w.offset, w.len);
            let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                println!("    <池化读取失败>");
                continue;
            };
            println!("    原始: {}", b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
            match yuris_script::decode_window(&b) {
                Ok(instrs) => {
                    for (ii, ins) in instrs.iter().enumerate() {
                        println!("    [{ii:2}] {:?}", ins.kind);
                    }
                }
                Err(e) => println!("    解码失败: {e}"),
            }
        }
    }
}
