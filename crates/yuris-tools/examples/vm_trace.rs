//! 无窗口 VM 追踪探针:启动游戏 → 逐节拍驱动 VM → 打印事件流。
//!
//! 用途:定位"黑屏"这类运行期停滞 —— 渲染器没问题时,画面空 =
//! VM 没产出 CG 事件。本探针**不依赖窗口**,只跑 VM,把 Jump/Call/
//! ScriptSwitch 的 from→to 序列与 CG/CgAct 计数打出来。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example vm_trace -- <游戏目录> [节拍数] [过滤]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{VmEvent, VmSuspend};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next()
            .expect("usage: vm_trace <游戏目录> [节拍数] [过滤子串]"),
    );
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(600);
    let filter = args.next();
    let all = std::env::var("YURIS_TRACE_ALL").is_ok();

    let index = std::sync::Arc::new(
        PacFileIndex::scan_game_dir_auto(&dir).expect("扫描封包"),
    );

    // 脚本包 = 含 ysbin 下 yst*.ybn 的那个
    let mut packs: Vec<std::path::PathBuf> = Vec::new();
    for root in [dir.clone(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
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
        .unwrap_or_else(|| {
            eprintln!("!! 没有包含脚本条目");
            std::process::exit(2);
        });
    eprintln!("[trace] 脚本包 {}", script_pack.display());

    let host = YpfScriptHost::open(&script_pack).expect("打开脚本包");
    eprintln!(
        "[trace] 方言 {:?} 密钥 {:02x?}",
        host.dialect(),
        host.script_key()
    );
    let booted = Bootstrap::boot_with_host(host, None).expect("启动链");
    eprintln!(
        "[trace] 入口 s{} pc{} YSVR {} 分派 {}",
        booted.script_id, booted.entry_pc, booted.applied_ysvr, booted.command_profile_source
    );
    let mut vm = booted.vm;
    vm.set_file_probe(index.clone());
    // 诊断用:非严格模式 —— 未实现命令记事件后继续,量出「还有多少处缺口」。
    if std::env::var("YURIS_LENIENT").is_ok() {
        vm.set_strict(false);
        eprintln!("[trace] lenient 模式:未实现命令记录后继续");
    }

    let mut cursor = 0usize;
    for tick in 0..ticks {
        let mut susp = None;
        for _ in 0..64 {
            match vm.run(4096) {
                Ok(VmSuspend::None) => {
                    if vm.executed_groups() % 250_000 == 0 {
                        break;
                    }
                }
                Ok(s) => {
                    susp = Some(format!("{s:?}"));
                    break;
                }
                Err(e) => {
                    susp = Some(format!("ERR {e}"));
                    break;
                }
            }
        }
        // 打印本拍事件。**打完即丢**(内存封顶):本探针不保留历史事件,
        // 否则长跑必然爆(实测 4000 tick 宽松模式下事件数达千万级;
        // 开启任务调度后更多)。`cursor` 因此恒为 0。
        let evs = vm.events();
        let fresh = &evs[cursor..];
        let consumed = evs.len();
        for e in fresh {
            let line = match e {
                VmEvent::Jump { from, to, kind } => {
                    Some(format!("  jump {kind:?} {from} → {to}"))
                }
                VmEvent::Call { from, to } => Some(format!("  call {from} → {to}")),
                VmEvent::ScriptSwitch { from_script, to_script } => {
                    Some(format!("  SWITCH s{from_script} → s{to_script}"))
                }
                VmEvent::Cg { pc, script_id, id, file, .. } => Some(format!(
                    "  CG s{script_id} pc{pc} id={:?} file={:?}",
                    id.as_deref().unwrap_or("-"),
                    file.as_ref()
                        .map(|f| String::from_utf8_lossy(f).into_owned())
                        .unwrap_or_else(|| "-".into())
                )),
                VmEvent::CgAct { pc, id, .. } => {
                    Some(format!("  CGACT pc{pc} id={:?}", id.as_deref().unwrap_or("-")))
                }
                VmEvent::Unsupported { pc, command, reason } => {
                    Some(format!("  UNSUP pc{pc} cmd{command} {reason}"))
                }
                VmEvent::GroupExecuted { pc, command, .. } => {
                    if all {
                        Some(format!("  g{pc} cmd{command}"))
                    } else {
                        None
                    }
                }
                VmEvent::Text { pc, file, .. } => Some(format!(
                    "  TEXT pc{pc} file={:?}",
                    file.as_ref()
                        .map(|f| String::from_utf8_lossy(f).into_owned())
                        .unwrap_or_else(|| "-".into())
                )),
                VmEvent::Sound {
                    pc,
                    id,
                    file,
                    play,
                    volume,
                    loop_count,
                    fade_ms,
                    ..
                } => Some(format!(
                    "  SOUND pc{pc} id={:?} file={:?} play={play:?} vol={volume:?} loop={loop_count:?} fade={fade_ms:?}",
                    id.as_deref().unwrap_or("-"),
                    file.as_ref()
                        .map(|f| String::from_utf8_lossy(f).into_owned())
                        .unwrap_or_else(|| "-".into())
                )),
                VmEvent::VarQuery { pc, evaluated, .. } => {
                    let s: Vec<String> =
                        evaluated.iter().map(|(b, v)| format!("{b}:{v}")).collect();
                    Some(format!("  VARQ pc{pc} [{}]", s.join(" ")))
                }
                _ => None,
            };
            let Some(line) = line else { continue };
            if let Some(f) = &filter {
                if !line.contains(f.as_str()) {
                    continue;
                }
            }
            println!("[{tick:4}] {line}");
        }
        cursor = vm.trim_consumed_events(consumed);
        if let Some(s) = susp {
            println!(
                "[{tick:4}] === 挂起: {s}  s{} pc={} ===",
                vm.script_id(),
                vm.pc()
            );
        }
    }
    // 收尾诊断:任务表规模(内存/调度健康度;`VmTask` 各持一份执行位)。
    println!(
        "[done] 执行 {} 组 · 事件 {} 条 · 任务 {} 个(已结束 {})",
        vm.executed_groups(),
        vm.events().len(),
        vm.task_count(),
        vm.task_finished_count()
    );
    for s in vm.task_summary() {
        println!("[task] {s}");
    }
}
