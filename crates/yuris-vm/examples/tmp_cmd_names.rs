//! 临时取证工具:导出游戏自身 YSCM 的「分派下标 → 命令名」表。
//!
//! 用法: cargo run -p yuris-vm --example tmp_cmd_names -- <游戏目录>

use yuris_vm::host::YpfScriptHost;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pack = std::path::Path::new(&args[1]).join("data.ypf");
    let mut host = YpfScriptHost::open(&pack).unwrap();
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.ends_with("ysbin\\ysc.ybn") || n.ends_with("ysbin/ysc.ybn"))
        .expect("ysc.ybn 不在封包内");
    let bytes = host.read_entry(&name).unwrap();
    let ysc = yuris_format::yscm::YscmTable::from_bytes(&bytes).expect("ysc.ybn 解析失败");
    let mut i = 0usize;
    while let Some(c) = ysc.command_at(i) {
        println!("{i}\t{}", c.name);
        i += 1;
    }
    eprintln!("// {i} 条命令({name})");
}
