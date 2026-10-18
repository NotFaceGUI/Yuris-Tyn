//! 无窗口探针:**「屏幕上本该有什么」的最终状态导出** —— 黑屏三段隔离实验。
//!
//! 起因:实机长期黑屏。黑屏可能断在三段中的任意一段:
//!   (1) VM 侧 —— 游戏从未注册任何「带纹理的层」;
//!   (2) 资源侧 —— 注册了,但纹理路径在封包里解不出来;
//!   (3) 渲染侧 —— 纹理与层都在,画不到屏幕上。
//! 本探针只回答 (1) 与 (2):跑 N 拍后导出
//!   - 事件侧:每个 CG id 的**末次** `Cg`(file/xy/z)与是否被 `CgEnd` 隐藏;
//!   - 注册表侧:`cg_registry_snapshot` 的最终内容;
//!   - 每个**去重后**的纹理路径:`image_dims`(能否解出宽高)+ `read_image_bytes` 字节数。
//! 输出里含「疑似可见层」计数:非空 FILE 且未隐藏 —— 该数为 0 则断点必在 (1)。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example screenstate -- <游戏目录> [节拍数]

use std::collections::{BTreeMap, BTreeSet};

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{VmEvent, VmSuspend};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(
        args.next()
            .expect("usage: screenstate <游戏目录> [节拍数]"),
    );
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(600);
    // 可选:启动后**强制 GOSUB 到某标签**(取证用,例如 `SCENARIO_START` = 标题脚本)。
    // 用途:把「到不了入口」与「到得了入口但画不出」两种故障分开定位。
    let force_label = args.next();

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
    if let Some(label) = force_label {
        match vm.push_guest_call(&label, &[], &[]) {
            Ok(()) => eprintln!("[force] 已 GOSUB 到标签 {label:?}"),
            Err(e) => eprintln!("[force] 标签 {label:?} 失败: {e}"),
        }
    }

    // —— 事件侧累计:每个 CG id 的末次状态 ——
    #[derive(Default, Clone)]
    struct LastCg {
        file: Option<String>,
        pos: Option<(i64, i64, i64)>,
        script: u16,
        pc: usize,
        hidden: bool,
        /// 最近一次 `E`(显隐;None = 语料未指定该槽)。
        e: Option<i64>,
        /// 是否曾给过 FILE(区分「注册件」与「带纹理件」)。
        had_file: bool,
        /// 末次 Z(手册:层优先级「レイヤの優先順位」)。
        z: Option<i64>,
        /// 末次 A(0〜256 不透明度)。
        a: Option<i64>,
        /// **逐槽**末次 X/Y(旧 `pos` 要求 X+Y 同时到场,会把只给一个轴的
        /// 调用误报成"没有位置")。
        x: Option<i64>,
        y: Option<i64>,
    }
    let mut layer: BTreeMap<String, LastCg> = BTreeMap::new();
    let mut cg_with_file = 0usize;
    let mut cg_empty_file = 0usize;
    let mut cg_no_id = 0usize;
    let mut cg_end = 0usize;
    let mut text_events = 0usize;

    for _tick in 0..ticks {
        let mut stop = false;
        for _ in 0..64 {
            match vm.run(4096) {
                Ok(VmSuspend::None) => {}
                // **WAIT/Complete = 一拍边界**(与播放器一致):继续下一拍,
                // 不得整体中止 —— 否则探针在第一处 WAIT 就停,量出的
                // 「可见层」永远只是启动瞬间的状态(实测踩到)。
                Ok(_) => break,
                Err(e) => {
                    eprintln!("[err] {e}");
                    stop = true;
                    break;
                }
            }
        }
        let evs = vm.events();
        let n = evs.len();
        for e in evs.iter() {
            match e {
                VmEvent::Cg { pc, script_id, id, position, file, e, z, a, x: px_, y: py_, .. } => match id {
                    None => cg_no_id += 1,
                    Some(name) => {
                        let en = layer.entry(name.clone()).or_default();
                        if file.as_ref().is_none_or(|f| f.is_empty()) {
                            cg_empty_file += 1;
                        } else {
                            cg_with_file += 1;
                            en.file = file
                                .as_ref()
                                .map(|f| String::from_utf8_lossy(f).into_owned());
                            en.had_file = true;
                        }
                        en.pos = *position;
                        en.script = *script_id;
                        en.pc = *pc;
                        en.hidden = false;
                        en.e = *e;
                        if let Some(v) = z {
                            en.z = Some(*v);
                        }
                        if let Some(v) = a {
                            en.a = Some(*v);
                        }
                        if let Some(v) = px_ {
                            en.x = Some(*v);
                        }
                        if let Some(v) = py_ {
                            en.y = Some(*v);
                        }
                    }
                },
                VmEvent::CgEnd { id, .. } => {
                    cg_end += 1;
                    if let Some(name) = id {
                        if let Some(en) = layer.get_mut(name) {
                            en.hidden = true;
                        }
                    }
                }
                VmEvent::Text { .. } => text_events += 1,
                _ => {}
            }
        }
        let consumed = evs.len();
        vm.trim_consumed_events(consumed);
        if stop {
            break;
        }
        let _ = n;
    }

    println!("=== 事件侧(执行 {:?} 组)===", vm.executed_groups());
    println!(
        "Cg 带 FILE {cg_with_file} · 空 FILE {cg_empty_file} · 无 ID {cg_no_id} · CgEnd {cg_end} · TEXT {text_events}"
    );
    println!("CG id 去重 {} 个", layer.len());

    let mut visible: Vec<(&String, &LastCg)> =
        layer.iter().filter(|(_, v)| v.file.is_some() && !v.hidden).collect();
    visible.sort_by_key(|(_, v)| v.pos.map(|p| p.2).unwrap_or(0));
    let e_shown = visible.iter().filter(|(_, v)| v.e != Some(0)).count();
    println!(
        "\n=== 「带纹理且未被 CgEnd 隐藏」的层:{} 个(其中 E≠0 即游戏要求显示 = {e_shown}) ===",
        visible.len()
    );
    for (name, v) in visible.iter().take(80) {
        let nm = name.as_str();
        let (fx, fy, _zp) = v.pos.unwrap_or((0, 0, 0));
        let (x, y) = (v.x.unwrap_or(fx), v.y.unwrap_or(fy));
        // Z 取**逐槽**字段(成果 122):只给 Z 的调用也算数。
        let z = v.z.unwrap_or(0);
        let amark = v
            .a
            .map(|a| format!("A={a}/256"))
            .unwrap_or_else(|| "A未给".into());
        let f = v.file.clone().unwrap_or_default();
        let dims = index
            .image_dims(&f)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "**解不出**".into());
        let bytes = index
            .read_image_bytes(&f)
            .map(|b| format!("{}B", b.len()))
            .unwrap_or_else(|| "**读不到**".into());
        let emark = match v.e {
            Some(0) => "E=0(游戏要求隐藏)",
            Some(_) => "E≠0",
            None => "E未指定",
        };
        println!(
            "  z{z:<6} ({x},{y}) {emark:<18} {amark:<10} {nm:<40} {f:<44} {dims} {bytes}  [s{} pc{}]",
            v.script, v.pc
        );
    }
    if visible.len() > 80 {
        println!("  …(其余 {} 个省略)", visible.len() - 80);
    }

    // —— 资源侧:全部去重路径的解码统计 ——
    let files: BTreeSet<String> = layer.values().filter_map(|v| v.file.clone()).collect();
    let mut ok = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for f in &files {
        if index.image_dims(f).is_some() {
            ok += 1;
        } else {
            bad.push(f.clone());
        }
    }
    println!(
        "\n=== 资源侧:去重纹理 {} 条 → 可解宽高 {} · 解不出 {} ===",
        files.len(),
        ok,
        bad.len()
    );
    for f in files.iter() {
        let n = index
            .read_image_bytes(f)
            .map(|b| b.len())
            .unwrap_or(0);
        let dims = index
            .image_dims(f)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "**解不出**".into());
        let cgsys = if f.to_ascii_lowercase().contains("cgsys") {
            "cgsys"
        } else {
            "**非 cgsys**"
        };
        // 解不出宽高的条目:附首 16 字节,便于识别真实格式(PNG/JPEG/BMP/图层束…)。
        let head = index
            .read_image_bytes(f)
            .map(|b| {
                let mut s = String::new();
                for x in b.iter().take(16) {
                    s.push_str(&format!("{x:02x} "));
                }
                s
            })
            .unwrap_or_default();
        println!("  {cgsys:<12} {dims:<12} {n:>9}B  {f}   [{head}]");
    }
    for f in bad.iter().take(40) {
        println!("  解不出: {f}");
    }

    println!("\n=== 注册表侧:cg_registry {} 条 ===", vm.cg_registry_len());
    let snap = vm.cg_registry_snapshot();
    println!("  其中带 FILE 的 {} 条", snap.iter().filter(|s| s.3.is_some()).count());
}
