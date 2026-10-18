//! 只读探针:**全语料图层 id 普查** —— 分组命名约定、以及「谁被 COPY2 写」。
//!
//! 用途(成果 134):渲染侧要实装「图层组离屏 + COPY2 复制」,先得知道
//! 本作的**实际数据形态**:
//!   - 图层 id 是否真的用 `名字/` 的尾斜杠表示**图层组**(手册 `ABC/`);
//!   - `COPY2` 的 `ID`/`ID2` 谁是源、谁是目标(手册只说"复制",没说方向);
//!   - 哪些 id 只作为 COPY2 的目标出现(= 纯离屏画布,不需要直接上屏)。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example layer_census -- <游戏目录> [节拍数]

use std::collections::{BTreeMap, BTreeSet};

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{EffectValue, VmEvent, VmSuspend};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: layer_census <dir> [ticks]"));
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1200);

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

    // id → (Cg 出现次数, CGACT 作为 ID 次数, 作为 COPY2/COPY 的 ID2 次数)
    let mut ids: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    // 「id → 它被 CG 创建时的 FILE(取首个非空)」
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut slash_ids: BTreeSet<String> = BTreeSet::new();
    // COPY2 的两端对偶:(ID, ID2) 计数 —— 判方向用
    let mut copy_pairs: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut copy_other_keys: BTreeMap<String, u64> = BTreeMap::new();

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
            match e {
                VmEvent::Cg { id, file, .. } => {
                    let Some(name) = id else { continue };
                    ids.entry(name.clone()).or_default().0 += 1;
                    if name.contains('/') {
                        slash_ids.insert(name.clone());
                    }
                    if let Some(f) = file {
                        let s = String::from_utf8_lossy(&f).into_owned();
                        if !s.is_empty() {
                            files.entry(name.clone()).or_insert(s);
                        }
                    }
                }
                VmEvent::CgAct { id, effects, .. } => {
                    let Some(name) = id else { continue };
                    ids.entry(name.clone()).or_default().1 += 1;
                    // 收集本次的全部槽名(判 COPY2 是否存在、ID2 取值)
                    let mut is_copy = false;
                    let mut id2: Option<String> = None;
                    for (_slot, pname, v) in effects.iter() {
                        let up = pname.to_ascii_uppercase();
                        if up == "COPY2" || up == "COPY" {
                            let on = match v {
                                EffectValue::Int(n) => *n != 0,
                                EffectValue::Float(f) => *f != 0.0,
                                EffectValue::Str(s) => !s.is_empty(),
                            };
                            if on {
                                is_copy = true;
                                copy_other_keys.entry(up.clone()).and_modify(|c| *c += 1).or_insert(1);
                            }
                        }
                        if up == "ID2" {
                            if let EffectValue::Str(s) = v {
                                id2 = Some(s.clone());
                            }
                        }
                    }
                    if is_copy {
                        if let Some(d) = id2 {
                            ids.entry(d.clone()).or_default().2 += 1;
                            *copy_pairs.entry((name.clone(), d)).or_insert(0) += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        vm.trim_consumed_events(0);
        if stop {
            break;
        }
    }

    println!("== 图层 id 总数 {} ==", ids.len());
    println!("-- 含 `/` 的 id({} 个) --", slash_ids.len());
    for s in slash_ids.iter().take(40) {
        println!("   {s:?}");
    }
    println!("-- COPY2 的方向与键 --");
    for (k, v) in copy_other_keys.iter() {
        println!("   {k} 真值次数 {v}");
    }
    let mut pairs: Vec<_> = copy_pairs.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1));
    println!("-- COPY2 (ID → ID2) 对偶 top 25 --");
    for ((a, b), n) in pairs.iter().take(25) {
        println!("   {n:7}  {a:?} → {b:?}");
    }
    println!("-- 纯 COPY2 目标(从未被 CG 创建过的 ID2)top 20 --");
    let mut pure: Vec<_> = ids
        .iter()
        .filter(|(_, (cg, _, copy2))| *cg == 0 && *copy2 > 0)
        .collect();
    pure.sort_by(|a, b| b.1 .2.cmp(&a.1 .2));
    for (name, (_, _, c2)) in pure.iter().take(20) {
        println!("   copy2×{c2:7}  {name:?}");
    }
    println!("-- 有 FILE 的 id 样本(前 25) --");
    for (name, f) in files.iter().take(25) {
        println!("   {name:?} ← {f:?}");
    }
}
