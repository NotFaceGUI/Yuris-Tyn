//! 只读探针:按 **CP932 原始字节**在全部脚本窗口中检索字符串字面量。
//!
//! 与 `litgrep` 的差别:`litgrep` 把字面量按 lossy UTF-8 比较(对 CP932 文本
//! 会变成替换字符,必然漏检);本探针直接从命令行十六进制字节比对,适合定位
//! 「谁在画某段日文 UI」。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example litgrep_bytes -- <游戏目录> <十六进制,如 8ac28bab90dd92e8> [命中上限]

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: litgrep_bytes <dir> <hex> [limit]"));
    let hex: String = args.next().expect("十六进制字节");
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50);
    let needle: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
        .collect();
    assert!(!needle.is_empty(), "空 needle");

    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) {
                packs.push(p);
            }
        }
    }
    packs.sort();
    let pack = packs
        .iter()
        .find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        })
        .cloned()
        .expect("找不到脚本包");
    let mut host = YpfScriptHost::open(&pack).expect("打开脚本包");

    let ysc = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysc.ybn"))
        .and_then(|n| host.read_entry(&n).ok())
        .expect("ysc.ybn");
    let t = yuris_format::yscm::YscmTable::from_bytes(&ysc).expect("YSCM");

    let mut hits = 0usize;
    for sid in host.script_ids().expect("ids") {
        let Ok(ctx) = host.load(sid) else { continue };
        for (gi, g) in ctx.groups.iter().enumerate() {
            let name = t
                .commands()
                .get(g.command_type as usize)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("cmd{}", g.command_type));
            let first = ctx.first_slots[gi];
            for (wi, w) in ctx.script.slots()[first..first + g.window_count as usize]
                .iter()
                .enumerate()
            {
                let Some(b) = ctx.script.window_bytes_pooled_copy(w) else { continue };
                if !b.windows(needle.len()).any(|x| x == needle.as_slice()) {
                    continue;
                }
                let Ok(ins) = yuris_script::decode_window(&b) else { continue };
                for i in &ins {
                    if let yuris_script::Insn::PushStr(s) = &i.kind {
                        if s.windows(needle.len()).any(|x| x == needle.as_slice()) {
                            hits += 1;
                            if hits <= limit {
                                let cp932: Vec<u8> = s.clone();
                                println!(
                                    "s{sid} g{gi} {name} 槽{} 窗{wi}: bytes={} {:?}",
                                    w.tag & 0xff,
                                    cp932.len(),
                                    String::from_utf8_lossy(&cp932)
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    println!("== 命中 {hits} 处(上限 {limit})");
}
