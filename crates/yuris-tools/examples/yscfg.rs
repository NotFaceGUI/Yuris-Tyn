//! 只读探针:打印游戏自身 `ysbin\yscfg.ybn`（YSCF 工程配置）的分辨率与标题。
//!
//! 用途:逻辑画布必须取**游戏自己的** `screen_width/height`，不得沿用内核里
//! 写死的 1920×1080（那是 v555 样本的值；本作 <game-b> 是 1100×825 ——
//! 其 `cgsys/config/back_*` 全屏底图实测 1100×825）。
//!
//! 用法: `yscfg <游戏目录>`

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: yscfg <游戏目录>"));

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
    let script_pack = packs
        .iter()
        .find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        })
        .cloned()
        .expect("没有包含脚本条目的 ypf");

    let mut host = YpfScriptHost::open(&script_pack).expect("打开脚本包");
    let raw = host.read_entry("ysbin\\yscfg.ybn").expect("读 yscfg.ybn");
    let cfg = yuris_format::yscf::YscfFile::from_bytes(&raw).expect("解析 YSCF");
    println!(
        "屏幕 = {}×{}   caption = {:?}",
        cfg.screen_width,
        cfg.screen_height,
        cfg.caption
    );
    println!("(内核里写死的逻辑画布 = 1920×1080，是最初样本 v555 的值)");
}
