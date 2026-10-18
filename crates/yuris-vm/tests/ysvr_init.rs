//! YSVR 初值语义回归:字符串初值(`ty=3`)是**表达式指令流**,必须解码求值。
//!
//! 实机背景(成果 119):本作 v494 冷启动恒黑屏。逐层回溯到
//! `es.SCENARIO.FLOW`(s21 g8)的 `GO[#=$es.STARTLABEL]` —— 它的目标串来自
//! YSVR 全局初值,而旧实现把该字段当**裸字符串**拷贝,于是值变成
//! `4d 10 00 22 "SCENARIO_START" 22`(19 字节窗口字节)→ 标签查不到 →
//! 场景任务空转、一张图都不画。
//!
//! 真值来源:
//! - 官方 SDK 0.495 示例源码 `ERIS/global.yst:63`
//!   `G_STR[$es.STARTLABEL="SCENARIO_START"]` —— 期望值 = `SCENARIO_START`;
//! - 本作 `ysbin\ysv.ybn` `id 0x040c`(kind=1 全局初值)`ty=3` 原始字段
//!   = `4d 10 00 22 "SCENARIO_START" 22`,与脚本窗口里同一字面量的编码逐字节一致;
//! - 全表形状(`examples/ysvr_strinit`):`ty=3` 306 条 → 有初值 28 条,
//!   **28/28 首字节 0x4d、28/28 解码为单条 PushStr**、0 条其他形态。

use yuris_format::ysvr::YsvrTable;
use yuris_format::ystb::YstbFile;
use yuris_value::{VarRef, VarSpace, Value};
use yuris_vm::cmd;
use yuris_vm::GroupVm;

mod common;
use common::{make_ystb, SAMPLE_KEY};

fn var(space: u8, id: u16) -> VarRef {
    VarRef { space: VarSpace::from_prefix(space), id }
}

/// 构造最小 YSVR:header + 条目。`init` = **字段原始字节**
/// (`ty=3` 时函数补 u16 长度;`ty=1/2` 为裸 8 字节)。
fn ysvr_bytes(entries: &[(u8, u8, u16, u16, u8, Vec<u8>)]) -> Vec<u8> {
    ysvr_bytes_with_bounds(
        &entries
            .iter()
            .map(|e| (e.clone(), Vec::new()))
            .collect::<Vec<_>>(),
    )
}

/// 同上,但带数组边界(`bounds` 空 = 标量)。
fn ysvr_bytes_with_bounds(
    entries: &[((u8, u8, u16, u16, u8, Vec<u8>), Vec<u32>)],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"YSVR");
    out.extend_from_slice(&494u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for ((kind, category, script, var_id, ty, init), bounds) in entries {
        out.push(*kind);
        out.push(*category);
        out.extend_from_slice(&script.to_le_bytes());
        out.extend_from_slice(&var_id.to_le_bytes());
        out.push(*ty);
        out.push(bounds.len() as u8);
        for b in bounds {
            out.extend_from_slice(&b.to_le_bytes());
        }
        match ty {
            1 | 2 => out.extend_from_slice(init),
            3 => {
                out.extend_from_slice(&(init.len() as u16).to_le_bytes());
                out.extend_from_slice(init);
            }
            _ => {}
        }
    }
    out
}

/// 最小可装载 VM(单组 `RETURN`,不执行任何命令)。
fn make_vm() -> GroupVm {
    let groups: &[(u8, u8, u16)] = &[(cmd::RETURN, 0, 0)];
    let windows: &[([u8; 4], u32, u32, &[u8])] = &[];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    GroupVm::load(script).unwrap()
}

/// 本作真实样本:`$es.STARTLABEL`(YSVR id 0x040c,kind=1)必须解出
/// `SCENARIO_START`,而**不是**其窗口字节。
#[test]
fn ysvr_string_initializer_is_an_expression_stream_real_sample() {
    // 原始字段(= 本作 ysv.ybn 实测,19 字节;与脚本窗口编码同型)
    let raw_field: Vec<u8> = vec![
        0x4d, 0x10, 0x00, 0x22, b'S', b'C', b'E', b'N', b'A', b'R', b'I', b'O', b'_',
        b'S', b'T', b'A', b'R', b'T', 0x22,
    ];
    let table = YsvrTable::from_bytes(&ysvr_bytes(&[(1, 1, 0, 0x040c, 3, raw_field.clone())]))
        .expect("YSVR 解析");
    let mut vm = make_vm();
    vm.apply_ysvr(&table).expect("应用初值");

    let got = vm.store().get(&var(b'$', 0x040c)).unwrap().clone();
    assert_eq!(
        got,
        Value::Str(b"SCENARIO_START".to_vec()),
        "官方 SDK ERIS/global.yst:63 `G_STR[$es.STARTLABEL=\"SCENARIO_START\"]` \
         ⇒ 初值应为 SCENARIO_START(14 字节,引号已剥)"
    );
    // 反例对照:旧行为(裸字节拷贝)会得到 19 字节的窗口字节 —— 正是黑屏根因。
    assert_ne!(
        got,
        Value::Str(raw_field),
        "不得把窗口字节原样当成字符串(旧行为 = 场景标签查表 miss = 画面恒黑)"
    );
}

/// 对照:同一字段的整数初值(裸 i64)不受影响 —— 防止修复波及其他类型。
/// 用 `kind=2`(按脚本初值)走**延迟应用**路径,顺带覆盖该分支。
#[test]
fn ysvr_int_initializer_unchanged_by_the_fix() {
    let table = YsvrTable::from_bytes(&ysvr_bytes(&[(
        2,
        1,
        52,
        0x0afb,
        1,
        10i64.to_le_bytes().to_vec(),
    )]))
    .expect("YSVR 解析");
    let mut vm = make_vm();
    vm.apply_ysvr(&table).expect("应用初值");
    // kind=2 挂起,待该脚本首次加载时应用
    vm.apply_ysvr_for_script(52).expect("按脚本应用");
    assert_eq!(vm.store().get(&var(b'@', 0x0afb)).unwrap(), &Value::Int(10));
}

/// 空串字面量形态:载荷 `''`(`4d 02 00 27 27`)→ 空串。
/// 该形态在本作语料里真实存在(s21 g2 第 3 窗)。
#[test]
fn ysvr_empty_string_literal_decodes_to_empty() {
    let table =
        YsvrTable::from_bytes(&ysvr_bytes(&[(1, 1, 4, 0x068f, 3, vec![0x4d, 0x02, 0x00, 0x27, 0x27])]))
            .expect("YSVR 解析");
    let mut vm = make_vm();
    vm.apply_ysvr(&table).expect("应用初值");
    assert_eq!(
        vm.store().get(&var(b'$', 0x068f)).unwrap(),
        &Value::Str(Vec::new())
    );
}

/// 畸形初值(不构成合法指令流)→ **保留原字节**并留痕(现口径,非静默改语义)。
#[test]
fn ysvr_malformed_string_initializer_keeps_raw_bytes() {
    let junk: Vec<u8> = vec![0x48]; // PushVar 缺操作数 → 解码失败
    let table = YsvrTable::from_bytes(&ysvr_bytes(&[(1, 1, 0, 0x0123, 3, junk.clone())]))
        .expect("YSVR 解析");
    let mut vm = make_vm();
    vm.apply_ysvr(&table).expect("应用初值");
    assert_eq!(
        vm.store().get(&var(b'$', 0x0123)).unwrap(),
        &Value::Str(junk),
        "无法按表达式解码时保留原字节(调用方口径;已 eprintln 留痕)"
    );
}

/// **YSVR 数组初值必须填满整个数组**(勘误,2026-10-17)。
///
/// 引擎 `ysv.ybn` 的数组条目只记**一个**初值(`bounds=[20] init=-1`)。
/// 脚本按任意下标读取时都期望拿到它 —— 本作 s60 `es.SND.PLAY` 的每声道
/// 默认参数表(`@2927..@2936`)按 `(@2971-1)/10` 取下标 0..14,旧实现
/// 只写元素 0,下标 ≥1 读到 0 ⇒ 脚本把「未指定」当显式 0 传给 `SOUND`,
/// `LOOP=0`(引擎语义 = 无限循环)导致语音/按钮 SE 永不停。
#[test]
fn ysvr_array_initializer_fills_every_element() {
    let table = YsvrTable::from_bytes(&ysvr_bytes_with_bounds(&[(
        (1, 1, 0, 0x0b73, 1, (-1i64).to_le_bytes().to_vec()),
        vec![20],
    )]))
    .expect("YSVR 解析");
    let mut vm = make_vm();
    vm.apply_ysvr(&table).expect("应用初值");

    let r = var(b'@', 0x0b73);
    for i in 0..20i64 {
        assert_eq!(
            vm.store().get_elem(&r, &[i]).unwrap(),
            &Value::Int(-1),
            "@0b73[{i}] 应等于数组初值 -1(下标 0..19 全部)"
        );
    }
}
