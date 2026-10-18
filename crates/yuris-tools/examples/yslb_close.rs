//! 只读探针:**YSLB 解析闭合性校验** —— 标签表到底读全了没有。
//!
//! 用途(成果 135):`SCENARIO_LOGO` 在 YSLB 里查不到,而官方 SDK 源码
//! (`ERIS/data/script/UserScript/SCENARIO_START.txt`)明确有 `#SCENARIO_LOGO`
//! 标签 —— 二者矛盾,必须判定是**解析器读到一半停了**(截断)还是
//! **发行数据真的没有**(游戏自己的问题)。
//!
//! 判据:`YslbTable::consumed`(解析器读完标签流的偏移)
//!      == 文件长度 ⇒ 全量闭合;不等 ⇒ **解析器截断**,是内核缺陷。
//!
//! 同时打印:标签总数、各脚本号的标签数(看 s142 有几个)、按 pc 排序的 s142 标签。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example yslb_close -- <游戏目录> [脚本号]

use yuris_format::yslb::YslbTable;
use yuris_vm::host::{ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: yslb_close <dir> [script_id]"));
    let want_sid: Option<u16> = args.next().and_then(|s| s.parse().ok());

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
            let n = p.file_name().map(|s| s.to_string_lossy().to_ascii_lowercase());
            // 脚本包 = 含 ysbin\ysl.ybn 的那个(逐包试开)。
            let Ok(mut h) = YpfScriptHost::open(p) else { return false };
            let _ = &n;
            h.entry_names()
                .iter()
                .any(|e| e.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
        })
        .cloned()
        .expect("找不到含 ysl.ybn 的包");

    let mut host = YpfScriptHost::open(&pack).expect("打开包");
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(r"ysbin\ysl.ybn"))
        .expect("ysl.ybn 条目");
    let data = host.read_entry(&name).expect("读 ysl.ybn");
    let t = YslbTable::from_bytes(&data).expect("解析 YSLB");

    println!("ysl.ybn 文件 {} 字节;解析器 consumed = {}", data.len(), t.consumed);
    if t.consumed == data.len() {
        println!("⇒ **全量闭合**:标签流读到文件尾,无截断。");
    } else {
        println!(
            "⇒ **解析器截断!** 剩余 {} 字节未消费 —— 这是内核缺陷。",
            data.len() as i64 - t.consumed as i64
        );
        let a = t.consumed.min(data.len());
        let b = (a + 64).min(data.len());
        let ctx: String = data[a..b]
            .iter()
            .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
            .collect();
        println!("   未消费段前 64 字节: {ctx}");
    }
    println!("标签总数 {}", t.labels().len());

    // 每个脚本号的标签数
    let mut per: std::collections::BTreeMap<u16, usize> = std::collections::BTreeMap::new();
    for l in t.labels() {
        *per.entry(l.script_id).or_default() += 1;
    }
    println!("按脚本号统计(仅列 s140..s160):");
    for (sid, n) in per.iter().filter(|(s, _)| (140u16..=160).contains(s)) {
        println!("   s{sid}: {n} 条");
    }

    // 指定脚本的标签明细(按 pc 排序)
    if let Some(sid) = want_sid {
        let mut v: Vec<_> = t.labels().iter().filter(|l| l.script_id == sid).collect();
        v.sort_by_key(|l| l.target_pc);
        println!("s{sid} 的标签明细({} 条,按 pc):", v.len());
        for l in v {
            println!(
                "   pc{:<5} hash={:08x} {}",
                l.target_pc,
                l.hash,
                String::from_utf8_lossy(&l.name)
            );
        }
    }
}
