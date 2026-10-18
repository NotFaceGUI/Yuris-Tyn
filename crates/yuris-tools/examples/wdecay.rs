//! 只读探针:全语料窗口解码失败统计 —— 按 (首个失败 op, 失败处前一条指令) 归类。
//! 用法: cargo run -p yuris-tools --release --example wdecay -- <游戏目录>
use std::collections::BTreeMap;
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: wdecay <dir>"));
    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) { packs.push(p); }
        }
    }
    packs.sort();
    let pack = packs.iter().find(|p| yuris_format::ypf::YpfIndex::from_path_auto(p)
        .map(|(idx,_,_)| idx.entries.iter().any(|e| is_script_entry(&e.name))).unwrap_or(false))
        .cloned().expect("脚本包");
    let mut host = YpfScriptHost::open(&pack).expect("open");
    let mut total = 0usize; let mut bad = 0usize;
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: BTreeMap<String, String> = BTreeMap::new();
    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        for (wi, w) in ctx.script.slots().iter().enumerate() {
            if w.len == 0 { continue }
            let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
            total += 1;
            if let Err(e) = yuris_script::decode_window(&b) {
                bad += 1;
                let m = e.to_string();
                let key = m.split("end=").next().unwrap_or(&m).to_string();
                *hist.entry(key.clone()).or_default() += 1;
                samples.entry(key.clone()).or_insert_with(|| {
                    format!("s{sid} w{wi} tag=0x{:x} len={} head={:02x?}", w.tag, w.len, &b[..b.len().min(12)])
                });
            }
        }
    }
    println!("窗口 {total} 个,解码失败 {bad} 个");
    for (k, v) in &hist {
        println!("  ×{v:<6} {k}   例: {}", samples.get(k).cloned().unwrap_or_default());
    }
}
