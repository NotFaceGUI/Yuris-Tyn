//! YSVR 字符串初值(ty=3)**形状**取证探针(不执行 VM)。
//!
//! 起因:实机长期黑屏,追到 `$es.STARTLABEL`(场景开始标签)的值是
//! **一段窗口字节**而非字符串。逐层回溯后落在 YSVR 初值:
//! `id 0x040c init=Str([4d 10 00 22 "SCENARIO_START" 22])` —— 与运行期
//! 观测值逐字节相同 ⇒ 字符串初值在 YSVR 里不是裸字符串,而是**表达式
//! 指令流**(与 `.ybn` 窗口同一套编码:`4d`=PushStr + u16 长度 + 带定界符
//! 的字面量)。
//!
//! 本探针统计全表 ty=3 条目的形状:首字节直方图 / 能否按指令流解码 /
//! 解码后是否恰为单条 PushStr / 解码得到的字符串。用于判定修复口径
//! (是"单条字面量"还是"任意表达式"必须全量求值)。
//!
//! 用法:
//!   cargo run -p yuris-tools --release --example ysvr_strinit -- <游戏目录> [样例上限]

use std::collections::BTreeMap;

use yuris_format::ysvr::{YsvrInit, YsvrTable};
use yuris_vm::host::{is_script_entry, YpfScriptHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: ysvr_strinit <游戏目录>"));
    let show: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);

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
        .expect("没有包含脚本条目的 ypf");

    let mut host = YpfScriptHost::open(&script_pack).expect("打开脚本包");
    let raw = host.read_entry("ysbin\\ysv.ybn").expect("读 ysv.ybn");
    let table = YsvrTable::from_bytes(&raw).expect("解析 YSVR");

    let mut total = 0usize;
    let mut empty = 0usize;
    let mut nonempty = 0usize;
    let mut first_byte: BTreeMap<u8, usize> = BTreeMap::new();
    let mut decodable = 0usize;
    let mut single_pushstr = 0usize;
    let mut other_shape = 0usize;
    let mut samples: Vec<String> = Vec::new();
    let mut weird: Vec<String> = Vec::new();

    for e in table.entries() {
        if e.ty != 3 {
            continue;
        }
        total += 1;
        let YsvrInit::Str(b) = &e.init else { continue };
        if b.is_empty() {
            empty += 1;
            continue;
        }
        nonempty += 1;
        *first_byte.entry(b[0]).or_default() += 1;
        match yuris_script::decode_window(b) {
            Ok(instrs) => {
                decodable += 1;
                let is_push = instrs.len() == 1
                    && matches!(instrs[0].kind, yuris_script::Insn::PushStr(_));
                if is_push {
                    single_pushstr += 1;
                    if let yuris_script::Insn::PushStr(p) = &instrs[0].kind {
                        let s = yuris_script::eval::decode_pushstr(p);
                        let txt = String::from_utf8_lossy(&s).into_owned();
                        if samples.len() < show {
                            samples.push(format!(
                                "id {:#06x} kind={} script={} 原始 {}B → 解码 {:?}",
                                e.var_id, e.kind, e.script, b.len(), txt
                            ));
                        }
                    }
                } else {
                    other_shape += 1;
                    if weird.len() < show {
                        weird.push(format!(
                            "id {:#06x} 指令 {} 条: {:?}",
                            e.var_id,
                            instrs.len(),
                            instrs.iter().map(|i| format!("{:?}", i.kind)).collect::<Vec<_>>()
                        ));
                    }
                }
            }
            Err(err) => {
                other_shape += 1;
                if weird.len() < show {
                    weird.push(format!(
                        "id {:#06x} 解码失败 {err}; 首 24B {:02x?}",
                        e.var_id,
                        &b[..b.len().min(24)]
                    ));
                }
            }
        }
    }

    println!("=== YSVR ty=3(字符串)初值形状 ===");
    println!(
        "ty=3 共 {total} · 无初值(空/None) {empty} · 有初值 {nonempty}"
    );
    println!("有初值中:可解码 {decodable} · 单条 PushStr {single_pushstr} · 其他形态 {other_shape}");
    println!("首字节直方图: {first_byte:?}   (0x4d = PushStr 指令)");
    println!("\n=== 单条 PushStr 样例(解码结果)===");
    for s in samples.iter() {
        println!("  {s}");
    }
    if !weird.is_empty() {
        println!("\n=== 非「单条 PushStr」形态(需全量求值或另行取证)===");
        for s in weird.iter() {
            println!("  {s}");
        }
    }
}
