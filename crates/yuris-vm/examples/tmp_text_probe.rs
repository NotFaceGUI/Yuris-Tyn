//! 临时取证:全语料定位 TEXT(idx96) 命令组 + 前后上下文,判定文本缓冲语义。
//! 用法: cargo run --release -p yuris-vm --example tmp_text_probe -- <游戏目录> [前后组数]
//! 输出: <tmp>/text_probe.txt
use yuris_vm::host::{ScriptHost, YpfScriptHost};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let pad: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(6);
    let pack = std::path::Path::new(dir).join("data.ypf");
    let mut host = YpfScriptHost::open(&pack).unwrap();
    let ysc_name = host
        .entry_names()
        .into_iter()
        .find(|n| n.contains("ysc.ybn"))
        .expect("ysc");
    let ysc_bytes = host.read_entry(&ysc_name).unwrap();
    let ysc = yuris_format::yscm::YscmTable::from_bytes(&ysc_bytes).ok();
    let name = |i: u8| -> String {
        ysc.as_ref()
            .and_then(|t| t.command_at(i as usize))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("cmd{i}"))
    };
    let text_idx = (0..=255u8).find(|i| name(*i) == "TEXT");
    let mut out = String::new();
    out.push_str(&format!("TEXT idx={text_idx:?}\n"));
    for sid in host.script_ids().unwrap_or_default() {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            if Some(g.command_type) != text_idx {
                continue;
            }
            out.push_str(&format!("=== s{sid} g{gi} TEXT n={} ===\n", g.window_count));
            let lo = gi.saturating_sub(pad);
            let hi = (gi + pad + 8).min(ctx.groups.len());
            for j in lo..hi {
                let gj = ctx.groups[j];
                let first = ctx.first_slots[j];
                let wins = &ctx.script.slots()[first..first + gj.window_count as usize];
                let mut parts = Vec::new();
                for w in wins {
                    let b = ctx.script.window_bytes_pooled_copy(w).unwrap_or_default();
                    let dec = match yuris_script::decode_window(&b) {
                        Ok(ins) => {
                            let s: Vec<String> = ins
                                .iter()
                                .map(|i| format!("{:?}", i.kind).chars().take(40).collect())
                                .collect();
                            s.join(";")
                        }
                        Err(_) => {
                            let txt = yuris_scenario::decode_text(&b).unwrap_or_default();
                            format!("raw {}B {:?}", b.len(), txt.chars().take(30).collect::<String>())
                        }
                    };
                    parts.push(format!("B0={} {}", w.tag & 0xff, dec));
                }
                out.push_str(&format!(
                    "  g{j} {}(n={}) {}\n",
                    name(gj.command_type),
                    gj.window_count,
                    parts.join(" | ")
                ));
            }
        }
    }
    let path = r"<side-repo>\workbench\tmp\text_probe.txt";
    std::fs::write(path, out).unwrap();
    println!("wrote {path}");
}
