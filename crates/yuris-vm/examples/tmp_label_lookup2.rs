//! 临时取证工具:YSLB 标签查址(适配 `data.ypf` 直接放在游戏根、无 `pac/` 的布局)。
//!
//! 用法: cargo run -p yuris-vm --example tmp_label_lookup2 -- <游戏目录> <标签名子串>

use yuris_vm::host::YpfScriptHost;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let needle = &args[2];
    let pack = std::path::Path::new(dir).join("data.ypf");
    let mut host = YpfScriptHost::open(&pack).unwrap();
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.ends_with("ysbin\\ysl.ybn") || n.ends_with("ysbin/ysl.ybn"))
        .expect("ysl.ybn 不在封包内");
    let data = host.read_entry(&name).unwrap();
    let yslb = yuris_format::yslb::YslbTable::from_bytes(&data).expect("yslb");
    let mut hits = 0;
    for l in &yslb.labels {
        let n = String::from_utf8_lossy(&l.name);
        if n.contains(needle.as_str()) {
            hits += 1;
            println!("{n} → script {} pc {}", l.script_id, l.target_pc);
        }
    }
    println!("// hits={hits} / total={}", yslb.labels.len());
}
