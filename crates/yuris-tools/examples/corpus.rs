//! 语料扫描探针:标签检索 + CG 文件字面量扫描(通用,不假设具体游戏)。
//!
//! 子命令:
//!   labels <游戏目录> <子串>        YSLB 标签名包含子串者(名字/脚本/pc)
//!   cgfiles <游戏目录> [脚本号]      扫描 CG 命令组中「字面量字符串」槽,
//!                                  按槽位归类打印(找图像注册点)
//!   windows <游戏目录> [脚本号]     扫描 WINDOW 命令组的首槽字面量
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example corpus -- labels <dir> es.S.GO

use std::collections::BTreeMap;

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let sub = args.next().unwrap_or_default();
    let dir = std::path::PathBuf::from(args.next().unwrap_or_default());

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
    let pack = packs
        .iter()
        .find(|p| {
            yuris_format::ypf::YpfIndex::from_path_auto(p)
                .map(|(idx, _, _)| idx.entries.iter().any(|e| is_script_entry(&e.name)))
                .unwrap_or(false)
        })
        .cloned()
        .expect("找不到脚本包");
    let mut host = YpfScriptHost::open(&pack).expect("打开脚本包");

    match sub.as_str() {
        "gb0" => {
            // 打印指定 (gparam, 窗口数) 的 GOSUB 组的 B0 槽位清单(取样)——
            // 判定 gparam 与槽位分配的对应关系。
            let want_gp = args.next().and_then(|s| s.parse().ok()).unwrap_or(6u32);
            let want_wc = args.next().and_then(|s| s.parse().ok()).unwrap_or(7u32);
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut shown = 0usize;
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let Some(name) = profile.as_ref().and_then(|p| p.name_of(g.command_type as u16))
                    else {
                        continue;
                    };
                    if name != "GOSUB" && name != "GO" {
                        continue;
                    }
                    if g.param as u32 != want_gp || g.window_count as u32 != want_wc {
                        continue;
                    }
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    let b0: Vec<String> =
                        wins.iter().map(|w| format!("{:02x}", w.tag & 0xff)).collect();
                    println!(
                        "s{sid} g{gi} {name} wc{} gp0x{:04x} B0=[{}]",
                        g.window_count,
                        g.param,
                        b0.join(" ")
                    );
                    shown += 1;
                    if shown >= 12 {
                        println!("--- 取样 {shown} 个 ---");
                        return;
                    }
                }
            }
            println!("--- 取样 {shown} 个 ---");
        }
        "decl" => {
            // 找某变量的**全部声明窗**(跨脚本)—— 判定「同一变量被不同脚本
            // 以不同形态声明」这类冲突。
            let spec = args.next().unwrap_or_default();
            let (prefix, hex) = match spec.chars().next() {
                Some('@') => (0x40u8, spec[1..].to_string()),
                Some('$') => (0x24u8, spec[1..].to_string()),
                Some('`') => (0x60u8, spec[1..].to_string()),
                Some('#') => (0x23u8, spec[1..].to_string()),
                _ => (0x40u8, spec.clone()),
            };
            let id = u16::from_str_radix(hex.trim_start_matches("0x"), 16).unwrap_or(0);
            let _needle: Vec<u8> = {
                let mut v = vec![prefix];
                v.extend_from_slice(&id.to_le_bytes());
                v
            };
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut n = 0usize;
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let name = profile
                        .as_ref()
                        .and_then(|p| p.name_of(g.command_type as u16))
                        .unwrap_or("?");
                    if !name.contains("INT") && !name.contains("FLT") && !name.contains("STR") {
                        continue;
                    }
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    // 只看**声明窗**(窗 0,且首指令是以本变量开头的引用窗)
                    for (wi, w) in wins.iter().enumerate() {
                        if w.tag & 0xff != 0 {
                            continue;
                        }
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        let Ok(instrs) = yuris_script::decode_window(&b) else { continue };
                        let lead = match instrs.first().map(|i| &i.kind) {
                            Some(yuris_script::Insn::PushVar(r))
                            | Some(yuris_script::Insn::PushVarRef(r))
                            | Some(yuris_script::Insn::PushVarIndexed(r)) => {
                                (r.space.prefix(), r.id)
                            }
                            _ => continue,
                        };
                        if lead != (prefix, id) {
                            continue;
                        }
                        let is_arr = instrs
                            .iter()
                            .any(|i| matches!(i.kind, yuris_script::Insn::ArrayLoad { .. }));
                        println!(
                            "s{sid} g{gi} {name} 窗{wi} 窗数{} {} 指令数{}",
                            g.window_count,
                            if is_arr { "数组" } else { "标量" },
                            instrs.len()
                        );
                        n += 1;
                    }
                }
            }
            println!("--- {spec} 共 {n} 处声明窗 ---");
        }
        "gmap" => {
            // (gparam, 窗口数, int_max, flt_max, str_max, nargs) 组合直方图 ——
            // 判定 gparam 的真实编码(位图? 计数? 最大槽号?)。
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut hist: BTreeMap<(u32, u32, u32, u32, u32, u32), usize> = BTreeMap::new();
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let Some(name) = profile.as_ref().and_then(|p| p.name_of(g.command_type as u16))
                    else {
                        continue;
                    };
                    if name != "GOSUB" && name != "GO" {
                        continue;
                    }
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    let (mut mi, mut mf, mut ms, mut n) = (0u32, 0u32, 0u32, 0u32);
                    for w in wins {
                        let b0 = (w.tag & 0xff) as u32;
                        match b0 {
                            0x01..=0x0f => { mi = mi.max(b0); n += 1; }
                            0x10..=0x1f => { mf = mf.max(b0 - 0x10); n += 1; }
                            0x20..=0x2f => { ms = ms.max(b0 - 0x20); n += 1; }
                            _ => {}
                        }
                    }
                    *hist.entry((g.param as u32, g.window_count as u32, mi, mf, ms, n))
                        .or_default() += 1;
                }
            }
            println!("gparam  wc  int flt str nargs  count   (判据: gparam==? )");
            for ((gp, wc, mi, mf, ms, n), c) in hist.iter() {
                println!(
                    "0x{gp:04x}  {wc:2}  {mi:3} {mf:3} {ms:3} {n:5}  {c:6}   \
                     [=nargs:{} =wc-1:{} =max:{} =sum_slots:{}]",
                    if *gp == *n { "Y" } else { "n" },
                    if *gp == wc.saturating_sub(1) { "Y" } else { "n" },
                    if *gp == (*mi).max(*mf).max(*ms) { "Y" } else { "n" },
                    if *gp == mi + mf + ms { "Y" } else { "n" },
                );
            }
        }
        "gparam" => {
            // 扫描全部 GOSUB:gparam(part1[2:4]) 与「实参窗槽位分布」的关系。
            // 目的:判定该引擎版本 gparam 是「维数位图」还是「实参计数」。
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut rows = 0usize;
            let mut extra = 0usize;
            let mut hi_nonzero = 0usize;
            let mut hist: BTreeMap<u32, usize> = BTreeMap::new();
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let Some(name) = profile.as_ref().and_then(|p| p.name_of(g.command_type as u16))
                    else {
                        continue;
                    };
                    if name != "GOSUB" && name != "GO" {
                        continue;
                    }
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    let (mut mi, mut mf, mut ms, mut n) = (0u32, 0u32, 0u32, 0u32);
                    for w in wins {
                        let b0 = (w.tag & 0xff) as u32;
                        match b0 {
                            0x01..=0x0f => { mi = mi.max(b0); n += 1; }
                            0x10..=0x1f => { mf = mf.max(b0 - 0x10); n += 1; }
                            0x20..=0x2f => { ms = ms.max(b0 - 0x20); n += 1; }
                            _ => {}
                        }
                    }
                    rows += 1;
                    let gp = g.param as u32;
                    *hist.entry(gp).or_default() += 1;
                    if gp >> 8 != 0 {
                        hi_nonzero += 1;
                        if hi_nonzero <= 8 {
                            println!(
                                "高字节非零 s{sid} g{gi} {name} wc{} gparam=0x{gp:04x} \
                                 int_max={mi} flt_max={mf} str_max={ms} nargs={n}",
                                g.window_count
                            );
                        }
                    }
                    // 「gparam 高位=维数位图」解码 vs 实际槽位
                    let bm_i = (gp & 0xff) >> 3;
                    let bm_f = (gp & 7) * 4 + (gp >> 14);
                    let bm_s = (gp >> 9) & 0x1f;
                    if mi > bm_i || mf > bm_f || ms > bm_s {
                        extra += 1;
                        if extra <= 12 {
                            println!(
                                "位图不足 s{sid} g{gi} {name} gparam=0x{gp:04x} \
                                 (位图 int{bm_i}/flt{bm_f}/str{bm_s} vs 实际 {mi}/{mf}/{ms}, nargs={n})"
                            );
                        }
                    }
                }
            }
            println!("--- GOSUB/GO 组 {rows} 个;高位非零 {hi_nonzero};位图不足 {extra} ---");
            let top: Vec<String> = hist.iter().take(20).map(|(k, v)| format!("0x{k:04x}={v}")).collect();
            println!("gparam 取值(前 20): {}", top.join(" "));
        }
        "params" => {
            // 全量命令 × 参数名 TSV(序号\t命令名\t槽\t参数名\t类型)
            let blob = find_boot(&mut host, "ysbin\\ysc.ybn").expect("ysc.ybn");
            let t = yuris_format::yscm::YscmTable::from_bytes(&blob).expect("解析 YSCM");
            println!("# YSCM v{} commands={}", t.version(), t.commands().len());
            for (i, c) in t.commands().iter().enumerate() {
                println!("{i}\t{}\t-\t-\t-", c.name);
                for (pi, p) in c.params.iter().enumerate() {
                    println!("{i}\t{}\t{pi}\t{}\t0x{:04x}", c.name, p.name, p.ty);
                }
            }
        }
        "findlit" => {
            // 全语料扫「含指定子串的 pushstr 字面量」→ 打印 脚本/pc/命令/槽/原始窗口
            let needle = args.next().unwrap_or_default().to_ascii_lowercase();
            let only: Option<u16> = args.next().and_then(|s| s.parse().ok());
            let profile = load_profile(&mut host);
            let ids = if let Some(s) = only {
                vec![s]
            } else {
                host.script_ids().expect("脚本号列表")
            };
            let mut hits = 0usize;
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let name = profile
                        .as_ref()
                        .and_then(|p| p.name_of(g.command_type as u16))
                        .unwrap_or("?")
                        .to_string();
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for w in wins {
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        // 该窗口是否含 pushstr(0x4d) 且字面量含 needle
                        let mut i = 0usize;
                        while i + 4 <= b.len() {
                            if b[i] == 0x4d {
                                let len = u16::from_le_bytes([b[i + 1], b[i + 2]]) as usize;
                                if b[i + 3] == 0x22 && i + 4 + len <= b.len() {
                                    let s = String::from_utf8_lossy(&b[i + 4..i + 4 + len])
                                        .to_ascii_lowercase();
                                    if s.contains(&needle) {
                                        let hex: String = b
                                            .iter()
                                            .map(|x| format!("{x:02x}"))
                                            .collect();
                                        println!(
                                            "s{sid} g{gi} {name} 槽{} t{:x} :: {hex}",
                                            w.tag & 0xff,
                                            w.tag
                                        );
                                        hits += 1;
                                        break;
                                    }
                                }
                            }
                            i += 1;
                        }
                    }
                }
            }
            println!("--- {hits} 处 ---");
        }
        "space" => {
            // 全语料扫「指定变量空间(前缀字节)的全部引用点」——
            // 用于判定某空间是否被**声明**过(0x76 DeclVar)、被**写**过
            // (0x56 引用,赋值左值)还是只被**读**(0x48)。
            // 扫法:指令头为 op(1)+len(u16 LE)+operand;变量族 operand[0]=空间字节。
            let space = args
                .next()
                .and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0);
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut total = 0usize;
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let name = profile
                        .as_ref()
                        .and_then(|p| p.name_of(g.command_type as u16))
                        .unwrap_or("?")
                        .to_string();
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for (wi, w) in wins.iter().enumerate() {
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        let mut i = 0usize;
                        while i + 4 <= b.len() {
                            let op = b[i];
                            let len = u16::from_le_bytes([b[i + 1], b[i + 2]]) as usize;
                            if matches!(op, 0x48 | 0x56 | 0x76) && len == 3 && b[i + 3] == space {
                                let id = u16::from_le_bytes([b[i + 4], b[i + 5]]);
                                let kind = match op {
                                    0x48 => "读",
                                    0x56 => "引用",
                                    _ => "声明",
                                };
                                *counts.entry(format!("{kind}")).or_default() += 1;
                                total += 1;
                                if total <= 60 {
                                    println!(
                                        "s{sid} g{gi} {name} 窗{wi} {kind} -> {space:02x}:{id:04x}"
                                    );
                                }
                                i += 6;
                                continue;
                            }
                            i += 1;
                        }
                    }
                }
            }
            let mut summary: Vec<String> =
                counts.iter().map(|(k, v)| format!("{k}={v}")).collect();
            summary.sort();
            println!("--- 空间 {space:02x} 共 {total} 处:{} ---", summary.join(" "));
        }
        "xspace" => {
            // 跨前缀 id 冲突扫描:**同一 id 是否被多个前缀字符引用**——
            // 判定「变量身份 = id 单独」还是「(前缀,id)」。
            // 引擎侧证据:描述符表 DAT_0087240c[id](无前缀维)、YSVR 条目无前缀字段。
            // 用**真解码器**(decode_window)而非字节扫描 —— 避免字面量里的偶合。
            let ids = host.script_ids().expect("脚本号列表");
            let mut map: BTreeMap<u16, BTreeMap<u8, usize>> = BTreeMap::new();
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for w in wins {
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        let Ok(instrs) = yuris_script::decode_window(&b) else { continue };
                        for ins in &instrs {
                            let r = match &ins.kind {
                                yuris_script::Insn::PushVar(r)
                                | yuris_script::Insn::PushVarRef(r)
                                | yuris_script::Insn::PushVarIndexed(r) => *r,
                                _ => continue,
                            };
                            *map.entry(r.id).or_default().entry(r.space.prefix()).or_default() += 1;
                        }
                    }
                }
            }
            let mut conflicts = 0usize;
            let mut shown = 0usize;
            for (id, sps) in &map {
                if sps.len() < 2 {
                    continue;
                }
                conflicts += 1;
                if shown < 60 {
                    let desc: Vec<String> =
                        sps.iter().map(|(s, c)| format!("{s:02x}×{c}")).collect();
                    println!("id {id:04x} 前缀 {}", desc.join(" "));
                    shown += 1;
                }
            }
            println!(
                "--- 共 {} 个 id 被引用;跨前缀冲突 {conflicts} 个 ---",
                map.len()
            );
        }
        "var" => {
            // 某变量 id 的**全部触碰点**(真解码):声明/写(窗0 左值)与读。
            // 判定「该 id 的真正声明命令与类型」—— 字节扫描会撞字面量偶合。
            let id = args
                .next()
                .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0);
            // 可选:只看某前缀(十六进制)的触碰点。
            let pfx: Option<u8> = args
                .next()
                .and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok());
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut decl = 0usize;
            let mut lv = 0usize;
            let mut rd = 0usize;
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let name = profile
                        .as_ref()
                        .and_then(|p| p.name_of(g.command_type as u16))
                        .unwrap_or("?")
                        .to_string();
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for (wi, w) in wins.iter().enumerate() {
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        let Ok(instrs) = yuris_script::decode_window(&b) else { continue };
                        let mut hits: Vec<&str> = Vec::new();
                        let mut is_lv = false;
                            for ins in &instrs {
                                let (r, tag) = match &ins.kind {
                                    yuris_script::Insn::PushVar(r) => (r, "读"),
                                    yuris_script::Insn::PushVarRef(r) => (r, "引用"),
                                    yuris_script::Insn::PushVarIndexed(r) => (r, "下标"),
                                    _ => continue,
                                };
                                if r.id != id {
                                    continue;
                                }
                                if let Some(p) = pfx {
                                    if p != r.space.prefix() {
                                        continue;
                                    }
                                }
                                hits.push(tag);
                                if tag != "读" {
                                    is_lv = true;
                                }
                            }
                        if hits.is_empty() {
                            continue;
                        }
                        // 窗 0 + 声明类命令 = 声明窗
                        let is_decl = wi == 0
                            && (name.contains("INT") || name.contains("FLT") || name.contains("STR"));
                        if is_decl {
                            decl += 1;
                        } else if is_lv {
                            lv += 1;
                        } else {
                            rd += 1;
                        }
                        if decl + lv + rd <= 80 {
                            let sp: Vec<String> = instrs
                                .iter()
                                .filter_map(|i| match &i.kind {
                                    yuris_script::Insn::PushVar(r)
                                    | yuris_script::Insn::PushVarRef(r)
                                    | yuris_script::Insn::PushVarIndexed(r)
                                        if r.id == id =>
                                    {
                                        Some(format!(
                                            "{:02x}:{}",
                                            r.space.prefix(),
                                            match &i.kind {
                                                yuris_script::Insn::PushVar(_) => "读",
                                                yuris_script::Insn::PushVarRef(_) => "引用",
                                                _ => "下标",
                                            }
                                        ))
                                    }
                                    _ => None,
                                })
                                .collect();
                            println!(
                                "s{sid} g{gi} {name} 窗{wi}/{} tag0x{:x} 指令{} [{}]{}",
                                wins.len(),
                                w.tag,
                                instrs.len(),
                                sp.join(" "),
                                if is_decl { "  ←声明窗" } else { "" }
                            );
                        }
                    }
                }
            }
            println!("--- id {id:04x}: 声明窗 {decl} / 左值 {lv} / 读 {rd} ---");
        }
        "tail41" => {
            // 扫「窗口末尾恰为 41 00 00」的窗 —— 判定它是否为编译器对
            // **缺席实参**的统一记号(0x41=bit-and 缺右操作数,无法独立求值)。
            let profile = load_profile(&mut host);
            let ids = host.script_ids().expect("脚本号列表");
            let mut n = 0usize;
            let mut by_cmd: BTreeMap<String, usize> = BTreeMap::new();
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let name = profile
                        .as_ref()
                        .and_then(|p| p.name_of(g.command_type as u16))
                        .unwrap_or("?")
                        .to_string();
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for w in wins {
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        if b.len() >= 3 && &b[b.len() - 3..] == [0x41, 0x00, 0x00] {
                            n += 1;
                            *by_cmd.entry(name.clone()).or_default() += 1;
                            if n <= 20 {
                                let hex: String =
                                    b.iter().map(|x| format!("{x:02x}")).collect();
                                println!("s{sid} g{gi} {name} 槽{} len{} :: {hex}", w.tag & 0xff, b.len());
                            }
                        }
                    }
                }
            }
            let top: Vec<String> = by_cmd.iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!("--- 末尾 41 00 00 的窗 {n} 个;按命令:{} ---", top.join(" "));
        }
        "cmd" => {
            // 命令名 → 序号 + 参数名/类型(YSCM 权威;槽位语义由此而来)
            let needle = args.next().unwrap_or_default().to_ascii_uppercase();
            let blob = find_boot(&mut host, "ysbin\\ysc.ybn").expect("ysc.ybn");
            let t = yuris_format::yscm::YscmTable::from_bytes(&blob).expect("解析 YSCM");
            println!("YSCM v{} 共 {} 命令", t.version(), t.commands().len());
            for (i, c) in t.commands().iter().enumerate() {
                if !c.name.eq_ignore_ascii_case(&needle) {
                    continue;
                }
                println!("[{i}] {} 参数 {} 个:", c.name, c.params.len());
                for (pi, p) in c.params.iter().enumerate() {
                    println!("    #{pi:2} 名字={:?} 类型=0x{:04x}", p.name, p.ty);
                }
            }
        }
        "labels" => {
            let needle = args.next().unwrap_or_default();
            let data = find_boot(&mut host, "ysbin\\ysl.ybn").expect("ysl.ybn");
            let t = yuris_format::yslb::YslbTable::from_bytes(&data).expect("解析 YSLB");
            for l in t.labels() {
                let n = String::from_utf8_lossy(&l.name);
                if n.to_ascii_lowercase().contains(&needle.to_ascii_lowercase()) {
                    println!("{n:44} → s{:<4} pc{}", l.script_id, l.target_pc);
                }
            }
        }
        _ => {
            let only: Option<u16> = args.next().and_then(|s| s.parse().ok());
            let profile = load_profile(&mut host);
            let ids = if let Some(s) = only {
                vec![s]
            } else {
                host.script_ids().expect("脚本号列表")
            };
            // 槽位 → 字面量集合
            let mut slot_lits: BTreeMap<u8, BTreeMap<String, usize>> = BTreeMap::new();
            for sid in ids {
                let Ok(ctx) = host.load(sid) else { continue };
                for (gi, g) in ctx.groups.iter().enumerate() {
                    let Some(name) = profile
                        .as_ref()
                        .and_then(|p| p.name_of(g.command_type as u16))
                    else {
                        continue;
                    };
                    let want = match sub.as_str() {
                        "cgfiles" => name.eq_ignore_ascii_case("CG"),
                        "windows" => name.eq_ignore_ascii_case("WINDOW"),
                        _ => false,
                    };
                    if !want {
                        continue;
                    }
                    let first = ctx.first_slots[gi];
                    let wins = &ctx.script.slots()[first..first + g.window_count as usize];
                    for w in wins {
                        let slot = (w.tag & 0xff) as u8;
                        let Some(b) = ctx.script.window_bytes_pooled_copy(w) else {
                            continue;
                        };
                        // 字面量 pushstr 形态:4d <len_lo> <len_hi> 22 "…"
                        if b.len() >= 4 && b[0] == 0x4d && b[3] == 0x22 {
                            let len = u16::from_le_bytes([b[1], b[2]]) as usize;
                            if b.len() >= 4 + len && len >= 2 {
                                let s = String::from_utf8_lossy(&b[4..4 + len - 1]).into_owned();
                                *slot_lits
                                    .entry(slot)
                                    .or_default()
                                    .entry(s)
                                    .or_default() += 1;
                            }
                        } else {
                            *slot_lits
                                .entry(slot)
                                .or_default()
                                .entry("<表达式>".into())
                                .or_default() += 1;
                        }
                    }
                    let _ = gi;
                }
            }
            for (slot, lits) in &slot_lits {
                println!("--- 槽 {slot} ---");
                for (s, n) in lits.iter().take(40) {
                    println!("  {n:6}× {s}");
                }
            }
        }
    }
}

fn find_boot(host: &mut YpfScriptHost, suffix: &str) -> Option<Vec<u8>> {
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase()))?;
    host.read_entry(&name).ok()
}

fn load_profile(host: &mut YpfScriptHost) -> Option<yuris_core::CommandProfile> {
    let blob = find_boot(host, "ysbin\\ysc.ybn")?;
    let table = yuris_format::yscm::YscmTable::from_bytes(&blob).ok()?;
    let version = table.version();
    let names: Vec<String> = table.commands().iter().map(|c| c.name.clone()).collect();
    Some(yuris_core::CommandProfile::from_names(names, version, "ysc.ybn"))
}
