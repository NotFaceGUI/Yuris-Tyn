//! 临时取证:找连续 WORD 组并打印上下文(判定「追加缓冲」语义)。
use yuris_vm::host::{ScriptHost, YpfScriptHost};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let want_script: Option<u16> = a.get(2).and_then(|s| s.parse().ok());
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
    let mut shown = 0usize;
    for sid in host.script_ids().unwrap_or_default() {
        if let Some(w) = want_script { if sid != w { continue } }
        let Ok(ctx) = host.load(sid) else { continue };
        // 找 连续 >=3 个 WORD 的段
        let mut i = 0usize;
        while i < ctx.groups.len() {
            if name(ctx.groups[i].command_type) != "WORD" { i += 1; continue }
            let start = i;
            while i < ctx.groups.len() && name(ctx.groups[i].command_type) == "WORD" { i += 1 }
            let run = i - start;
            if run >= 3 && shown < 6 {
                out.push_str(&format!("=== s{sid} WORD run {start}..{} (len {run}) ===\n", i - 1));
                let lo = start.saturating_sub(6);
                let hi = (i + 6).min(ctx.groups.len());
                for j in lo..hi {
                    let g = ctx.groups[j];
                    let first = ctx.first_slots[j];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    let mut parts = Vec::new();
                    for w in wins {
                        let b = ctx.script.window_bytes_pooled_copy(w).unwrap_or_default();
                        let txt = yuris_scenario::decode_text(&b).unwrap_or_default();
                        parts.push(format!("B0={} len={} {:?}", w.tag & 0xff, b.len(),
                            txt.chars().take(50).collect::<String>()));
                    }
                    out.push_str(&format!("  g{j} {}(n={}) {}\n", name(g.command_type), g.window_count, parts.join(" | ")));
                }
                shown += 1;
            }
        }
    }
    let p = r"<side-repo>\workbench\tmp\dlg_probe.txt";
    std::fs::write(p, out).unwrap();
    println!("wrote {p} ({shown} runs)");
}
