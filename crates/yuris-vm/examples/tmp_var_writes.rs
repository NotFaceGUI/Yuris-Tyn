//! 临时取证:列出语料中对指定变量 id 的写入组(声明 + LET)。
use yuris_vm::host::ScriptHost;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let ids: Vec<u16> = a[2..].iter().filter_map(|s| s.parse().ok()).collect();
    let pack = std::path::Path::new(dir).join("data.ypf");
    let mut host = yuris_vm::host::YpfScriptHost::open(&pack).unwrap();
    let ysc_name = host.entry_names().into_iter().find(|n| n.contains("ysc.ybn")).expect("ysc");
    let ysc_bytes = host.read_entry(&ysc_name).unwrap();
    let ysc = yuris_format::yscm::YscmTable::from_bytes(&ysc_bytes).ok();
    let name = |i: u8| -> String {
        ysc.as_ref().and_then(|t| t.command_at(i as usize)).map(|c| c.name.clone())
            .unwrap_or_else(|| format!("cmd{i}"))
    };
    for sid in host.script_ids().unwrap_or_default() {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let first = ctx.first_slots[gi];
            let wins = &ctx.script.slots()[first..first + g.window_count as usize];
            let mut hit = false;
            for w in wins {
                let b = ctx.script.window_bytes_pooled_copy(w).unwrap_or_default();
                let Ok(ins) = yuris_script::decode_window(&b) else { continue };
                for i in &ins {
                    let s = format!("{:?}", i.kind);
                    for id in &ids {
                        if s.contains(&format!("id: {}", id)) || s.contains(&format!("id: {}", id+0)) {
                            hit = true;
                        }
                    }
                }
            }
            if hit {
                println!("s{sid} g{gi} {}", name(g.command_type));
            }
        }
    }
}
