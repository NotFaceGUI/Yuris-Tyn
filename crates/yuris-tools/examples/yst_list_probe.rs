//! `ysbin\yst_list.ybn` 解析探针(成果 127 后续:真实启动链取证)。
//! 记录布局 = 旁线仓库/docs/format-ybn-loader.md §3(反推,待验证):
//! count@B+8;记录 = index(i32) nameLen(i32) name(CP932) 16B无名 fieldX(i32)。

use yuris_vm::host::YpfScriptHost;

fn main() {
    let dir = std::env::args().nth(1).expect("用法: yst_list_probe <游戏目录>");
    // 自动找包(根目录 + pac/;与 CLI 的发现逻辑同约定)。
    let pack = ["", "pac/"]
        .iter()
        .flat_map(|p| {
            std::fs::read_dir(format!("{dir}/{p}"))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
        })
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|s| s.to_str()).map(|s| s.eq_ignore_ascii_case("ypf")).unwrap_or(false))
        .expect("目录内没有 .ypf");
    let mut host = YpfScriptHost::open(std::path::Path::new(&pack)).expect("打开脚本包");
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.replace('/', "\\").to_ascii_lowercase().ends_with("ysbin\\yst_list.ybn"));
    let Some(name) = name else {
        eprintln!("包内没有 yst_list.ybn;散装目录也没有?列出含 list 的条目:");
        for n in host.entry_names().into_iter() {
            if n.to_ascii_lowercase().contains("list") {
                eprintln!("  {n}");
            }
        }
        return;
    };
    let data = host.read_entry(&name).expect("读条目");
    println!("条目 {name}  {} B  头16字节 {:02x?}", data.len(), &data[..16.min(data.len())]);

    let count = i32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    println!("count = {count}");
    let mut p = 12usize;
    for _ in 0..count {
        let index = i32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        let name_len = i32::from_le_bytes(data[p + 4..p + 8].try_into().unwrap()) as usize;
        let script_name = String::from_utf8_lossy(&data[p + 8..p + 8 + name_len]).to_string();
        let tail_all: Vec<u8> =
            data[p + 8 + name_len..p + 8 + name_len + 16].to_vec();
        let field_x =
            i32::from_le_bytes(data[p + 8 + name_len + 16..p + 8 + name_len + 20].try_into().unwrap());
        println!("  slot={index:3}  {script_name:28} tail16={tail_all:02x?} fieldX={field_x}");
        p += 0x1C + name_len;
    }
    println!("解析结束 @ {p} / {}", data.len());
}
