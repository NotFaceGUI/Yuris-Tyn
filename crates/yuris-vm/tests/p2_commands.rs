//! P2 命令补全测试:运行期声明族(INT/FLT/STR)+ 后端子系统事件族。
//!
//! 真实样本断言:窗口字节取自引擎 trace 实际执行过的组
//! (engine_trace_boot.jsonl 定位 → probe_group_windows.py dump):
//! - INT:script190 g30(@6293 = 0)、script12 g11(@1749 = 0)
//! - FLT:script301 g0(@7558 = 1080*100.0/1200 + 0.5 = 90.5,分辨率自适应布局)
//! - STR:script12 g10($1748 = $55[1])
//! - FONTINFO:script45 g26(B0=18 LET 目标 + B0=11 参数)
//! - MATH:script190 g342(函数选择 B0=20 + 操作数 @53[1]/@53[2] + 目标 @6352)

use yuris_format::ystb::YstbFile;
use yuris_vm::{GroupVm, VmEvent, cmd};
mod common;
use common::{make_ystb, SAMPLE_KEY};
use yuris_value::{ElemType, VarRef, VarSpace, Value};

fn var(space: u8, id: u16) -> VarRef {
    VarRef { space: VarSpace::from_prefix(space), id }
}

/// script190 g30 真实窗口:`INT @6293 = 0`(w1 = pushint64 0)
#[test]
fn runtime_int_declare_init_real_sample() {
    let w0: &[u8] = &[0x48, 3, 0, 0x40, 0x95, 0x18]; // var @6293
    let w1: &[u8] = &[
        0x4c, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, // pushint64 0
    ];
    let groups: &[(u8, u8, u16)] = &[(0x32, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0),
        ([0x00, 0x01, 0x00, 0x00], w1.len() as u32, 0, w1),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.run(100).unwrap();
    assert_eq!(
        vm.store().get(&var(0x40, 6293)).unwrap(),
        &Value::Int(0),
        "引擎 trace 实证:script190 g30 INT @6293 = 0(desc[6293] type=1 实测)"
    );
    assert!(vm.events().iter().any(
        |e| matches!(e, VmEvent::Declaration { command: 0x32, .. })
    ));
}

/// script12 g11 真实窗口:`INT @1749 = 0`;非零变体验证初值写入
#[test]
fn runtime_int_init_value_nonzero() {
    let w0: &[u8] = &[0x48, 3, 0, 0x40, 0xd5, 0x06]; // var @1749
    let w1: &[u8] = &[0x42, 1, 0, 7]; // pushint8 7
    let groups: &[(u8, u8, u16)] = &[(0x32, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0),
        ([0x00, 0x01, 0x00, 0x00], w1.len() as u32, 0, w1),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.run(100).unwrap();
    assert_eq!(vm.store().get(&var(0x40, 1749)).unwrap(), &Value::Int(7));
}

/// script301 g0 真实窗口:`FLT @7558 = 1080*100.0/1200 + 0.5`(= 90.5)
#[test]
fn runtime_flt_expression_init_real_sample() {
    let w0: &[u8] = &[0x48, 3, 0, 0x40, 0x86, 0x1d]; // var @7558
    let w1: &[u8] = &[
        0x57, 2, 0, 0x38, 0x04, // pushint16 1080
        0x46, 8, 0, 0, 0, 0, 0, 0, 0, 0x59, 0x40, // pushfloat 100.0
        0x2a, 0, 0, // *
        0x57, 2, 0, 0xb0, 0x04, // pushint16 1200
        0x2f, 0, 0, // /
        0x46, 8, 0, 0, 0, 0, 0, 0, 0, 0xe0, 0x3f, // pushfloat 0.5
        0x2b, 0, 0, // +
    ];
    let groups: &[(u8, u8, u16)] = &[(0x19, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0),
        ([0x00, 0x02, 0x00, 0x00], w1.len() as u32, 0, w1),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.run(100).unwrap();
    assert_eq!(
        vm.store().get(&var(0x40, 7558)).unwrap(),
        &Value::Float(90.5),
        "script301 分辨率自适应布局表达式:1080*100.0/1200 + 0.5"
    );
}

/// script12 g10 真实窗口:`STR $1748 = $55[1]`(字符串拷贝)
#[test]
fn runtime_str_copy_real_sample() {
    let w0: &[u8] = &[0x48, 3, 0, 0x24, 0xd4, 0x06]; // var $1748
    let w1: &[u8] = &[
        0x56, 3, 0, 0x24, 0x37, 0x00, // ref $55
        0x42, 1, 0, 1, // push 1
        0x29, 1, 0, 0x00, // aload → $55[1]
    ];
    let groups: &[(u8, u8, u16)] = &[(0x5c, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x03, 0x00, 0x00], w0.len() as u32, 0, w0),
        ([0x00, 0x03, 0x00, 0x00], w1.len() as u32, 0, w1),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    // 引擎 trace 时点:script12 处于某 GOSUB 帧,$55[1] = 该帧实参
    // (成果 56:帧局部族读恒走当前帧,全局 store 无回退)。
    vm.frame_locals_mut().declare_array(
        &var(0x24, 55),
        ElemType::Str,
        &[2],
    );
    vm.frame_locals_mut()
        .set_elem(&var(0x24, 55), &[1], Value::Str(b"abc".to_vec()))
        .unwrap();
    vm.run(100).unwrap();
    assert_eq!(
        vm.store().get(&var(0x24, 1748)).unwrap(),
        &Value::Str(b"abc".to_vec()),
        "引擎 trace 实证:script12 g10 STR $1748 = $55[1]"
    );
}

/// script45 g26/g28 真实窗口:FONTINFO(B0=18 LET 目标 / B0=11、B0=13 参数)。
/// 未声明目标读取 → 引擎容忍(实测),VM 事件记录 err,不挂起。
#[test]
fn subsystem_fontinfo_real_sample() {
    let w0: &[u8] = &[0x48, 3, 0, 0x40, 0x7c, 0x0a]; // var @2684
    let w1: &[u8] = &[0x42, 1, 0, 1]; // push 1
    let groups: &[(u8, u8, u16)] = &[(0x1b, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x12, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0), // B0=18
        ([0x0b, 0x01, 0x00, 0x00], w1.len() as u32, 0, w1), // B0=11
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.run(100).unwrap();
    let ev = vm.events().iter().find_map(|e| match e {
        VmEvent::Subsystem { command, evaluated, .. } if *command == 0x1b => {
            Some(evaluated.clone())
        }
        _ => None,
    });
    let evaluated = ev.expect("FONTINFO 应产生 Subsystem 事件");
    // NUM 查询写回 @2684 = oracle 字体数(环境依赖常量,与 lib.rs 同步;
    // 2026-09-05 采集 = 783)
    const FONTS: i64 = 783;
    assert_eq!(evaluated.len(), 2, "两个窗口都应记录");
    assert_eq!(evaluated[0], (18, format!("int:{FONTS}")));
    assert_eq!(evaluated[1], (11, "int:1".to_string()));
    let r = VarRef { space: VarSpace::At, id: 2684 };
    assert_eq!(vm.store().get(&r).unwrap(), &Value::Int(FONTS));
}

/// script190 g342 真实窗口:MATH(函数选择 B0=20=RND + 操作数 @53[1]/@53[2]
/// + 目标 @6352)。
///
/// **T22 更新(2026-10-16)**:此前 MATH 只记录事件、**LET 写回不发生**
/// (旧断言即"目标未声明 → err 记录")。现按引擎语义实装写回:目标声明后
/// 结果落在 `@6352`,RND 取值在 `[11,22]`(引擎:lo + draw % (hi−lo+1))。
#[test]
fn subsystem_math_real_sample() {
    let w0: &[u8] = &[0x42, 1, 0, 1]; // push 1(函数选择)
    let w1: &[u8] = &[
        0x56, 3, 0, 0x40, 0x35, 0x00, 0x42, 1, 0, 1, 0x29, 1, 0, 0x00, // @53[1]
    ];
    let w2: &[u8] = &[
        0x56, 3, 0, 0x40, 0x35, 0x00, 0x42, 1, 0, 2, 0x29, 1, 0, 0x00, // @53[2]
    ];
    let w3: &[u8] = &[0x48, 3, 0, 0x40, 0xd0, 0x18]; // var @6352
    let groups: &[(u8, u8, u16)] = &[(0x3c, 4, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x14, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0), // B0=20
        ([0x01, 0x01, 0x00, 0x00], w1.len() as u32, 0, w1), // B0=1
        ([0x02, 0x01, 0x00, 0x00], w2.len() as u32, 0, w2), // B0=2
        ([0x00, 0x01, 0x00, 0x00], w3.len() as u32, 0, w3), // B0=0
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_math_seed(20261016); // 确定性:逐位序列不依赖时间种子
    // 引擎 trace 时点:s190 g342 处于某子程序帧,@53[1]/@53[2] = 帧实参
    // (成果 56:帧局部族读恒走当前帧,全局 store 无回退)。
    vm.frame_locals_mut().declare_array(&var(0x40, 53), ElemType::Int, &[4]);
    vm.frame_locals_mut().set_elem(&var(0x40, 53), &[1], Value::Int(11)).unwrap();
    vm.frame_locals_mut().set_elem(&var(0x40, 53), &[2], Value::Int(22)).unwrap();
    // 目标 @6352 由写入建立(引擎:描述符恒存在,MATH 直接写)
    vm.store_mut().set(&var(0x40, 6352), Value::Int(0));
    vm.run(100).unwrap();
    // RND 写回:取值在闭区间 [11,22](引擎 lo + draw % (hi-lo+1))
    let got = vm.store().get(&var(0x40, 6352)).unwrap().clone();
    match got {
        Value::Int(v) => assert!((11..=22).contains(&v), "RND 越界:{v}"),
        other => panic!("RND 应写整数,得到 {other:?}"),
    }
    // 事件仍在(求值记录口径不变:四个窗口各一条)
    let ev = vm.events().iter().find_map(|e| match e {
        VmEvent::Subsystem { command, evaluated, .. } if *command == 0x3c => {
            Some(evaluated.clone())
        }
        _ => None,
    });
    let evaluated = ev.expect("MATH 应产生 Subsystem 事件");
    assert_eq!(evaluated.len(), 4);
    assert_eq!(evaluated[0], (20, "int:1".to_string()));
    assert_eq!(evaluated[1], (1, "int:11".to_string()));
    assert_eq!(evaluated[2], (2, "int:22".to_string()));
    // 槽 0 = LET 引用窗(引擎:引用窗不装载 → 记录为 err/引用形态)
    assert_eq!(evaluated[3].0, 0);
}

/// **T22 HASH**:`MATH[LET @x SETSTR="123456789" HASH=1]` → 写回**标准 CRC-32**
/// (`0xCBF43926`;证据 = 另一条线对引擎 `0x43fad8` 的逐字节比对 + 检查值)。
#[test]
fn math_hash_writes_standard_crc32() {
    // SETSTR(槽 3):M-串 `"123456789"`
    let setstr: &[u8] = &[
        0x4d, 11, 0, b'"', b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'"',
    ];
    let hash_on: &[u8] = &[0x42, 1, 0, 1]; // HASH(槽 21)= 1
    let let_w: &[u8] = &[0x48, 3, 0, 0x40, 0x77, 0x10]; // var @4215
    let groups: &[(u8, u8, u16)] = &[(0x3c, 3, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], let_w.len() as u32, 0, let_w),     // LET(槽 0)
        ([0x03, 0x01, 0x00, 0x00], setstr.len() as u32, 0, setstr),   // SETSTR(槽 3)
        ([0x15, 0x01, 0x00, 0x00], hash_on.len() as u32, 0, hash_on), // HASH(槽 21)
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.store_mut().set(&var(0x40, 4215), Value::Int(0));
    vm.run(100).unwrap();
    assert_eq!(
        vm.store().get(&var(0x40, 4215)).unwrap(),
        &Value::Int(0xCBF4_3926),
        "HASH 应写标准 CRC-32 检查值"
    );
}

/// **T22 一元族**:`MATH[LET @x SET=100.0 LOG10=1]` → 写回 2.0。
#[test]
fn math_unary_log10_writes_back() {
    let set_w: &[u8] = &{
        let mut v = vec![0x46, 8, 0];
        v.extend_from_slice(&100.0f64.to_le_bytes());
        v
    };
    let log10_on: &[u8] = &[0x42, 1, 0, 1]; // LOG10(槽 18)= 1
    let let_w: &[u8] = &[0x48, 3, 0, 0x40, 0x78, 0x10]; // var @4216
    let groups: &[(u8, u8, u16)] = &[(0x3c, 3, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], let_w.len() as u32, 0, let_w),
        ([0x01, 0x02, 0x00, 0x00], set_w.len() as u32, 0, &set_w), // SET(槽 1),B2=2(FLT)
        ([0x12, 0x01, 0x00, 0x00], log10_on.len() as u32, 0, log10_on), // LOG10(槽 18)
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.store_mut().set(&var(0x40, 4216), Value::Float(0.0));
    vm.run(100).unwrap();
    assert_eq!(
        vm.store().get(&var(0x40, 4216)).unwrap(),
        &Value::Float(2.0),
        "LOG10(100) 应写回 2.0"
    );
}

/// 「求值参数 → 事件化」族:各命令至少产生一个带参数的事件。
///
/// 2026-10-18 变更：此前用 v555 的**裸序号**（0x0e/0x14/…）当样本，它们在
/// v494 下指向别的命令。现在按**命令名**取号（跨版本稳定），并把已经
/// 独立建模的命令（ERROR→`ErrorConfig`、FILEACT→`FileAct`、SYSTEM→
/// `SystemConfig`、EMOTE→`Emote`）也纳入断言 —— 事件变体不同，但
/// 「参数被求值并进事件」这一契约不变。
#[test]
fn subsystem_family_events() {
    // 按**命令名**取号（跨版本稳定）。这些命令分布在四条实现路径上：
    // - `Subsystem` 兜底族（FILEINFO/FONT/FPS/INPUT/MOUSE/WINDOWINFO）→ `Subsystem`
    // - `ERROR` → `ErrorConfig`
    // - `FILEACT` → `FileAct`
    // - `SYSTEM` → `SystemConfig`
    // 契约相同：参数被求值并进事件。
    let w: &[u8] = &[0x42, 1, 0, 3]; // push 3
    let p = yuris_core::CommandProfile::v555();
    for name in [
        "ERROR", "FILEACT", "FILEINFO", "FONT", "FPS", "INPUT", "MOUSE", "SYSTEM",
        "WINDOWINFO",
    ] {
        let c = u8::try_from(p.index_of(name).expect(name)).expect("u8");
        let groups: &[(u8, u8, u16)] = &[(c, 1, 0), (cmd::RETURN, 0, 0)];
        // 各命令的参数槽号不同；用 YSCM 参数表的**首槽**（=该命令的第一个参数）。
        let slot = 0u8;
        let tag = [slot, 0, 0, 0];
        let windows = &[(tag, w.len() as u32, 0, w)];
        let bytes = make_ystb(SAMPLE_KEY, groups, windows);
        let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
        let mut vm = GroupVm::load(script).unwrap();
        vm.set_strict(false); // 参数形态与真实语料不同 → 允许记录后继续
        vm.run(100).unwrap();
        let has_param_event = vm.events().iter().any(|e| match e {
            VmEvent::Subsystem { evaluated, .. } => !evaluated.is_empty(),
            VmEvent::ErrorConfig { evaluated, .. } => !evaluated.is_empty(),
            VmEvent::FileAct { .. } => true,
            VmEvent::SystemConfig { evaluated, .. } => !evaluated.is_empty(),
            _ => false,
        });
        assert!(has_param_event, "{name} 应产生带参数的事件");
    }
}

/// 事件 JSON schema:sub 事件与 diff 工具域一致。
#[test]
fn subsystem_event_json_shape() {
    use yuris_vm::event_json;
    let e = VmEvent::Subsystem {
        pc: 5,
        command: 0x1b,
        evaluated: vec![(18, "int:1".into())],
    };
    assert_eq!(
        event_json(&e),
        r#"{"ev":"sub","pc":5,"cmd":27,"ev_":["18:int:1"]}"#
    );
}

/// 声明组消费不受运行期声明族影响(boot 消费路径回归)。
#[test]
fn declarations_still_consumed_at_load() {
    use yuris_vm::consume_declarations_into;
    use yuris_vm::host::ScriptCtx;
    use yuris_vm::CommandDispatch;
    let w0: &[u8] = &[0x48, 3, 0, 0x40, 0x11, 0x04]; // var @1041
    let groups: &[(u8, u8, u16)] = &[(0x32, 1, 0)];
    let windows = &[([0x00, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0)];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let ctx = ScriptCtx::parse(0, script).unwrap();
    let mut store = yuris_value::VariableStore::new();
    let d = CommandDispatch::for_engine_version(555).expect("v555 内置表");
    let n = consume_declarations_into(&mut store, &ctx, &d).unwrap();
    assert_eq!(n, 1);
    assert_eq!(store.get(&var(0x40, 1041)).unwrap(), &Value::Int(0));
}

/// `RETURNCODE`(v494/495 索引 79)= **调用点返回标记,不弹帧**(成果 117)。
///
/// 语料形态(PROGRESS 成果 117 取证):全语料 28299 组,**恒为 1 个空窗口**,
/// `tag` 为逐脚本递增序号;YSCM 参数数 0。
///
/// 本测试用 `GOSUB → RETURN → RETURNCODE → WAIT` 的四组脚本把语义钉死:
/// - 若 `RETURNCODE` 与 `RETURN` 同义(弹帧),子程序返回后帧已空,
///   再弹一次会直接结束脚本 → **到不了 pc2 的 WAIT**;
/// - 正确实现(不弹帧、推进 pc)则必须停在 pc2 的 WAIT 上。
///
/// 反例对照:同一脚本把 `RETURNCODE` 换成 `RETURN`(索引 77)时,
/// **必须**在 pc1 处结束(帧空返回 = 脚本结束),两者行为必须不同。
#[test]
fn returncode_marks_call_site_without_popping_frame() {
    use yuris_core::commands::SDK_495_COMMAND_NAMES;
    use yuris_core::CommandProfile;
    use yuris_vm::VmState;
    use std::collections::HashMap;

    fn idx(name: &str) -> u8 {
        SDK_495_COMMAND_NAMES
            .iter()
            .position(|n| *n == name)
            .expect("命令名") as u8
    }

    let cond_true: &[u8] = &[0x42, 1, 0, 1];
    let sub_label: &[u8] = &[0x4d, 5, 0, b'"', b'S', b'U', b'B', b'"'];
    let wait_w: &[u8] = &[0x42, 1, 0, 1];
    let empty: &[u8] = &[];

    let run = |marker: u8| -> (VmState, (bool, bool), u64) {
        let groups: &[(u8, u8, u16)] = &[
            (idx("GOSUB"), 2, 0),  // g0
            (marker, 1, 0),        // g1 ← 返回点标记(单空窗,与语料形态一致)
            (idx("WAIT"), 1, 0),   // g2 ← 只有「标记不弹帧」才会执行到这里
            (idx("RETURN"), 0, 0), // g3 = SUB 体
        ];
        let windows = &[
            ([0, 0, 0, 0], cond_true.len() as u32, 0, cond_true),
            ([0, 0, 0, 0], sub_label.len() as u32, 0, sub_label),
            ([1, 0, 0, 0], empty.len() as u32, 0, empty), // tag=0x1:空窗口
            ([0, 0, 0, 0], wait_w.len() as u32, 0, wait_w),
        ];
        let bytes = common::make_ystb_version(SAMPLE_KEY, 494, groups, windows);
        let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
        let mut vm = GroupVm::load(script).unwrap();
        vm.set_command_profile(Some(CommandProfile::from_names(
            SDK_495_COMMAND_NAMES,
            494,
            "test profile (RETURNCODE=79)",
        )));
        let mut labels = HashMap::new();
        labels.insert(b"SUB".to_vec(), (3u32, 0u16));
        vm.set_labels(labels);
        vm.set_script_id(0);
        let s = vm.run(100);
        let _ = &s;
        // 记下「pc1 那组」(返回点标记)与「pc2 那组」(WAIT)是否各执行过一次。
        let ran_pc = |p: usize| {
            vm.events().iter().any(|e| match e {
                VmEvent::GroupExecuted { pc, .. } | VmEvent::Subsystem { pc, .. } => *pc == p,
                _ => false,
            })
        };
        (vm.state(), (ran_pc(1), ran_pc(2)), vm.returncode_hits())
    };

    let (st_rc, (ran1, ran2), hits) = run(idx("RETURNCODE"));
    assert!(ran1, "RETURNCODE(pc1)必须被执行到");
    assert!(
        ran2,
        "RETURNCODE 之后必须继续执行 pc2 的 WAIT —— 若在此为假,说明它被误当成 RETURN 弹了帧"
    );
    assert_eq!(hits, 1, "RETURNCODE 应计数 1 次");
    assert_eq!(
        st_rc,
        VmState::Running,
        "RETURNCODE 后应停在 WAIT(状态 Running,由 VmSuspend::Wait 表达)"
    );

    // 对照:同位置放 `RETURN` 时必须**到不了** pc2(帧空返回 = 脚本结束)。
    // (成果 132 勘误:引擎基帧 RETURN = FUN_00451724 重排队,VM 表达为
    //  VmSuspend::Wait{None,None};状态保持 Running = 基帧可重入执行位。)
    let (st_ret, (c1, c2), _) = run(idx("RETURN"));
    assert!(c1, "对照:RETURN(pc1)被执行到");
    assert!(
        !c2,
        "对照:RETURN 在帧空时结束了脚本,不得继续执行 pc2 —— 这正是它与 RETURNCODE 的分界"
    );
    assert_eq!(st_ret, VmState::Running, "对照:RETURN 不再是终结态;状态保持 Running(重排队语义)");
}

/// `TASK` **建立并调度任务体**(成果 118)：任务体是独立执行位，与主线并行推进。
///
/// 语料背景：本作 `s20 g26/g27` 经 `es._task(...)` 建立
/// `ES.SCENARIO.BtnLoop`(s20 pc184) 与 `es.SCENARIO.FLOW`(s21 pc0)；
/// 旧实现只注册名字不调度 ⇒ 两者 4000 tick 执行 0 次 ⇒ 场景推进链整体缺席。
///
/// 夹具：主线 `TASK` → `WAIT`（= 帧边界，任务在此被推）→ `END`；
/// 任务体 `LET @4242 = 7` → `WAIT(1)` → `RETURN`。
#[test]
fn task_body_is_scheduled_alongside_the_main_line() {
    use yuris_core::commands::SDK_495_COMMAND_NAMES;
    use yuris_core::CommandProfile;
    use yuris_vm::VmSuspend;
    use std::collections::HashMap;

    fn idx(name: &str) -> u8 {
        SDK_495_COMMAND_NAMES.iter().position(|n| *n == name).expect("命令名") as u8
    }

    let w_id: &[u8] = &[0x4d, 3, 0, b'"', b't', b'"']; // "t"
    let w_entry: &[u8] = &[0x4d, 5, 0, b'"', b'S', b'U', b'B', b'"']; // "SUB"
    let w_one: &[u8] = &[0x42, 1, 0, 1]; // pushint8 1
    let w_lhs: &[u8] = &[0x48, 3, 0, 0x40, 0x92, 0x10]; // var @4242
    let w_rhs: &[u8] = &[0x42, 1, 0, 7]; // pushint8 7

    let groups: &[(u8, u8, u16)] = &[
        (idx("TASK"), 2, 0),   // g0 主:建任务
        (idx("WAIT"), 1, 0),   // g1 主:让出 → 触发任务调度
        (idx("END"), 0, 0),    // g2 主:结束
        (idx("LET"), 2, 0),    // g3 任务体:写 @4242 = 7
        (idx("WAIT"), 1, 0),   // g4 任务体:让出
        (idx("RETURN"), 0, 0), // g5 任务体:结束
    ];
    let windows = &[
        ([0, 0, 0, 0], w_id.len() as u32, 0, w_id),       // TASK.ID(槽0)
        ([34, 0, 0, 0], w_entry.len() as u32, 0, w_entry), // TASK.#(槽34)
        ([0, 0, 0, 0], w_one.len() as u32, 0, w_one),     // WAIT.FRAME(槽0)
        ([0, 0, 0, 0], w_lhs.len() as u32, 0, w_lhs),     // LET 左值
        ([0, 0, 0, 0], w_rhs.len() as u32, 0, w_rhs),     // LET 右值
        ([0, 0, 0, 0], w_one.len() as u32, 0, w_one),     // 任务体 WAIT.FRAME(槽0)
    ];
    let bytes = common::make_ystb_version(SAMPLE_KEY, 494, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_command_profile(Some(CommandProfile::from_names(
        SDK_495_COMMAND_NAMES,
        494,
        "test profile (TASK/#=34)",
    )));
    let mut labels = HashMap::new();
    labels.insert(b"SUB".to_vec(), (3u32, 0u16)); // 入口落在同脚本 g3
    vm.set_labels(labels);
    vm.set_script_id(0);

    // 第一次 run:主线 g0(TASK) → g1(WAIT 让出) → 任务被推一次。
    let s = vm.run(64).unwrap();
    assert!(
        matches!(s, VmSuspend::Wait { .. }),
        "主线应在 WAIT 让出,实测 {s:?}"
    );
    assert_eq!(vm.task_count(), 1, "TASK 应建立一个任务");
    assert_eq!(
        vm.store().get(&var(0x40, 4242)).ok(),
        Some(&Value::Int(7)),
        "任务体必须真的执行(旧实现只注册名字 → 这里取不到)"
    );
    let summary = vm.task_summary();
    // 任务的 WAIT 在 pc4:VM 推进 pc 到 5 后返回挂起,并把它记成"待命 1 帧"。
    assert!(
        summary[0].starts_with("t s0 pc5 ")
            && summary[0].contains("steps=1")
            && summary[0].contains("wait=1")
            && summary[0].contains("fin=false"),
        "任务应被推一次、停在自身 WAIT(待命 1 帧、未结束),实测 {:?}",
        summary[0]
    );
}

/// 事件缓冲必须能被消费方**封顶**(内存泄漏修复,成果 118)。
///
/// 修复前:`events()` 是只增不减的 `Vec`,而所有消费方都只持游标 ⇒ 长跑必爆
/// (实测数十帧即 1.16e6 条;开启任务调度后 60 tick 就累积 4.06e6 条)。
#[test]
fn event_buffer_can_be_trimmed_by_the_consumer() {
    let w0: &[u8] = &[0x42, 1, 0, 0];
    let groups: &[(u8, u8, u16)] = &[(
        // 用一个必然产生事件的命令(INT 声明),保证缓冲非空。
        0x32, 1, 0,
    )];
    let windows = &[([0x00, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0)];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let _ = vm.run(64);
    let before = vm.events().len();
    assert!(before > 0, "夹具应产生事件");
    let cursor = vm.trim_consumed_events(before);
    assert_eq!(cursor, 0, "消费方游标应归零");
    assert_eq!(vm.events().len(), 0, "已消费事件必须被丢弃");
}
