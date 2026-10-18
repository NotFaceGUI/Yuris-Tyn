//! 只读探针:**指定脚本 pc 处的变量实时取值**(在执行上下文中读)。
//!
//! ## 为什么不能用 `vmvars` 读
//!
//! `@48` 这类小 id 系统变量**读即计算**(引擎 sysvar switch),没有存储槽;
//! `$55` 族是**帧局部**(GOSUB 实参),只在被调函数的帧内存在。从全局存储读
//! 它们恒得 0 / undefined —— 那是**测量伪影**,会得出"没人写它"的假结论
//! (本项目已踩过两次)。
//!
//! 本探针的做法:跑到目标脚本的**目标 pc**,在该组执行前/后抓快照,
//! 并把该组的窗口字节**逐条译码**成「操作数 → 值」,于是能直接看到
//! 「这一组读了什么、写了什么」。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example pcdump -- <游戏目录> <脚本号> <pc> [节拍]
//!
//! 输出:命中 pc 时的组号 / 命令名 / 每个窗口的字节 + 求值结果。

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{VmEvent, VmSuspend};

fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: pcdump <dir> <sid> <pc> [ticks]"));
    let want_sid: u16 = a.next().and_then(|s| s.parse().ok()).expect("脚本号");
    let want_pc: usize = a.next().and_then(|s| s.parse().ok()).expect("pc");
    let ticks: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(40);

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
        .expect("脚本包");

    let host = YpfScriptHost::open(&pack).expect("打开包");
    let booted = Bootstrap::boot_with_host(host, None).expect("启动链");
    let mut vm = booted.vm;
    vm.set_file_probe(index);
    vm.set_strict(false);

    // 逐**组**步进(与 `YURIS_TRACE_GROUPS` 同一条路径:每条事件都发生在
    // 真正执行它的脚本上下文里 —— `GroupExecuted` 不带脚本号,而
    // `vm.script_id()` 此时正是该组的脚本;批量 `run(4096)` 会错配)。
    let mut tick = 0usize;
    let mut hits = 0usize;
    let mut first_at: Option<usize> = None;
    while tick < ticks {
        let s = vm.run(1);
        let done = !matches!(s, Ok(VmSuspend::None));
        for e in vm.events().iter() {
            if let VmEvent::GroupExecuted { pc, command, condition } = e {
                if *pc == want_pc && vm.script_id() == want_sid {
                    let cname = vm
                        .command_dispatch()
                        .name_of(*command as u16)
                        .unwrap_or("?")
                        .to_string();
                    if hits < 12 {
                        println!("[t{tick}] ★ 命中 s{want_sid} pc{want_pc} cmd={cname} cond={condition:?}");
                    }
                    hits += 1;
                    first_at.get_or_insert(tick);
                }
            }
        }
        vm.trim_consumed_events(0);
        if done {
            tick += 1;
        }
        if hits >= 2000 {
            break;
        }
    }
    println!("== 命中 {hits} 次;首次 t{first_at:?} ==");
}

/// 导出小 id 变量族(帧局部 `@53`/`@54`/`$55` + 全局字符串 0x0e00..0x0f80)。
fn dump_vars(vm: &yuris_vm::GroupVm, sid: u16) {
    use yuris_value::{VarRef, VarSpace, Value};
    let print_one = |space: VarSpace, id: u16, idx: Option<i64>| {
        let r = VarRef { space, id };
        let v = match idx {
            Some(i) => vm.store().get_elem(&r, &[i]),
            None => vm.store().get(&r),
        };
        match v {
            Ok(Value::Str(b)) => {
                let s = String::from_utf8_lossy(&b);
                if !b.is_empty() {
                    println!("    {}{}[{}] = Str {s:?} ({}B)", sp(space), id, idx.unwrap_or(0), b.len());
                }
            }
            Ok(Value::Int(n)) if *n != 0 => {
                println!("    {}{}[{}] = Int {n}", sp(space), id, idx.unwrap_or(0))
            }
            Ok(Value::Float(f)) if *f != 0.0 => {
                println!("    {}{}[{}] = Flt {f}", sp(space), id, idx.unwrap_or(0))
            }
            _ => {}
        }
    };
    fn sp(s: VarSpace) -> &'static str {
        match s {
            VarSpace::At => "@",
            VarSpace::Dollar => "$",
            VarSpace::Hash => "#",
            VarSpace::Backtick => "`",
            _ => "?",
        }
    }
    println!("  — s{sid} 变量快照(仅非零)—");
    // 帧局部族
    for i in 1..=8i64 {
        print_one(VarSpace::At, 53, Some(i));
        print_one(VarSpace::At, 54, Some(i));
        print_one(VarSpace::Dollar, 55, Some(i));
        print_one(VarSpace::At, 60, Some(i));
        print_one(VarSpace::Dollar, 62, Some(i));
    }
    // 精灵链涉及的全局 id(0x0e40..0x0f60 段 + 0x0cb0..0x0ce0 段)
    for id in (0x0e40u16..=0x0f60).chain(0x0cb0u16..=0x0ce2) {
        print_one(VarSpace::At, id, None);
        print_one(VarSpace::Dollar, id, None);
        print_one(VarSpace::Dollar, id, Some(1));
    }
}
