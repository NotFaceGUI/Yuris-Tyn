//! 无窗口探针:**指定变量的运行期取值**(黑屏定位用)。
//!
//! 用法: `vmvars <游戏目录> <节拍数> <变量...>`
//!   变量写法:`@1029` / `$1036` / `@53[2]`(二维用 `@53[2,3]`)
//! 变量空间前缀:`@` 整数/浮点、`$` 字符串、`#` 标签、`` ` `` 其它。
//!
//! 输出:每个变量的类型与值(字符串按 SJIS 有损解码 + 原始字节)。
//! 用途:对照官方 SDK 示例(ERIS)的全局变量默认值,判断「场景开始标签」
//! 这类关键变量在本内核里是不是空串 —— 空串会让 `GO[#=$xxx]` 无处可去,
//! 场景任务静默不做事,画面恒黑。

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
                .map(|x| x.trim().parse::<i64>().ok())
                .collect::<Option<Vec<_>>>()?;
            (&rest[..p], Some(idxs))
        }
        None => (rest, None),
    };
    let id: u16 = id_s.trim().parse().ok()?;
    Some((VarRef { space, id }, idx.unwrap_or_default()))
}

fn show(v: &Value) -> String {
    match v {
        Value::Int(i) => format!("Int {i}"),
        Value::Float(f) => format!("Float {f}"),
        Value::Str(b) => {
            let lossy = String::from_utf8_lossy(b).replace('\u{FFFD}', "");
            format!("Str {lossy:?} ({} 字节: {:02x?})", b.len(), b)
        }
        other => format!("{other:?}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next().expect("usage: vmvars <游戏目录> <节拍数> <变量...>"),
    );
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(300);
    // 可选:`--at=<标签>` 启动后强制 GOSUB 到该标签(与 screenstate 同一取证口径)。
    let mut force_label: Option<String> = None;
    let mut specs: Vec<String> = Vec::new();
    for a in args {
        match a.strip_prefix("--at=") {
            Some(l) => force_label = Some(l.to_string()),
            None => specs.push(a),
        }
    }
    let wants: Vec<(String, VarRef, Vec<i64>)> = specs
        .iter()
        .filter_map(|spec| parse_var(spec).map(|(r, i)| (spec.clone(), r, i)))
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
        .unwrap_or_else(|| {
            eprintln!("!! 没有包含脚本条目");
            std::process::exit(2);
        });

    let host = YpfScriptHost::open(&script_pack).expect("打开脚本包");
    let booted = Bootstrap::boot_with_host(host, None).expect("启动链");
    let mut vm = booted.vm;
    vm.set_file_probe(index.clone());
    vm.set_strict(false);
    if let Some(label) = &force_label {
        match vm.push_guest_call(label, &[], &[]) {
            Ok(()) => eprintln!("[force] 已 GOSUB 到标签 {label:?}"),
            Err(e) => eprintln!("[force] 标签 {label:?} 失败: {e}"),
        }
    }

    // 每个节拍前后都采样一次取值 —— 变量可能在某一拍被改写并立刻被读走。
    let mut seen: Vec<Vec<String>> = vec![Vec::new(); wants.len()];
    let sample = |vm: &yuris_vm::GroupVm, seen: &mut Vec<Vec<String>>, tag: &str| {
        for (i, (_spec, r, idxs)) in wants.iter().enumerate() {
            let v = if idxs.is_empty() {
                vm.store().get(r)
            } else {
                vm.store().get_elem(r, idxs)
            };
            let s = match v {
                Ok(v) => format!("{tag} {}", show(v)),
                Err(e) => format!("{tag} <{e}>"),
            };
            if seen[i].last() != Some(&s) {
                seen[i].push(s);
            }
        }
    };

    for tick in 0..ticks {
        for _ in 0..64 {
            match vm.run(4096) {
                Ok(VmSuspend::None) => {}
                Ok(_) => break,
                Err(_) => break,
            }
        }
        let n = vm.events().len();
        vm.trim_consumed_events(n);
        sample(&vm, &mut seen, &format!("t{tick}"));
    }

    println!("=== 执行 {} 组 / {} 拍后的变量轨迹 ===", vm.executed_groups(), ticks);
    for (i, (spec, _r, _idx)) in wants.iter().enumerate() {
        println!("\n{spec}:");
        if seen[i].is_empty() {
            println!("  (空)");
        }
        for line in seen[i].iter().take(12) {
            println!("  {line}");
        }
        if seen[i].len() > 12 {
            println!("  …(共 {} 次变化)", seen[i].len());
        }
    }
}
