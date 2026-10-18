//! P5.2 TASKINFO/TASK 测试(成果 61)。
//!
//! 引擎语义(反编译 `p5_sysvar/00451838_CMD_TASKINFO_0x61.c` +
//! `0044fe40_CMD_TASK_0x5f.c`):
//! - TASK:槽 0=ID(名)、槽 34='#'(入口标签)、槽 3-7 参数 → 按名创建/
//!   重配置任务(FUN_0040ca7d 按名查找);
//! - TASKINFO:槽 0=ID + 槽 2=EXIST + 槽 1=LET → 注册表命中写 1/未命中写 0。
//!
//! 真实样本:引擎 400k trace seq 139496/139505 两处 TASKINFO(s190 g472)
//! 均 EXIST=0(任务未创建)→ 随后 TASK(0x5f)注册 "es.IDSubTask";
//! VM 对拍零分歧至 137544 组(全 boot 链 + scenario 入口)。

mod common;

use common::{make_ystb, SAMPLE_KEY};
use yuris_format::ystb::YstbFile;
use yuris_value::{Value, VarRef, VarSpace};
use yuris_vm::{GroupVm, VmEvent, VmSuspend, cmd};

fn var(space: VarSpace, id: u16) -> VarRef {
    VarRef { space, id }
}

/// 窗口三元组:`(tag, 解码后的窗口长度, base_off, 字节)`。
///
/// **必须**用 `w.len()` 而不是手写常量:窗口长度与字节数不一致时,
/// `decode_window` 会在半途截断(旧夹具把 12 字节的 M-串写成 11,
/// 导致 ID 求值失败 → 查询退化成「当前任务」)。
macro_rules! win {
    ($tag:expr, $prog:expr) => {
        ($tag, $prog.len() as u32, 0u32, $prog)
    };
}

/// 合成:TASK(名) → TASKINFO(ID+EXIST+LET) → 断言 1;未注册名 → 0。
#[test]
fn taskinfo_exist_roundtrip() {
    let key = SAMPLE_KEY;
    // w_task_name: 槽0 = "es.Test" ;w_label: 槽34 = "ES.LOOP"
    let w_name: &[u8] = &[
        0x4d, 8, 0, b'"', b'e', b's', b'.', b'T', b'e', b's', b't', b'"',
    ];
    let w_label: &[u8] = &[
        0x4d, 8, 0, b'"', b'E', b'S', b'.', b'L', b'O', b'O', b'P', b'"',
    ];
    let w_exist: &[u8] = &[0x42, 1, 0, 1]; // push 1
    // TASKINFO 的 ID 槽 = 字符串(合成用字面量推入;引擎经 $55[1] 帧局部)
    let w_query: &[u8] = &[
        0x4d, 8, 0, b'"', b'e', b's', b'.', b'T', b'e', b's', b't', b'"',
    ];
    let w_let: &[u8] = &[0x48, 3, 0, 0x40, 0x28, 0x23]; // var @9000

    let groups: &[(u8, u8, u16)] = &[
        (cmd::TASK, 2, 0),       // TASK(名, 标签)
        (cmd::TASKINFO, 3, 0),   // TASKINFO(ID, EXIST, LET)
        (cmd::RETURN, 0, 0),
    ];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], w_name.len() as u32, 0, w_name),  // 槽 0
        ([0x22, 0x00, 0x00, 0x00], w_label.len() as u32, 0, w_label), // 槽 34
        ([0x00, 0x00, 0x00, 0x00], w_query.len() as u32, 0, w_query), // 槽 0
        ([0x02, 0x00, 0x00, 0x00], w_exist.len() as u32, 0, w_exist), // 槽 2
        ([0x01, 0x00, 0x00, 0x00], w_let.len() as u32, 0, w_let),    // 槽 1
    ];
    let bytes = make_ystb(key, groups, windows);
    let script = YstbFile::from_bytes(&bytes, key).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let s = vm.run(100).unwrap();
    // 成果 132:脚本尾 RETURN(基帧) = 重排队让出(Wait),不再 Complete。
    assert!(
        matches!(s, VmSuspend::Wait { counter: None, time_ms: None }),
        "{s:?}"
    );
    let r = var(VarSpace::At, 9000);
    assert_eq!(vm.store().get(&r).unwrap(), &Value::Int(1), "注册后 EXIST=1");
}

/// 未注册任务名 → EXIST=0。
#[test]
fn taskinfo_exist_missing() {
    let key = SAMPLE_KEY;
    let w_query: &[u8] = &[
        0x4d, 8, 0, b'"', b'e', b's', b'N', b'o', b'p', b'e', b'"',
    ];
    let w_exist: &[u8] = &[0x42, 1, 0, 1];
    let w_let: &[u8] = &[0x48, 3, 0, 0x40, 0x29, 0x23]; // var @9001
    let groups: &[(u8, u8, u16)] = &[(cmd::TASKINFO, 3, 0), (cmd::RETURN, 0, 0)];
    // **注意**：`make_ystb` 的窗口顺序决定 content 布局顺序，`base_off`
    // 只作占位（实现按顺序拼接）。三个窗的 len 必须与各自 prog 的字节数
    // 一致 —— 旧夹具把 `w_query`（12B）写成 11，解码时窗口被截断。
    let windows = &[
        win!([0x00, 0x00, 0x00, 0x00], w_query),
        win!([0x02, 0x00, 0x00, 0x00], w_exist),
        win!([0x01, 0x00, 0x00, 0x00], w_let),
    ];
    let bytes = make_ystb(key, groups, windows);
    let script = YstbFile::from_bytes(&bytes, key).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let s = vm.run(100).unwrap();
    assert!(
        vm.executed_groups() > 0,
        "应执行组:suspend={s:?} events={}",
        vm.events().len()
    );
    let r = var(VarSpace::At, 9001);
    assert_eq!(vm.store().get(&r).unwrap(), &Value::Int(0), "未注册 EXIST=0");
}

/// 非 EXIST 查询槽(NEXTVOICE=1 无 ID)→ **记录事件、不写回、不挂起**。
///
/// **政策变更(2026-10-17,用户要求)**:成果 120 的「记录事件、不写回、
/// 不中止」属于「本会中断却被特判续命」——调用方读到的仍是变量原值,
/// 分支结果不可信。现在:
/// - **strict(运行时默认)**:挂起 + 致命错误窗(下面是断言);
/// - **lenient(取证)**:保留旧口径(记录事件、不写回),供批量对拍。
#[test]
fn taskinfo_other_query_halts_in_strict_and_records_in_lenient() {
    let key = SAMPLE_KEY;
    let w_voice: &[u8] = &[0x42, 1, 0, 1]; // 槽 13 NEXTVOICE = 1
    let w_let: &[u8] = &[0x48, 3, 0, 0x40, 0x2a, 0x23]; // var @9002
    let groups: &[(u8, u8, u16)] = &[(cmd::TASKINFO, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        win!([0x0d, 0x00, 0x00, 0x00], w_voice),
        win!([0x01, 0x00, 0x00, 0x00], w_let),
    ];
    let bytes = make_ystb(key, groups, windows);

    // --- strict:未实装查询 = 中断 ---
    let script = YstbFile::from_bytes(&bytes, key).unwrap();
    let mut strict_vm = GroupVm::load(script).unwrap();
    let s = strict_vm.run(100).unwrap();
    match s {
        VmSuspend::Error(reason) => assert!(
            reason.contains("TASKINFO"),
            "根因应点名 TASKINFO: {reason}"
        ),
        other => panic!("strict 下未实装查询必须挂起,得到 {other:?}"),
    }

    // --- lenient:记录事件、不写回 ---
    let script = YstbFile::from_bytes(&bytes, key).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_strict(false);
    let s = vm.run(100).unwrap();
    assert!(
        !matches!(s, VmSuspend::Error(_)),
        "lenient 下查询槽不应中止:{s:?}"
    );
    // **2026-10-18**:TASKINFO 已抽成独立模块，事件变体是 `TaskInfo`
    //（不再是通用 `Subsystem`）；`missing` 列出未实现字段。
    assert!(
        vm.events().iter().any(|e| matches!(
            e,
            VmEvent::TaskInfo { missing, .. } if missing.iter().any(|m| m == "NEXTVOICE")
        )),
        "lenient 下应留下 TaskInfo 事件并点名未实现字段"
    );
    // 不写回:LET 目标保持**未定义**(调用方读到变量原值)
    let r = var(VarSpace::At, 9002);
    assert!(
        vm.store().get(&r).is_err(),
        "未实装查询不得向 LET 目标写入任何值"
    );
}
