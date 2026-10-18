//! 只读探针:**跑到指定拍数后**一次性导出指定变量的取值(成果 138)。
//!
//! ## 为什么需要它
//!
//! `vmvars` 打印的是**前 12 拍**的轨迹;而本作的精灵/演出链是**帧驱动**的
//! —— `MAC.EV` 在 s142 进门就发起,但精灵真正装配发生在 **t26 拍**。
//! 用前 12 拍的值判断"某变量一直是空"会得出**完全错误的结论**
//! (本项目已因此两次误判,见成果 137 勘误)。
//!
//! 本探针:boot → 空跑 N 拍 → **在该时刻**逐变量取值并打印。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example vardump -- <游戏目录> <拍数> <变量…>
//!   变量写法:`@1029` / `$3650` / `$3906` / `@53[2]`

use yuris_value::{VarRef, VarSpace, Value};
use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::VmSuspend;

fn parse_var(s: &str) -> Option<(VarRef, Vec<i64>)> {
    let mut ch = s.chars();
    let prefix = ch.next()? as u8;
    let space = VarSpace::from_prefix(prefix);
    if matches!(space, VarSpace::Unknown(_)) {
        return None;
    }
    let rest = &s[1..];
    let (id_s, idx) = match rest.find('[') {
        Some(p) => {
            let close = rest.find(']')?;
            let idxs: Vec<i64> = rest[p + 1..close]
                .split(',')
                .filter_map(|x| x.trim().parse::<i64>().ok())
                .collect();
            (&rest[..p], idxs)
        }
        None => (rest, Vec::new()),
    };
    let id: u16 = id_s.parse().ok()?;
    Some((VarRef { space, id }, idx))
}

fn main() {
    let mut a = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(a.next().expect("usage: vardump <dir> <ticks> <var…>"));
    let ticks: usize = a.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let wants: Vec<(String, VarRef, Vec<i64>)> = a
        .filter_map(|s| parse_var(&s).map(|(r, i)| (s.clone(), r, i)))
        .collect();
    if wants.is_empty() {
        eprintln!("!! 没有可解析的变量");
        std::process::exit(2);
    }

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

    for _ in 0..ticks {
        for _ in 0..64 {
            match vm.run(4096) {
                Ok(VmSuspend::None) => {}
                Ok(_) => break,
                Err(_) => break,
            }
        }
        vm.trim_consumed_events(0);
    }

    println!("=== 空跑 {ticks} 拍后的取值 ===");
    for (spec, r, idxs) in &wants {
        let v = if idxs.is_empty() {
            vm.store().get(r)
        } else {
            vm.store().get_elem(r, idxs)
        };
        match v {
            Ok(Value::Int(n)) => println!("  {spec:16} = Int {n}"),
            Ok(Value::Float(f)) => println!("  {spec:16} = Flt {f}"),
            Ok(Value::Str(b)) => {
                let s = String::from_utf8_lossy(&b);
                let mut hx = String::new();
                for x in b.iter().take(32) {
                    hx.push_str(&format!("{x:02x} "));
                }
                println!("  {spec:16} = Str {s:?} ({}B) [{hx}]", b.len());
            }
            Err(e) => println!("  {spec:16} = <{e}>"),
        }
    }
}
