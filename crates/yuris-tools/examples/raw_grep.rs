//! 只读探针:在启动链条目(YSLB / YSCM 等)**原始字节**里搜子串。
//!
//! 用途(成果 133):判定「脚本引用了某标签,但表里查不到」到底是我方**解析器
//! 丢条目**,还是**数据里真的没有**。二者处置完全不同:
//!   - 数据里没有 → 游戏自身如此(可能是版本差异/松散补丁覆盖),照实报告;
//!   - 数据里有但查不到 → **解析器缺陷**,必须修。
//!
//! 用法:
//!   cargo run --release -p yuris-tools --example raw_grep -- <游戏目录> <条目名后缀> <子串…>

use yuris_vm::host::YpfScriptHost;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: raw_grep <dir> <entry‑suffix> <needle…>"));
    let suffix = args.next().expect("条目名后缀(如 ysbin\\ysl.ybn)").to_ascii_lowercase();
    let needles: Vec<String> = args.map(|s| s.to_ascii_uppercase()).collect();

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

    for pack in &packs {
        let Ok(mut host) = YpfScriptHost::open(pack) else { continue };
        let Some(name) = host
            .entry_names()
            .into_iter()
            .find(|n| n.replace('/', "\\").to_ascii_lowercase().ends_with(&suffix))
        else {
            continue;
        };
        let Ok(data) = host.read_entry(&name) else { continue };
        println!("== {name}  {} 字节 ==", data.len());
        let up = data.iter().map(|b| b.to_ascii_uppercase()).collect::<Vec<u8>>();
        let mut total = 0usize;
        for nd in &needles {
            let nb = nd.as_bytes();
            let mut hits = Vec::new();
            let mut i = 0usize;
            while i + nb.len() <= up.len() {
                if up[i..i + nb.len()] == *nb {
                    hits.push(i);
                    if hits.len() > 20 {
                        break;
                    }
                    i += nb.len();
                } else {
                    i += 1;
                }
            }
            println!("  {nd:?}: {} 处 {:?}", hits.len(), &hits[..hits.len().min(20)]);
            // 上下文(前后 32 字节的可打印视图):看清命中处的完整标签名
            for &h in hits.iter().take(6) {
                let a = h.saturating_sub(24);
                let b = (h + 40).min(data.len());
                let ctx: String = data[a..b]
                    .iter()
                    .map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '.' })
                    .collect();
                println!("       @{h}: {ctx}");
            }
            total += hits.len();
        }
        if total == 0 {
            println!("  (无命中 —— 数据里确实没有这些串)");
        }
    }
}
