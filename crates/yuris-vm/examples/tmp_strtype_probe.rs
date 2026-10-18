//! 临时取证:全语料 VARINFO STRTYPE 调用的槽形态。
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
            if name(g.command_type) != "VARINFO" { continue; }
            let first = ctx.first_slots[gi];
            let wins = &ctx.script.slots()[first..first + g.window_count as usize];
            // 只看 STRTYPE 槽(3)非空的组
            let has = wins.iter().any(|w| (w.tag & 0xff) == 3 && w.len > 0);
            if !has { continue; }
            let mut parts = Vec::new();
            for w in wins {
                let b = ctx.script.window_bytes_pooled_copy(w).unwrap_or_default();
                let d = match yuris_script::decode_window(&b) {
                    Ok(ins) => ins.iter().map(|i| format!("{:?}", i.kind)).collect::<Vec<_>>().join(";"),
                    Err(_) => format!("raw{}B", b.len()),
                };
                parts.push(format!("slot{} {}", w.tag & 0xff, d));
            }
            out.push_str(&format!("s{sid} g{gi} :: {}\n", parts.join(" | ")));
            n += 1;
        }
    }
    out.push_str(&format!("== {n} 处 ==\n"));
    let p = r"<side-repo>\workbench\tmp\strtype_probe.txt";
    std::fs::write(p, out).unwrap();
    println!("wrote {p} ({n})");
}
