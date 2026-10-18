//! 只读探针:在**全部脚本的原始字节**里数某个字符串出现几次、在哪些脚本里。
//!
//! 用途(成果 135):判定「标签名只被引用、从未定义」还是「别处有定义」。
//! `litgrep_bytes` 只搜**窗口**(表达式池),而标签定义如果存在于脚本里,
//! 会落在窗口之外(声明段/附属表)—— 本探针搜整条脚本字节。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example script_grep -- <游戏目录> <字面串>

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: script_grep <dir> <needle>"));
    let needle = args.next().expect("needle");

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
    let nb = needle.as_bytes();
    let ids = host.script_ids().expect("脚本清单");
    let mut total = 0usize;
    for sid in ids {
        let Ok(ctx) = host.load(sid) else { continue };
        // 脚本的**原始解密字节**:拼回窗口池 + 头部,近似"整脚本"。
        let mut blob: Vec<u8> = Vec::new();
        for w in ctx.script.slots() {
            if let Some(b) = ctx.script.window_bytes_pooled_copy(w) {
                blob.extend_from_slice(&b);
                blob.push(0);
            }
        }
        let mut hits = Vec::new();
        let mut i = 0usize;
        while i + nb.len() <= blob.len() {
            if &blob[i..i + nb.len()] == nb {
                hits.push(i);
                i += nb.len();
            } else {
                i += 1;
            }
        }
        if !hits.is_empty() {
            println!("s{sid}: {} 处 {:?}", hits.len(), hits);
            total += hits.len();
        }
    }
    println!("== 合计 {total} 处(串 {needle:?}) ==");
}
