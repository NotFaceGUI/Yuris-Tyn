//! 只读探针:在**全部封包条目解压后**搜索 CP932 字节串(定位台词/按钮标签来源)。
//! 用法: cargo run --release -p yuris-tools --example cp932_sweep -- <游戏目录> <hex>
use yuris_vm::host::YpfScriptHost;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let hex = &a[2];
    let needle: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    let mut packs = Vec::new();
    for root in [std::path::PathBuf::from(dir), std::path::Path::new(dir).join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) {
                packs.push(p);
            }
        }
    }
    packs.sort();
    let mut total = 0usize;
    for pack in &packs {
        let Ok(mut host) = YpfScriptHost::open(pack) else {
            println!("[skip] {} (open failed)", pack.display());
            continue;
        };
        let names: Vec<String> = host.entry_names();
        for n in names {
            let Ok(d) = host.read_entry(&n) else { continue };
            let mut i = 0usize;
            let mut c = 0usize;
            while i + needle.len() <= d.len() {
                if d[i..i + needle.len()] == needle[..] {
                    c += 1;
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
            if c > 0 {
                println!("{} :: {} ×{c}", pack.display(), n);
                total += c;
            }
        }
    }
    println!("== 合计 {total} 处 ==");
}
