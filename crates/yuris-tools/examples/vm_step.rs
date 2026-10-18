//! 单组步进探针:逐组执行,定位**静默进入 Error 态**的那一组。
//!
//! 用途:某些处理器把 `state = Error` 设好后没有把错误信息冒泡出来,
//! 事件流里看不出是哪一组。本探针每组一停,状态一变就打印现场。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example vm_step -- <游戏目录> [最大组数]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, YpfScriptHost};
use yuris_vm::{VmEvent, VmState, VmSuspend};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: vm_step <游戏目录> [n]"));
    let limit: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(4000);

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
    let pack = packs
        .iter()
        .find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        })
        .cloned()
        .expect("找不到脚本包");
    let host = YpfScriptHost::open(&pack).expect("打开脚本包");
    let mut vm = Bootstrap::boot_with_host(host, None).expect("启动链").vm;
    vm.set_strict(false);

    let mut cursor = 0usize;
    for step in 0..limit {
        let before = vm.state();
        let r = vm.run(1);
        let after = vm.state();
        let evs = vm.events();
        let fresh = evs[cursor..].to_vec();
        cursor = evs.len();

        if let Err(e) = &r {
            println!("[step {step}] run() ERR: {e}");
            dump(&fresh);
            println!("   现场: s{} pc={}", vm.script_id(), vm.pc());
            return;
        }
        if let Ok(VmSuspend::Error(m)) = &r {
            println!("[step {step}] 挂起 Error: {m}");
            dump(&fresh);
            println!("   现场: s{} pc={}", vm.script_id(), vm.pc());
            return;
        }
        if after == VmState::Error && before != VmState::Error {
            println!("[step {step}] **静默进入 Error**");
            dump(&fresh);
            println!("   现场: s{} pc={}", vm.script_id(), vm.pc());
            return;
        }
        if let Ok(VmSuspend::Wait { .. }) = &r {
            if step % 1 == 0 {
                // 帧边界:继续(每帧一组也无妨,limit 兜底)
            }
        }
        if after == VmState::Finished {
            println!("[step {step}] VM Finished");
            return;
        }
    }
    println!("到达步数上限 {limit},状态 {:?} s{} pc={}", vm.state(), vm.script_id(), vm.pc());
}

fn dump(evs: &[VmEvent]) {
    let from = evs.len().saturating_sub(6);
    for e in &evs[from..] {
        let d = format!("{e:?}");
        println!("   {}", &d[..d.len().min(180)]);
    }
}
