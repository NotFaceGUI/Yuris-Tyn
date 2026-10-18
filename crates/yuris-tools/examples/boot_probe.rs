//! 只读探针:检查一个游戏目录能否满足启动链 —— **通用**,不假设任何具体游戏。
//!
//! 回答三个问题:
//! 1. `name_key` 能否自动探测(逐游戏不同,不可硬编码)?
//! 2. 启动链所需条目(`ysc`/`ysl`/`ysv`/`yst` …)是否都在、能否解析?
//! 3. **索引游走是否对齐** —— 用「数据魔数反查」独立验证(名字看着对不代表
//!    字段对;只有 off 处真的躺着 PNG/OGG/YSTB/zlib 才说明对齐)。
//!
//! 用法:
//!   cargo run -p yuris-tools --example boot_probe -- <游戏目录> [name_key_hex]

use yuris_format::ypf::{YpfIndex, YpfReader};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: boot_probe <游戏目录> [key_hex]"));
    let _key_given = args
        .next()
        .map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).unwrap());

    // ---- 1. 找封包(游戏根 + pac/ 子目录)----
    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for d in [dir.clone(), dir.join("pac")] {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x.eq_ignore_ascii_case("ypf")).unwrap_or(false) {
                    packs.push(p);
                }
            }
        }
    }
    packs.sort();
    println!("=== 封包 {} 个 ===", packs.len());

    // ---- 2. 索引方言自动判别(通用: 不假设任何具体游戏的 key)----
    let mut chosen: Option<(std::path::PathBuf, String, YpfIndex)> = None;
    for p in &packs {
        let sz = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        match YpfIndex::detect_dialect(p) {
            Ok((d, sc)) => {
                let idx = match d {
                    yuris_format::ypf::IndexDialect::LenEncNot => YpfIndex::from_path_lenenc(p),
                    yuris_format::ypf::IndexDialect::NulXor(k) => YpfIndex::from_path(p, k),
                };
                if let Ok(idx) = idx {
                    println!(
                        "   [{d:?}] {} ({sz} B) → ypf v{} 结构自洽率 {:.1}%",
                        p.file_name().unwrap().to_string_lossy(),
                        idx.header.version,
                        sc * 100.0
                    );
                    if chosen.is_none() {
                        chosen = Some((p.clone(), format!("{d:?}"), idx));
                    }
                }
            }
            Err(e) => println!("   x {} ({sz} B): {e}", p.file_name().unwrap().to_string_lossy()),
        }
    }

    let Some((pack, dialect, idx)) = chosen else {
        println!("!! 没有可解析的封包");
        return;
    };
    println!("\n=== 采用 {} (方言 {dialect}) ===", pack.display());

    let mut r = YpfReader::from_index(&pack, idx).expect("open");
    let names: Vec<String> = r
        .index()
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.name).to_string())
        .collect();

    // ---- 3. 启动链所需条目 ----
    for want in [
        "ysbin\\ysc.ybn",
        "ysbin\\ysl.ybn",
        "ysbin\\ysv.ybn",
        "ysbin\\yst.ybn",
        "ysbin\\yse.ybn",
        "ysbin\\yscfg.ybn",
    ] {
        let hit = names
            .iter()
            .find(|n| n.to_ascii_lowercase().replace('/', "\\").ends_with(want));
        match hit {
            Some(n) => {
                let blob = r.read(n.as_bytes()).expect("read");
                println!("  ok  {want:<20} <- {n:?}  {} B", blob.len());
            }
            None => println!("  --  {want}  缺失"),
        }
    }

    // ---- 4. 索引对齐实测(数据魔数反查)----
    {
        use std::io::{Read, Seek, SeekFrom};
        const MAGS: [&[u8]; 6] = [
            &[0x89, b'P', b'N', b'G'],
            b"OggS",
            b"RIFF",
            b"YSTB",
            &[0xFF, 0xD8, 0xFF],
            b"BM",
        ];
        let mut f = std::fs::File::open(&pack).expect("open pack");
        let flen = std::fs::metadata(&pack).map(|m| m.len()).unwrap_or(0);
        let (mut ok, mut bad_off, mut bad_magic) = (0usize, 0usize, 0usize);
        let mut samples: Vec<String> = Vec::new();
        for e in r.index().entries.iter() {
            let (o, c) = (e.offset as u64, e.compressed_len as u64);
            if o == 0 || o + c > flen {
                bad_off += 1;
                continue;
            }
            let mut h = [0u8; 4];
            if f.seek(SeekFrom::Start(o)).is_err() || f.read_exact(&mut h).is_err() {
                bad_off += 1;
                continue;
            }
            let zlib = h[0] == 0x78 && matches!(h[1], 0x01 | 0x9c | 0xda);
            if zlib || MAGS.iter().any(|m| h.starts_with(m)) {
                ok += 1;
            } else {
                bad_magic += 1;
                if samples.len() < 6 {
                    samples.push(format!(
                        "{:?} magic={:02x?} uncomp={} comp={} off={:#x} flag={:#x}",
                        String::from_utf8_lossy(&e.name),
                        h,
                        e.uncompressed_len,
                        e.compressed_len,
                        e.offset,
                        e.flag
                    ));
                }
            }
        }
        let total = ok + bad_off + bad_magic;
        println!("\n=== 索引对齐实测(数据魔数反查) ===");
        println!(
            "  对齐 {ok} / off 越界 {bad_off} / 魔数不符 {bad_magic}  共 {total}  ({:.0}% 对齐)",
            ok as f32 * 100.0 / total.max(1) as f32
        );
        for s in &samples {
            println!("   x {s}");
        }
    }

    // ---- 5. 脚本与 YSTB 密钥 ----
    let scripts: Vec<String> = names
        .iter()
        .filter(|n| {
            let norm = n.replace('/', "\\");
            norm.contains("ysbin\\yst") && norm.ends_with(".ybn")
        })
        .cloned()
        .collect();
    println!("\n  脚本条目 {} 个", scripts.len());
    if let Some(first) = scripts.first() {
        let blob = r.read(first.as_bytes()).expect("read script");
        println!("  首脚本 {first:?}  {} B  魔数 {:?}", blob.len(), &blob[..4.min(blob.len())]);
        match yuris_format::ystb::guess_key(&blob) {
            Ok((k, rate)) => println!("  YSTB 密钥猜测 = {k:02x?} (把握 {rate:.2})"),
            Err(e) => println!("  YSTB 密钥猜测失败: {e}"),
        }
    }

    // ---- 6. 条目面概览 ----
    let txts: Vec<&String> = names.iter().filter(|n| n.ends_with(".txt")).collect();
    println!("\n  .txt 条目 {} 个(剧本播放器的输入)", txts.len());
    let mut exts: std::collections::BTreeMap<String, usize> = Default::default();
    for n in &names {
        let e = n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        *exts.entry(e).or_default() += 1;
    }
    let mut v: Vec<_> = exts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    println!("  扩展名分布(前 10):");
    for (e, c) in v.iter().take(10) {
        println!("     .{e:<10} {c}");
    }
}
