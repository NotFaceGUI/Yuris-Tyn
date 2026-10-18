//! 只读探针:转储运行期全部 **CGINFO** 事件的 (ID, 查询槽, 应答值 / 写回目标)。
//!
//! 用途:定位「脚本查层存在性却得 0」这类缺口 —— 引擎用
//! `CGINFO[ID=… EXIST=1 LET=@r]` 做存在判据,若我方应答恒 0,
//! 脚本会走「层不存在 ⇒ 终止演出」分支,表现为**画面永远黑**而
//! 执行计数上看不出是谁关的门(成果 138/139 同类)。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example cginfo_trace -- <游戏目录> [拍数] [ID 子串滤器]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{VmEvent, VmSuspend};

fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: cginfo_trace <dir> [ticks] [filter]"));
    let ticks: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(40);
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
    let pack = packs
        .iter()
        .find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        })
        .cloned()
        .expect("找不到脚本包");
    let host = YpfScriptHost::open(&pack).expect("打开包");
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
            if let VmEvent::CgInfo { pc, id, evaluated } = e {
                let name = id.clone().unwrap_or_else(|| "<无ID>".into());
                if let Some(f) = &filter {
                    if !name.contains(f.as_str()) {
                        continue;
                    }
                }
                let rows: Vec<String> = evaluated
                    .iter()
                    .map(|(slot, s)| format!("{slot}:{s}"))
                    .collect();
                println!("[t{tick} pc{pc}] CGINFO {name:?} [{}]", rows.join(" "));
            }
        }
        vm.trim_consumed_events(0);
        if stop {
            break;
        }
    }
}
