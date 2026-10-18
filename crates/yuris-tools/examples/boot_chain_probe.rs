//! 真实启动链取证探针(成果 128/129 的续篇,通用,不假设具体游戏)。
//!
//! 回答四个问题:
//! 1. `SYSTEM_START` 标签指向哪个 (script, pc)?同址还有哪些标签?
//! 2. `yst_list.ybn` 槽表:哪些槽被实际占用、名字分别是什么?
//! 3. 入口脚本对应的槽名是什么(把 s<id> 映射到系统脚本名)?
//! 4. 槽表里 es_ / global / macro 等系统脚本的真实数量与顺序。
//!
//! 用法:
//!   cargo run -p yuris-tools --example boot_chain_probe -- <游戏目录>

use yuris_vm::host::YpfScriptHost;

fn main() {
    let dir = std::env::args().nth(1).expect("用法: boot_chain_probe <游戏目录>");
    // 自动找包(契约同 CLI:根目录 + pac/)。
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
        .find(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("ypf"))
                .unwrap_or(false)
        })
        .expect("目录内没有 .ypf");
    println!("=== 封包:{} ===", pack.display());
    let mut host = YpfScriptHost::open(&pack).expect("打开脚本包");

    // ---- 1. yst_list 槽表 ----
    let names = host.entry_names();
    let list_name = names
        .iter()
        .find(|n| n.replace('/', "\\").to_ascii_lowercase().ends_with("ysbin\\yst_list.ybn"))
        .cloned();
    let mut slot_of_script: Vec<(u16, String)> = Vec::new();
    match &list_name {
        Some(n) => {
            let data = host.read_entry(n).expect("读 yst_list");
            let count = i32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
            println!("=== yst_list:条目 {n}  {count} 条 ===");
            let mut p = 12usize;
            let mut rows: Vec<(i32, String, Vec<u8>)> = Vec::new();
            for _ in 0..count {
                if p + 12 > data.len() {
                    break;
                }
                let index = i32::from_le_bytes(data[p..p + 4].try_into().unwrap());
                let name_len = i32::from_le_bytes(data[p + 4..p + 8].try_into().unwrap()) as usize;
                if p + 8 + name_len + 20 > data.len() {
                    println!("   (截断 @ {p})");
                    break;
                }
                let script_name =
                    String::from_utf8_lossy(&data[p + 8..p + 8 + name_len]).to_string();
                let tail = data[p + 8 + name_len..p + 8 + name_len + 16].to_vec();
                rows.push((index, script_name, tail));
                p += 0x1C + name_len;
            }
            for (index, script_name, tail) in &rows {
                let tl = &tail[..8.min(tail.len())];
                println!("  slot={index:3}  {script_name:30} tail8={tl:02x?}");
                slot_of_script.push((*index as u16, script_name.clone()));
            }
        }
        None => println!("=== 包内没有 yst_list.ybn ==="),
    }

    // ---- 2. 散装 ysbin(补丁脚本,按文件名 idx 映射槽号)----
    let loose_dir = format!("{dir}/ysbin");
    if let Ok(rd) = std::fs::read_dir(&loose_dir) {
        let mut loose: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.to_ascii_lowercase().starts_with("yst") && n.ends_with(".ybn"))
            .collect();
        loose.sort();
        if !loose.is_empty() {
            println!("=== 散装补丁脚本 {} 个(前 20) ===", loose.len());
            for n in loose.iter().take(20) {
                println!("  {n}");
            }
        }
    }

    // ---- 3. SYSTEM_START 标签 ----
    let ysl_name = names
        .iter()
        .find(|n| n.replace('/', "\\").to_ascii_lowercase().ends_with("ysbin\\ysl.ybn"))
        .cloned();
    match &ysl_name {
        Some(n) => {
            let blob = host.read_entry(n).expect("读 ysl");
            let t = yuris_format::yslb::YslbTable::from_bytes(&blob).expect("解析 YSLB");
            println!("=== YSLB:{} 条标签 ===", t.labels().len());
            // 同址标签簇:SYSTEM_START 落点 + 该处全部名字
            use std::collections::BTreeMap;
            let mut by_target: BTreeMap<(u16, u32), Vec<String>> = BTreeMap::new();
            for l in t.labels() {
                by_target
                    .entry((l.script_id, l.target_pc))
                    .or_default()
                    .push(String::from_utf8_lossy(&l.name).to_string());
            }
            for key in [b"SYSTEM_START".as_slice(), b"SYSTEM".as_slice()] {
                let needle = String::from_utf8_lossy(key).to_string();
                for l in t.labels() {
                    let name = String::from_utf8_lossy(&l.name);
                    if name.eq_ignore_ascii_case(&needle) {
                        println!(
                            "  标签 {name} -> s{} pc{}",
                            l.script_id, l.target_pc
                        );
                        if let Some(others) = by_target.get(&(l.script_id, l.target_pc)) {
                            println!("    同址标签: {others:?}");
                        }
                        // 落点属于哪个槽?用 yst_list 的 idx→name 反推
                        if let Some((_, sname)) =
                            slot_of_script.iter().find(|(i, _)| *i == l.script_id)
                        {
                            println!("    yst_list 槽 {} = {sname}", l.script_id);
                        } else {
                            println!("    (yst_list 无槽 {})", l.script_id);
                        }
                    }
                }
            }
            // 含 SYSTEM 的所有标签
            println!("  --- 名字含 SYSTEM 的标签 ---");
            for l in t.labels() {
                let name = String::from_utf8_lossy(&l.name);
                if name.to_ascii_uppercase().contains("SYSTEM") {
                    println!("   {name} -> s{} pc{}", l.script_id, l.target_pc);
                }
            }
            // 含 ES. 场景名的关键标签(标题/LOGO/警告链)
            println!("  --- 关键场景标签(标题/警告/LOGO)---");
            for l in t.labels() {
                let name = String::from_utf8_lossy(&l.name);
                let up = name.to_ascii_uppercase();
                if up.contains("TITLE") || up.contains("WARN") || up.contains("LOGO")
                    || up.contains("CAUTION") || up.contains("NOTICE")
                {
                    println!("   {name} -> s{} pc{}", l.script_id, l.target_pc);
                }
            }
        }
        None => println!("=== 包内没有 ysl.ybn ==="),
    }
}
