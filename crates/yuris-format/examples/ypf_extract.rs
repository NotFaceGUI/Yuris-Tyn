//! 只读探针：按条目名从 YPF 封包中导出单个条目（用于音频/图像实测）。
//!
//! 用法：
//!   cargo run -p yuris-format --example ypf_extract -- <pack.ypf> <name-substring> <out-file>
//!
//! 名字按 **CP932** 解码后做子串匹配（大小写不敏感）；命中多个时报错并列出，
//! 避免静默取错条目。

use std::io::{Read, Seek, SeekFrom};

use yuris_format::ypf::YpfIndex;

fn main() {
    let mut args = std::env::args().skip(1);
    let pack = std::path::PathBuf::from(args.next().expect("usage: ypf_extract <pack> <substr> <out>"));
    let needle = args.next().expect("needle").to_lowercase();
    let out = std::path::PathBuf::from(args.next().expect("out"));

    let (idx, dialect, _score) = YpfIndex::from_path_auto(&pack).expect("解析 YPF 索引");
    eprintln!("[ypf_extract] dialect={dialect:?} entries={}", idx.entries.len());

    let mut hits = Vec::new();
    for e in &idx.entries {
        let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(&e.name);
        let display = decoded.to_string();
        let fallback = String::from_utf8_lossy(&e.name).to_string();
        if display.to_lowercase().contains(&needle) || fallback.to_lowercase().contains(&needle) {
            hits.push((e, display));
        }
    }
    if hits.is_empty() {
        eprintln!("没有条目匹配 {needle:?}");
        std::process::exit(2);
    }
    if hits.len() > 1 {
        eprintln!("命中 {} 条，请给更精确的名字：", hits.len());
        for (e, name) in hits.iter().take(20) {
            eprintln!("  {name} ({}B)", e.uncompressed_len);
        }
        std::process::exit(3);
    }

    let (entry, name) = hits.into_iter().next().unwrap();
    let mut f = std::fs::File::open(&pack).expect("open pack");
    f.seek(SeekFrom::Start(u64::from(entry.offset))).expect("seek");
    let mut raw = vec![0u8; entry.compressed_len as usize];
    f.read_exact(&mut raw).expect("read entry");
    let data = if raw.starts_with(&[0x78]) {
        let mut out = Vec::with_capacity(entry.uncompressed_len as usize);
        flate2::read::ZlibDecoder::new(&raw[..])
            .read_to_end(&mut out)
            .expect("zlib");
        out
    } else {
        raw
    };
    std::fs::write(&out, &data).expect("write out");
    println!(
        "extracted {name} ({}B, flag=0x{:02x}) -> {}",
        data.len(),
        entry.flag,
        out.display()
    );
}
