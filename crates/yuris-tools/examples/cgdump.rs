//! 只读探针:按名字子串转储某个 CG 的**逐条原始事件槽**(X/Y/Z/E/A/FILE)。
//!
//! 用途:定位「注册表里位置正确、但场景层坐标是 0」这类字段丢失 ——
//! 直接看每次 `Cg` 事件带没带 X/Y,以及 CGACT 是否补了位置。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cgdump -- <游戏目录> <名字子串> [节拍数] [CGACT效果名过滤]
//!
//! 第 4 参(可选)只转储**含该效果槽**的 CGACT(如 `COPY2` / `TEXSSCOPY`),
//! 用于按需取「某个效果到底带什么参数」的实证样本。

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{VmEvent, VmSuspend};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: cgdump <dir> <needle> [ticks] [effect]"));
    let needle = args.next().unwrap_or_default();
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(900);
    let want_effect = args.next().map(|s| s.to_ascii_uppercase());

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
                    eprintln!("[err] {e}");
                    stop = true;
                    break;
                }
            }
        }
        for e in vm.events() {
            match e {
                VmEvent::Cg {
                    pc,
                    script_id,
                    id,
                    position,
                    file,
                    e: ev_e,
                    x,
                    y,
                    z,
                    a,
                    mode,
                    t,
                    ta,
                    ..
                } => {
                    let Some(name) = id else { continue };
                    if !name.contains(&needle) {
                        continue;
                    }
                    println!(
                        "[t{tick} s{script_id} pc{pc}] CG {name:?} pos={position:?} x={x:?} y={y:?} z={z:?} E={ev_e:?} A={a:?} MODE={mode:?} T={t:?} TA={ta:?} file={:?}",
                        file.as_ref().map(|f| String::from_utf8_lossy(f).to_string())
                    );
                }
                VmEvent::CgAct { pc, id, effects, .. } => {
                    // 效果名过滤(第 4 参):只看真用到某效果的样本。
                    if let Some(want) = &want_effect {
                        if !effects.iter().any(|(_, n, _)| n.eq_ignore_ascii_case(want)) {
                            continue;
                        }
                    }
                    let name_s = id.as_deref().unwrap_or("<无ID>");
                    if !want_effect.is_some() && !name_s.contains(&needle) {
                        continue;
                    }
                    // 效果槽(成果 133):CGACT 的图形管线参数 —— 片头 LOGO 的内容
                    // 靠 COPY/TEXSSCOPY/SETSTR 等槽从别处搬过来,这里逐槽打全。
                    let fx: Vec<String> = effects
                        .iter()
                        .map(|(_slot, pname, v)| match v {
                            yuris_vm::EffectValue::Int(n) => format!("{pname}={n}"),
                            yuris_vm::EffectValue::Float(f) => format!("{pname}={f}"),
                            yuris_vm::EffectValue::Str(t) => format!("{pname}={t:?}"),
                        })
                        .collect();
                    let pos = vm.cg_position(name_s);
                    println!(
                        "[t{tick} pc{pc}] CGACT {name_s:?} pos={pos:?} fx=[{}]",
                        fx.join(" ")
                    );
                    continue;
                }
                VmEvent::CgEnd { pc, id, .. } => {
                    let Some(name) = id.as_deref() else { continue };
                    if !name.contains(&needle) {
                        continue;
                    }
                    println!("[t{tick} pc{pc}] CGEND {name:?}");
                }
                VmEvent::ScriptSwitch { from_script, to_script } => {
                    println!("[t{tick}] SWITCH s{from_script} → s{to_script}");
                }
                _ => {}
            }
        }
        if stop {
            break;
        }
    }
    println!("== 转储结束 ==");
}
