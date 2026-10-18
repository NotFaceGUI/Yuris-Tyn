//! 只读探针:转储某段时间内**全部 CG / CGACT 事件**(不过名字滤器),
//! 按时间顺序看画面是怎么被建起来的。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cgflow -- <游戏目录> <节拍> [名字滤]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{EffectValue, VmEvent, VmSuspend};

fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: cgflow <dir> <ticks> [filter]"));
    let ticks: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(8);
    let filter: Option<String> = a.next();

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

    for tick in 0..ticks {
        let mut stop = false;
        for _ in 0..64 {
            match vm.run(4096) {
                Ok(VmSuspend::None) => {}
                Ok(_) => break,
                Err(e) => {
                    eprintln!("[err t{tick}] {e}");
                    stop = true;
                    break;
                }
            }
        }
        for e in vm.events().iter() {
            match e {
                VmEvent::Cg { pc, script_id, id, file, .. } => {
                    let Some(n) = id else { continue };
                    if let Some(f) = &filter {
                        if !n.contains(f.as_str()) {
                            continue;
                        }
                    }
                    let fs = file
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .unwrap_or_else(|| "<无FILE>".into());
                    println!("[t{tick} s{script_id} pc{pc}] CG   {n}  file={fs}");
                }
                VmEvent::CgAct { pc, id, effects, .. } => {
                    let n = id.clone().unwrap_or_else(|| "<无ID>".into());
                    if let Some(f) = &filter {
                        if !n.contains(f.as_str()) {
                            continue;
                        }
                    }
                    let fx: Vec<String> = effects
                        .iter()
                        .map(|(_, pn, v)| match v {
                            EffectValue::Int(x) => format!("{pn}={x}"),
                            EffectValue::Float(x) => format!("{pn}={x}"),
                            EffectValue::Str(s) => format!("{pn}={s:?}"),
                        })
                        .collect();
                    println!("[t{tick} pc{pc}] CGACT {n} [{}]", fx.join(" "));
                }
                VmEvent::CgEnd { pc, id, .. } => {
                    let n = id.clone().unwrap_or_default();
                    if let Some(f) = &filter {
                        if !n.contains(f.as_str()) {
                            continue;
                        }
                    }
                    println!("[t{tick} pc{pc}] CGEND {n}");
                }
                _ => {}
            }
        }
        vm.trim_consumed_events(0);
        if stop {
            break;
        }
    }
}
