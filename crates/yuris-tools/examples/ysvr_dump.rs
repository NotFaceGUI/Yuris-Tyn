//! YSVR 全表探针:条目字段直方图 + 同一 var_id 多条目检测。
//!
//! 用途:判定「变量身份 = id 还是 (前缀,id)」——
//! 若 YSVR 里同一 var_id 出现多次(不同 kind/category/ty),说明 id 不是全局唯一,
//! 必有一个字段充当命名空间;该字段即 EEPROM 前缀字节在引擎侧的对应物。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example ysvr_dump -- <游戏目录> [id...]

use std::collections::BTreeMap;

use yuris_vm::host::{is_script_entry, ScriptHost, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: ysvr_dump <游戏目录> [id...]"));
    let wants: Vec<u16> = args
        .filter_map(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .collect();

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
    let name = host
        .entry_names()
        .into_iter()
        .find(|n| n.to_ascii_lowercase().ends_with("ysbin\\ysv.ybn"))
        .expect("ysv.ybn 条目");
    let blob = host.read_entry(&name).expect("读 ysv.ybn");
    let t = yuris_format::ysvr::YsvrTable::from_bytes(&blob).expect("解析 YSVR");

    println!(
        "YSVR v{} 条目 {} 闭合 {}B",
        t.version(),
        t.entries.len(),
        t.consumed
    );

    // 字段直方图
    let mut by_kind: BTreeMap<u8, usize> = BTreeMap::new();
    let mut by_cat: BTreeMap<u8, usize> = BTreeMap::new();
    let mut by_ty: BTreeMap<u8, usize> = BTreeMap::new();
    for e in t.entries() {
        *by_kind.entry(e.kind).or_default() += 1;
        *by_cat.entry(e.category).or_default() += 1;
        *by_ty.entry(e.ty).or_default() += 1;
    }
    let fmt = |m: &BTreeMap<u8, usize>| {
        m.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!("kind: {}", fmt(&by_kind));
    println!("category: {}", fmt(&by_cat));
    println!("ty: {}", fmt(&by_ty));

    // id → 条目集合(kind,category,ty)
    let mut by_id: BTreeMap<u16, Vec<(u8, u8, u8, usize)>> = BTreeMap::new();
    for e in t.entries() {
        by_id
            .entry(e.var_id)
            .or_default()
            .push((e.kind, e.category, e.ty, e.bounds.len()));
    }
    let dup: Vec<(&u16, &Vec<(u8, u8, u8, usize)>)> =
        by_id.iter().filter(|(_, v)| v.len() > 1).collect();
    let idmin = by_id.keys().next().copied().unwrap_or(0);
    let idmax = by_id.keys().next_back().copied().unwrap_or(0);
    println!("var_id 范围 {idmin}..{idmax} (0x{idmin:04x}..0x{idmax:04x})");
    println!(
        "不同 var_id {} 个 / 条目 {} 条 / 多条目 id {} 个",
        by_id.len(),
        t.entries().len(),
        dup.len()
    );

    // 「同 id 不同 ty」= id 非全局唯一的直接证据
    let mut ty_conflict = 0usize;
    for (id, v) in &by_id {
        let tys: std::collections::BTreeSet<u8> = v.iter().map(|x| x.2).collect();
        if tys.len() > 1 {
            ty_conflict += 1;
            if ty_conflict <= 20 {
                println!("  id {id:04x} 类型冲突 {}", format!("{v:?}"));
            }
        }
    }
    println!("--- 同 id 不同 ty 的 id: {ty_conflict} 个 ---");

    // category × ty 联合分布(id 是否需要前缀来解释)
    let mut ct: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    for e in t.entries() {
        *ct.entry((e.category, e.ty)).or_default() += 1;
    }
    println!("category×ty:");
    for ((c, ty), n) in &ct {
        println!("  category={c} ty={ty} → {n}");
    }

    println!("--- category>=2 的全部条目(罕见类别)---");
    for e in t.entries().iter().filter(|e| e.category >= 2) {
        println!(
            "  id {:04x} kind={} category={} script={} ty={} bounds={:?} init={:?}",
            e.var_id, e.kind, e.category, e.script, e.ty, e.bounds, e.init
        );
    }

    // 数组条目里「初值 ≠ 类型默认值」的清单。
    //
    // 这些条目是 `apply_ysvr_entry` 必须**填满整个数组**的直接依据:脚本按
    // 任意下标读取时都期望拿到该初值。实测 v494 的 `@2926..@2936`
    // (s60 `es.SND.PLAY` 的每声道参数表)全在此列。
    let is_default = |e: &yuris_format::ysvr::YsvrEntry| match &e.init {
        yuris_format::ysvr::YsvrInit::Int(0) => true,
        yuris_format::ysvr::YsvrInit::Float(f) => *f == 0.0,
        yuris_format::ysvr::YsvrInit::Str(s) => s.is_empty(),
        _ => false,
    };
    let arrs: Vec<_> = t
        .entries()
        .iter()
        .filter(|e| !e.bounds.is_empty() && !is_default(e))
        .collect();
    println!("--- 数组 + 非默认初值:{} 条 ---", arrs.len());
    for e in &arrs {
        println!(
            "  id {:04x} kind={} category={} ty={} bounds={:?} init={:?}",
            e.var_id, e.kind, e.category, e.ty, e.bounds, e.init
        );
    }

    // 指定 id 的完整条目
    for id in wants {        match by_id.get(&id) {
            Some(v) => {
                for (kind, cat, ty, dims) in v {
                    let e = t
                        .entries()
                        .iter()
                        .find(|e| {
                            e.var_id == id
                                && e.kind == *kind
                                && e.category == *cat
                                && e.ty == *ty
                        })
                        .unwrap();
                    println!(
                        "id {id:04x}: kind={kind} category={cat} script={} ty={ty} dims={dims} bounds={:?} init={:?}",
                        e.script, e.bounds, e.init
                    );
                }
            }
            None => println!("id {id:04x}: 无条目"),
        }
    }
}
