//! 临时取证:全语料 FONT(idx27)/FONTINFO 调用 —— 打印每槽的求值摘要,
//! 判定「字号真值」在哪一槽(引擎 FONT 无显式 size 参数,疑 SX/SY)。
use yuris_vm::host::{ScriptHost, YpfScriptHost};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let pack = std::path::Path::new(dir).join("data.ypf");
    let mut host = YpfScriptHost::open(&pack).unwrap();
    let ysc_name = host.entry_names().into_iter().find(|n| n.contains("ysc.ybn")).expect("ysc");
    let ysc_bytes = host.read_entry(&ysc_name).unwrap();
    let ysc = yuris_format::yscm::YscmTable::from_bytes(&ysc_bytes).ok();
    let name = |i: u8| -> String {
        ysc.as_ref().and_then(|t| t.command_at(i as usize)).map(|c| c.name.clone())
            .unwrap_or_else(|| format!("cmd{i}"))
    };
    let mut out = String::new();
    let mut n = 0usize;
    for sid in host.script_ids().unwrap_or_default() {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let cn = name(g.command_type);
            if cn != "FONT" && cn != "FONTINFO" { continue; }
            let first = ctx.first_slots[gi];
            let wins = &ctx.script.slots()[first..first + g.window_count as usize];
            let mut parts = Vec::new();
            for w in wins {
                let b = ctx.script.window_bytes_pooled_copy(w).unwrap_or_default();
                let d = match yuris_script::decode_window(&b) {
                    Ok(ins) => ins.iter().map(|i| format!("{:?}", i.kind).chars().take(50).collect::<String>())
                        .collect::<Vec<_>>().join(";"),
                    Err(_) => format!("raw{}B", b.len()),
                };
                parts.push(format!("B0={} {}", w.tag & 0xff, d));
            }
            out.push_str(&format!("s{sid} g{gi} {cn} n={} :: {}\n", g.window_count, parts.join(" | ")));
            n += 1;
        }
    }
    out.push_str(&format!("== {n} 处 ==\n"));
    let p = r"<side-repo>\workbench\tmp\font_probe.txt";
    std::fs::write(p, out).unwrap();
    println!("wrote {p} ({n})");
}
