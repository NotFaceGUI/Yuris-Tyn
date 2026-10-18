//! 只读探针:解码**散装** `ysbin\ystNNNNN.ybn`(不走封包)并列出其标签。
//!
//! 背景(成果 133):发行版把部分脚本作为**松散文件**放在 `<游戏>\ysbin\`
//! (补丁更新版),引擎的 VFS 按「松散文件优先于封包」解析 —— 与包内同名脚本
//! 的字节数不同(实测 yst00123:散装 1,125,871 B vs 包内 945,959 B)。
//! 内核旧实现只读封包 ⇒ 用的是**旧版脚本**,标签表里缺新版新增的标签
//! (实测 `SCENARIO_LOGO` 只在散装版里)。
//!
//! 用法:
//!   cargo run --release -p yuris-tools --example loose_ystb -- <游戏目录> <脚本号…> [--grep 子串]

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: loose_ystb <dir> <id…> [--grep s]"));
    let mut ids: Vec<u16> = Vec::new();
    let mut needle: Option<String> = None;
    let mut it = args.peekable();
    while let Some(a) = it.next() {
        if a == "--grep" {
            needle = it.next();
        } else if let Ok(n) = a.parse::<u16>() {
            ids.push(n);
        }
    }
    // 脚本密钥:借封包内首个脚本条目的 guess-key(散装与包内同密钥)
    let mut key = [0x2b, 0x90, 0x4f, 0x93];
    {
        use yuris_vm::host::{is_script_entry, YpfScriptHost};
        let mut packs: Vec<PathBuf> = Vec::new();
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
        if let Some(pack) = packs.iter().find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        }) {
            if let Ok(host) = YpfScriptHost::open(pack) {
                key = host.script_key();
            }
        }
    }
    eprintln!("[loose] 脚本密钥 {key:02x?}");

    for id in ids {
        let p = dir.join("ysbin").join(format!("yst{id:05}.ybn"));
        let Ok(bytes) = std::fs::read(&p) else {
            println!("s{id}: 散装不存在 ({})", p.display());
            continue;
        };
        println!("s{id}: 散装 {} 字节", bytes.len());
        match yuris_format::ystb::YstbFile::from_bytes(&bytes, key) {
            Ok(f) => {
                let groups_vec = f.groups().unwrap_or_default();
                println!("     组数 {}", groups_vec.len());
                // 标签不在 YSTB（在 YSLB）。这里直接扫**已解密的池字节**：
                // 散装版比包内版多出来的字符串 = 补丁新增内容。
                let Some(nd) = &needle else {
                    println!("     (给 --grep <子串> 可搜池内字符串)");
                    continue;
                };
                let nsc = nd.to_ascii_uppercase();
                let firsts = f.group_first_slots(&groups_vec);
                let mut hit = 0usize;
                for (gi, g) in groups_vec.iter().enumerate() {
                    let first = firsts[gi];
                    for w in &f.slots()[first..first + g.window_count as usize] {
                        let Some(b) = f.window_bytes_pooled_copy(w) else { continue };
                        let hay = String::from_utf8_lossy(&b).to_ascii_uppercase();
                        if hay.contains(&nsc) {
                            let txt: String = b
                                .iter()
                                .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
                                .collect();
                            println!("       g{gi} 槽{}: {txt}", w.tag & 0xff);
                            hit += 1;
                            if hit > 30 {
                                println!("       …(截断)");
                                break;
                            }
                        }
                    }
                }
                println!("     命中 {hit} 处");
            }
            Err(e) => println!("     解析失败: {e}"),
        }
    }
}
