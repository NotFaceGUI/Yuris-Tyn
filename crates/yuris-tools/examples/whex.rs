//! 只读探针:打印某脚本若干窗口的**原始字节**(hex),用于逐字节对齐核查。
//! 用法: cargo run -p yuris-tools --release --example whex -- <游戏目录> <脚本> <组> [窗数]
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: whex <dir> <script> <group> [n]"));
    let sid: u16 = a.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let g0: usize = a.next().and_then(|s| s.parse().ok()).expect("组号");
    let n: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut packs = Vec::new();
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
    let ctx = host.load(sid).expect("load");
    for gi in g0..g0 + n {
        let Some(g) = ctx.groups.get(gi) else { break };
        println!("== g{gi} cmd={} wc={} gparam={:#x}", g.command_type, g.window_count, g.param);
        let first = ctx.first_slots[gi];
        for (wi, w) in ctx.script.slots()[first..first + g.window_count as usize].iter().enumerate() {
            println!("   窗{wi} tag=0x{:x} off={} len={}", w.tag, w.offset, w.len);
            let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
            for (r, chunk) in b.chunks(16).enumerate() {
                let hx: Vec<String> = chunk.iter().map(|c| format!("{c:02x}")).collect();
                println!("      {:04x}  {}", r * 16, hx.join(" "));
            }
        }
    }
}
