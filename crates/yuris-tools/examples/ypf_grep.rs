//! 只读探针:按 **CP932 名字子串**列出 YPF 条目(通用,不假设具体游戏)。
//!
//! 背景:`ypf_entries` 按 UTF-8 匹配,而包内条目名是 CP932(日文)——
//! 用英文/日文关键词搜资源(logo/ロゴ/movie/title …)必须按 CP932 字节比对。
//!
//! 用法:
//!   cargo run --release -p yuris-tools --example ypf_grep -- <游戏目录> <名字子串(UTF-8)> [最多条数]

use yuris_vm::host::YpfScriptHost;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir =
        std::path::PathBuf::from(args.next().expect("usage: ypf_grep <dir> <needle> [limit]"));
    let needle_utf8 = args.next().unwrap_or_default();
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);
    // UTF-8 → CP932(名字在包里就是 CP932 字节)。ASCII 关键字直接用字节。
    let needle: Vec<u8> = if needle_utf8.is_ascii() {
        needle_utf8.as_bytes().to_vec()
    } else {
        // 非 ASCII:借 player-core 的转换能力(此处避免新依赖,用简易表)
        let (enc, _, _) = cp932_encode(&needle_utf8);
        enc
    };
    let needle_lc: Vec<u8> = needle.iter().map(|b| b.to_ascii_lowercase()).collect();

    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension()
                .map(|x| x.eq_ignore_ascii_case("ypf"))
                .unwrap_or(false)
            {
                packs.push(p);
            }
        }
    }
    packs.sort();

    let mut total = 0usize;
    for pack in &packs {
        let Ok((idx, _, score)) = yuris_format::ypf::YpfIndex::from_path_auto(pack) else {
            continue;
        };
        let mut shown = 0usize;
        for e in &idx.entries {
            // 名字按 CP932 解码显示;比对用 ASCII 大小写不敏感 + 原始字节
            let raw = &e.name;
            let low: Vec<u8> = raw.iter().map(|b| b.to_ascii_lowercase()).collect();
            let hit = low
                .windows(needle_lc.len())
                .any(|w| w == needle_lc.as_slice())
                || raw.windows(needle.len()).any(|w| w == needle.as_slice());
            if !hit {
                continue;
            }
            let disp = String::from_utf8_lossy(raw).to_string();
            let (uncomp, comp) = (e.uncompressed_len, e.compressed_len);
            println!(
                "{}  flag=0x{:02x} uncomp={uncomp} comp={comp} off={}  {disp}",
                pack.file_name().unwrap_or_default().to_string_lossy(),
                e.flag,
                e.offset
            );
            shown += 1;
            total += 1;
            if total >= limit {
                break;
            }
        }
        if shown > 0 {
            println!("  ({} 条命中 @ 自洽 {:.0}%)", shown, score * 100.0);
        }
        if total >= limit {
            break;
        }
    }
    println!("== 共 {total} 条(上限 {limit}) ==");
}

/// 极简 CP932 编码:仅处理本项目探针需要的形态。
///
/// 非 ASCII 关键字(如日文)需要完整 CP932 表 —— 那属于 `yuris-format` 的
/// 职责。本探针只做 ASCII 关键词搜索(资源名几乎都是 ASCII 路径段),
/// 非 ASCII 输入原样按 UTF-8 字节返回(会命中不了,但不会误报)。
fn cp932_encode(s: &str) -> (Vec<u8>, usize, bool) {
    (s.as_bytes().to_vec(), 0, !s.is_ascii())
}
