//! 只读探针:**场景层的可见性/坐标最终态**(黑屏与"画面被盖"定位用)。
//!
//! 与 `screenstate` 的差别:那个看 VM 注册表与纹理可解性;本探针复刻
//! **播放器侧的场景构建**(含 `vm_ui_layer_allowed` 门控、E 显隐、
//! COPY 别名),打印**最终会上屏的层**——回答「屏幕上到底画了什么、谁盖住谁」。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example scenetrace -- <游戏目录> [节拍数] [名字过滤]

use yuris_vm::boot::Bootstrap;
use yuris_vm::host::{is_script_entry, PacFileIndex, YpfScriptHost};
use yuris_vm::{EffectValue, VmEvent, VmSuspend};

fn fnv1a(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// 复刻播放器的 UI 门控(与 `yuris-player-core::vm_ui_layer_allowed` 同式)。
fn ui_allowed(path_lower: &str, builtin_title: bool) -> bool {
    let norm = path_lower.replace('/', "\\").replace("cgsys_c\\", "cgsys\\");
    if norm.contains(r"\debug\") {
        return false;
    }
    if !builtin_title {
        return true;
    }
    if norm.contains(r"cgsys\title\") || norm.contains(r"cgsys\extra\") {
        return false;
    }
    if norm.contains(r"cgsys\main\pop\")
        || norm.contains(r"cgsys\main\autoskipicon\")
        || norm.contains(r"cgsys\main\skipicon\")
        || norm.contains(r"cgsys\main\autoicon\")
        || norm.contains(r"cgsys\main\count\")
    {
        return false;
    }
    norm.contains(r"cgsys\")
}

/// 一层的最终态(复刻播放器持久层语义:未给槽保持原值)。
#[derive(Default, Clone)]
struct L {
    name: String,
    file: String,
    visible: bool,
    x: f32,
    y: f32,
    z: i64,
    alpha: f32,
    alias: Option<u64>,
    parent: Option<u64>,
    group: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: scenetrace <dir> [ticks] [filter]"));
    let ticks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(300);
    let filter: Option<String> = args.next();

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

    // 复刻播放器状态:id → 层
    let mut layers: std::collections::HashMap<u64, L> = std::collections::HashMap::new();
    let mut alias_wanted: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut builtin_title = false;

    for tick in 0..=ticks {
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
        for e in vm.events().iter() {
            match e {
                VmEvent::Cg { id, file, e: ev_e, x, y, z, a, sx, sy, .. } => {
                    let Some(name) = id else { continue };
                    let lower = file
                        .as_ref()
                        .map(|f| String::from_utf8_lossy(f).to_ascii_lowercase())
                        .unwrap_or_default();
                    let lid = fnv1a(name.as_bytes());
                    let exists = layers.contains_key(&lid);
                    // 门控(与播放器同式;无 scenario 的场景 builtin_title 会翻转)
                    let is_cgsys = lower.contains("cgsys");
                    if is_cgsys && !ui_allowed(&lower, builtin_title) {
                        continue;
                    }
                    if !lower.is_empty() && !is_cgsys && (lower.contains("debug") || lower.contains("cgsys2")) {
                        continue;
                    }
                    if file.is_none() && !exists {
                        continue; // 纯 patch 未建层
                    }
                    let group_size = match (sx, sy) {
                        (Some(w), Some(h)) if *w > 0 && *h > 0 => Some((*w as f32, *h as f32)),
                        _ => None,
                    };
                    // 先算父(要读 layers),再取可变借用 —— 借用顺序不能反。
                    let parent = parent_of(name.as_bytes(), &layers);
                    let ent = layers.entry(lid).or_default();
                    ent.name = name.clone();
                    if let Some(f) = file {
                        let s = String::from_utf8_lossy(f).into_owned();
                        if !s.is_empty() {
                            ent.file = s;
                        }
                    }
                    ent.visible = match ev_e {
                        Some(0) => false,
                        Some(_) => true,
                        None => if exists { ent.visible } else { true },
                    };
                    if let Some(v) = x {
                        ent.x = *v as f32;
                    }
                    if let Some(v) = y {
                        ent.y = *v as f32;
                    }
                    if let Some(v) = z {
                        ent.z = *v;
                    }
                    if let Some(v) = a {
                        ent.alpha = (*v as f32 / 256.0).clamp(0.0, 1.0);
                    }
                    if group_size.is_some() {
                        ent.group = true;
                    }
                    if let Some(al) = alias_wanted.get(&lid) {
                        ent.alias = Some(*al);
                    }
                    ent.parent = parent;
                }
                VmEvent::CgAct { id, effects, .. } => {
                    let has_copy = effects.iter().any(|(_, n, v)| {
                        (n.eq_ignore_ascii_case("COPY") || n.eq_ignore_ascii_case("COPY2"))
                            && match v {
                                EffectValue::Int(x) => *x != 0,
                                EffectValue::Float(f) => *f != 0.0,
                                EffectValue::Str(s) => !s.is_empty(),
                            }
                    });
                    if has_copy {
                        let src = effects.iter().find_map(|(_, n, v)| {
                            (n.eq_ignore_ascii_case("ID2")).then_some(v).and_then(|v| match v {
                                EffectValue::Str(s) => Some(s.clone()),
                                _ => None,
                            })
                        });
                        if let (Some(dst_name), Some(src)) = (id, src) {
                            let dst = fnv1a(dst_name.as_bytes());
                            let s = fnv1a(src.as_bytes());
                            alias_wanted.insert(dst, s);
                            if let Some(l) = layers.get_mut(&dst) {
                                l.alias = Some(s);
                            }
                        }
                    }
                    // XY.SET 同步
                    if let Some(name) = id {
                        if let Some((x, y)) = vm.cg_position(name) {
                            if let Some(l) = layers.get_mut(&fnv1a(name.as_bytes())) {
                                l.x = x as f32;
                                l.y = y as f32;
                            }
                        }
                    }
                }
                VmEvent::CgEnd { id, .. } => {
                    if let Some(name) = id {
                        if let Some(l) = layers.get_mut(&fnv1a(name.as_bytes())) {
                            l.visible = false;
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
        // 标题期门控翻转(播放器按 @1029 判;这里用环境开关模拟)
        if tick == 0 {
            if let Ok(v) = vm.store().get(&yuris_value::VarRef { space: yuris_value::VarSpace::At, id: 1029 }) {
                if let yuris_value::Value::Int(n) = &v {
                    builtin_title = *n >= 200;
                }
            }
        }
    }

    // 打印
    let name_of = |id: u64| layers.get(&id).map(|l| l.name.clone()).unwrap_or_default();
    // `YURIS_SCENE_ALL=1` → 连不可见层一起列(找「层压根没建」还是「建了被隐藏」)。
    let all = std::env::var_os("YURIS_SCENE_ALL").is_some();
    let mut vis: Vec<(i64, u64, &L)> = layers
        .iter()
        .filter(|(_, l)| l.visible || all)
        .filter(|(_, l)| {
            filter
                .as_deref()
                .is_none_or(|f| l.name.contains(f) || l.file.contains(f))
        })
        .map(|(id, l)| (l.z, *id, l))
        .collect();
    vis.sort_by_key(|(z, id, _)| (*z, *id));
    println!("== 可见层 {} / 总层 {} (过滤 {:?}) ==", vis.len(), layers.len(), filter);
    for (z, id, l) in &vis {
        let kind = if !l.visible {
            "隐"
        } else if l.group {
            "组"
        } else if let Some(a) = l.alias {
            return_marker(&name_of(a))
        } else if !l.file.is_empty() {
            "图"
        } else {
            "空"
        };
        println!(
            "  z={:<22} {:<6} [{id:016x}] {:<46} file={:<38} @({},{}) a={:.2} parent={}",
            z,
            kind,
            l.name,
            l.file,
            l.x as i32,
            l.y as i32,
            l.alpha,
            l.parent.map(|p| name_of(p)).unwrap_or_else(|| "-".into())
        );
        let _ = id;
    }
}

fn return_marker(s: &str) -> &'static str {
    if s.is_empty() {
        "别"
    } else {
        "别"
    }
}

/// 名字前缀推父(与播放器 `layer_parent_of` 同式)。
fn parent_of(name: &[u8], layers: &std::collections::HashMap<u64, L>) -> Option<u64> {
    let mut cut = name.len();
    while let Some(pos) = name[..cut].iter().rposition(|&c| c == b'/') {
        if pos == 0 {
            break;
        }
        let pid = fnv1a(&name[..=pos]);
        if layers.get(&pid).map(|l| l.group).unwrap_or(false) {
            return Some(pid);
        }
        cut = pos;
    }
    None
}
