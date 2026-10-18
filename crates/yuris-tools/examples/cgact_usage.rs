//! 只读探针:**全语料统计 CGACT 效果槽的使用频度**。
//!
//! 用途:CGACT 有 70 个效果槽(COPY/TEXCOPY/BLUR/…),渲染侧不可能一次全
//! 实装 —— 本探针按「本作实际用到几次」排序,给出**按需实装**的优先级。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cgact_usage -- <游戏目录> [节拍数]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{EffectValue, VmEvent, VmSuspend};
use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: cgact_usage <dir> [ticks]"));
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2000);

    let index = std::sync::Arc::new(PacFileIndex::scan_game_dir_auto(&dir).expect("扫描封包"));
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
        .expect("找不到脚本包");

    let host = YpfScriptHost::open(&script_pack).expect("打开脚本包");
    let booted = Bootstrap::boot_with_host(host, None).expect("启动链");
    let mut vm = booted.vm;
    vm.set_file_probe(index);
    vm.set_strict(false);

    // 槽名 → 出现次数;另记「带非平凡值的次数」(值 != 0 / != "" 才算真用)。
    let mut hits: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut cgact_total = 0u64;
    for _tick in 0..ticks {
        let mut stop = false;
        for _ in 0..64 {
            match vm.run(4096) {
                Ok(VmSuspend::None) => {}
                Ok(_) => break,
                Err(_) => {
                    stop = true;
                    break;
                }
            }
        }
        for e in vm.events() {
            if let VmEvent::CgAct { effects, .. } = e {
                cgact_total += 1;
                for (_slot, name, v) in effects.iter() {
                    let nontrivial = match v {
                        EffectValue::Int(n) => *n != 0,
                        EffectValue::Float(f) => *f != 0.0,
                        EffectValue::Str(s) => !s.is_empty(),
                    };
                    let ent = hits.entry(name.clone()).or_insert((0, 0));
                    ent.0 += 1;
                    if nontrivial {
                        ent.1 += 1;
                    }
                }
            }
        }
        vm.trim_consumed_events(0);
        if stop {
            break;
        }
    }
    // 按「非平凡值次数」降序 = 真用得上的槽排前面。
    let mut rows: Vec<(String, u64, u64)> =
        hits.into_iter().map(|(k, (a, b))| (k, a, b)).collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)));
    println!("CGACT 组总数 {cgact_total};效果槽使用统计(按真值次数降序):");
    for (name, total, real) in rows {
        println!("  {name:14} 出现 {total:7}  真值 {real:7}");
    }
    let _ = is_script_entry;
}
