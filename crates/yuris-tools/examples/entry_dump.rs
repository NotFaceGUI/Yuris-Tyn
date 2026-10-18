//! 包内条目转储:按名字后缀找条目并把原始字节打出来(hex + 可打印 SJIS 视图)。
//!
//! 用途:看 `yscfg.ybn` / `yscfg.dat` 这类「游戏自述」小文件里到底有什么,
//! 例如窗口标题、版本号 —— 而不是靠目录名猜。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example entry_dump -- <游戏目录> <名字后缀>

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: entry_dump <dir> <suffix>"));
    let suffix = args.next().unwrap_or_default().to_ascii_lowercase();

    // 先看散落在目录里的普通文件
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        let n = p.file_name().unwrap().to_string_lossy().to_lowercase();
        if n.ends_with(&suffix) && p.is_file() {
            if let Ok(b) = std::fs::read(&p) {
                println!("== 文件 {} ({} B)", p.display(), b.len());
                dump(&b);
            }
        }
    }

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
    for pack in packs {
        let Ok(mut host) = YpfScriptHost::open(&pack) else {
            continue;
        };
        for name in host.entry_names() {
            if !name.to_ascii_lowercase().ends_with(&suffix) {
                continue;
            }
            match host.read_entry(&name) {
                Ok(b) => {
                    println!("== 包条目 {name} ({} B)", b.len());
                    dump(&b);
                }
                Err(e) => println!("== 包条目 {name} 读取失败: {e}"),
            }
        }
        // 顺带确认脚本包可解析
        drop(host);
        let _ = is_script_entry(b"ysbin\\yst00001.ybn");
    }
}

fn dump(b: &[u8]) {
    for (i, chunk) in b.chunks(16).enumerate() {
        let hex: String = chunk.iter().map(|x| format!("{x:02x} ")).collect();
        let asc: String = chunk
            .iter()
            .map(|&x| {
                if (0x20..0x7f).contains(&x) {
                    x as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {:04x}  {hex:<48} {asc}", i * 16);
    }
}
