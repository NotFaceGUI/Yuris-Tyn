//! 只读探针:按池偏移打印原始字节(核查窗口是否只是更大 blob 的子切片)。
//! 用法: cargo run -p yuris-tools --release --example poolhex -- <游戏目录> <脚本> <offset> <len>
use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};
fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: poolhex <dir> <script> <off> <len>"));
    let sid: u16 = a.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let off: usize = a.next().and_then(|s| s.parse().ok()).expect("offset");
    let len: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(512);
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
    // 找覆盖该偏移的窗口及其后继窗口,确认池布局
    let mut wins: Vec<(usize, usize, u32)> = ctx.script.slots().iter()
        .map(|w| (w.offset as usize, w.len as usize, w.tag)).collect();
    wins.sort();
    let containing: Vec<_> = wins.iter().filter(|(o,l,_)| *o <= off && off < o + l || (*o >= off && *o < off + len)).collect();
    println!("含该区间的窗口(前 8): {:?}", &containing[..containing.len().min(8)]);
    let (c, p4) = ctx.script.pool();
    println!("content={} part4={} 总={}", c.len(), p4.len(), c.len() + p4.len());
    let end = (off + len).min(c.len() + p4.len());
    let mut buf: Vec<u8> = Vec::new();
    if off < c.len() {
        buf.extend_from_slice(&c[off..c.len().min(end)]);
    }
    if end > c.len() {
        buf.extend_from_slice(&p4[..end - c.len()]);
    }
    for (r, chunk) in buf.chunks(16).enumerate() {
        let hx: Vec<String> = chunk.iter().map(|x| format!("{x:02x}")).collect();
        let mark = if off + r * 16 <= c.len() && c.len() < off + r * 16 + 16 { " <== content/part4 边界" } else { "" };
        println!("  {:06x}  {}{}", off + r * 16, hx.join(" "), mark);
    }
}
