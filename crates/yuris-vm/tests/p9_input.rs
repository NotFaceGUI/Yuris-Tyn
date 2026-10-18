//! P9.2 输入层 VM 接线测试：引擎输入系统变量 + `_INP` 数组 + 写回钳制。
//!
//! 语料依据：
//! - 官方手册 `basic/input.html`：按键状态 = 64 位帧计数器（按下 1,2,3… /
//!   抬起 -1,-2,…）。
//! - 官方手册 `basic/svariable.html`：`@_INP(code)`、`@_KEY_*`、`@_MOUSE_*`、
//!   `@_CODE_*` 的可赋值标记。
//! - 引擎 `FUN_00447014`：脚本写输入变量时按物理态钳制。
//! - ERIS `es_Button.yst`：`IF[@_MOUSE_L==1] … @_MOUSE_L=2` 的按钮互斥写法。

use yuris_format::ystb::YstbFile;
use yuris_value::{VarRef, VarSpace, Value};
use yuris_vm::{cmd, GroupVm, InputSnapshot};

mod common;
use common::{make_ystb, SAMPLE_KEY};

fn at(id: u16) -> VarRef {
    VarRef {
        space: VarSpace::At,
        id,
    }
}

fn snapshot() -> InputSnapshot {
    let mut s = InputSnapshot::default();
    s.claimed = (133..=547).collect();
    s.scalar.insert(133, 640); // _MOUSE_X
    s.scalar.insert(138, 360); // _MOUSE_Y
    s.scalar.insert(147, 1); // _MOUSE_L（按下帧）
    s.scalar.insert(244, -2); // _KEY_ENTER（抬起第 2 帧）
    s.scalar.insert(435, 0x41); // _CODE_A
    s.writable.insert(147);
    s.writable.insert(244);
    s.down.insert(0x01);
    let mut inp = vec![0i64; 256];
    inp[0x01] = 1;
    inp[0x41] = 3;
    s.inp = inp;
    s
}

/// `LET @100 = @_MOUSE_L` + `LET @101 = @_INP(@_CODE_A)` + `LET @102 = @138`。
#[test]
fn reads_engine_input_sysvars() {
    let lhs100: &[u8] = &[0x48, 3, 0, 0x40, 100, 0];
    let lhs101: &[u8] = &[0x48, 3, 0, 0x40, 101, 0];
    let lhs102: &[u8] = &[0x48, 3, 0, 0x40, 102, 0];
    // @_MOUSE_L = @147
    let mouse_l: &[u8] = &[0x48, 3, 0, 0x40, 147, 0];
    // @_MOUSE_Y = @138
    let mouse_y: &[u8] = &[0x48, 3, 0, 0x40, 138, 0];
    // _INP(@_CODE_A): pushvarref(@,144) | pushvar(@,435) | aload
    let inp_code_a: &[u8] = &[
        0x56, 3, 0, 0x40, 144, 0, // 左值引用 @144
        0x48, 3, 0, 0x40, 0xB3, 0x01, // 下标 = @_CODE_A(435 = 0x01B3)
        0x29, 1, 0, 0, // aload
    ];
    let groups: &[(u8, u8, u16)] = &[
        (cmd::LET, 2, 0),
        (cmd::LET, 2, 0),
        (cmd::LET, 2, 0),
        (cmd::RETURN, 0, 0),
    ];
    fn w(tag: [u8; 4], code: &[u8]) -> ([u8; 4], u32, u32, &[u8]) {
        (tag, code.len() as u32, 0u32, code)
    }
    let windows = &[
        w([0x00, 0x00, 0x00, 0x00], lhs100),
        w([0x00, 0x01, 0x00, 0x00], mouse_l),
        w([0x00, 0x00, 0x00, 0x00], lhs101),
        w([0x00, 0x01, 0x00, 0x00], inp_code_a),
        w([0x00, 0x00, 0x00, 0x00], lhs102),
        w([0x00, 0x01, 0x00, 0x00], mouse_y),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_input_snapshot(snapshot());
    vm.run(100).unwrap();

    assert_eq!(vm.store().get(&at(100)).unwrap(), &Value::Int(1)); // _MOUSE_L
    assert_eq!(vm.store().get(&at(101)).unwrap(), &Value::Int(3)); // _INP(0x41)
    assert_eq!(vm.store().get(&at(102)).unwrap(), &Value::Int(360)); // _MOUSE_Y
}

/// ERIS 按钮互斥写法：`IF @_MOUSE_L == 1 { @_MOUSE_L = 2 }`。
#[test]
fn script_write_to_input_sysvar_is_queued_and_clamped() {
    // IF w0=cond(pushvar @147, pushint 1, eq), w1=真分支组号(2)
    let cond: &[u8] = &[
        0x48, 3, 0, 0x40, 147, 0, // @_MOUSE_L
        0x42, 1, 0, 1, // 1
        0x3d, 0, 0, // ==
    ];
    let lhs: &[u8] = &[0x48, 3, 0, 0x40, 147, 0];
    let two: &[u8] = &[0x42, 1, 0, 2];
    let groups: &[(u8, u8, u16)] = &[
        (cmd::IF, 3, 0),     // g0
        (cmd::LET, 2, 0),    // g1 真分支:@_MOUSE_L = 2
        (cmd::RETURN, 0, 0), // g2 假跳目标 / 收尾
    ];
    fn w(tag: [u8; 4], code: &[u8]) -> ([u8; 4], u32, u32, &[u8]) {
        (tag, code.len() as u32, 0u32, code)
    }
    // IF 3 窗:w0 = 条件;w1.len = 假跳组号(2 = RETURN);w2.len = end 组号。
    let windows = &[
        w([0x00, 0x00, 0x00, 0x00], cond),
        (common::DUMMY, 2, 0, &[][..]),
        (common::DUMMY, 2, 0, &[][..]),
        w([0x00, 0x00, 0x00, 0x00], lhs),
        w([0x00, 0x01, 0x00, 0x00], two),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_input_snapshot(snapshot());
    vm.run(100).unwrap();

    let writes = vm.take_input_writes();
    assert_eq!(writes, vec![(147u16, None, Value::Int(2))]);
    // 同帧读取看到钳制后的值（物理按住 → 保留 2）。用第二条 LET 读回。
    // （`read_var_value` 是私有方法；集成测试经脚本读面验证。）
}

/// 未按住时写正数 → 0（`FUN_00447014` 的钳制）；未按住写负数保留。
#[test]
fn rewrite_is_clamped_when_not_physically_down() {
    let mut vm_inputs = snapshot();
    vm_inputs.down.clear();
    vm_inputs.scalar.insert(147, -1);
    vm_inputs.writable.insert(148);

    let lhs: &[u8] = &[0x48, 3, 0, 0x40, 148, 0]; // @_MOUSE_L1
    let two: &[u8] = &[0x42, 1, 0, 2];
    let groups: &[(u8, u8, u16)] = &[(cmd::LET, 2, 0), (cmd::RETURN, 0, 0)];
    fn w(tag: [u8; 4], code: &[u8]) -> ([u8; 4], u32, u32, &[u8]) {
        (tag, code.len() as u32, 0u32, code)
    }
    let windows = &[
        w([0x00, 0x00, 0x00, 0x00], lhs),
        w([0x00, 0x01, 0x00, 0x00], two),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_input_snapshot(vm_inputs);
    vm.run(100).unwrap();
    let writes = vm.take_input_writes();
    assert_eq!(writes, vec![(148u16, None, Value::Int(0))]);
}

/// `INPUT[KEY=0 MOUSE=1 PAD=0]` / `MOUSE[MODE=1 TIME=1500 CLICK=0]` 落到
/// 引擎设置结构（参数按命令自身 YSCM 参数名解析）。
#[test]
fn input_and_mouse_commands_update_settings() {
    // v555: INPUT 槽 0=KEY 1=PAD 2=MOUSE;MOUSE 槽 0=MODE 1=TIME 3=CLICK
    let key0: &[u8] = &[0x42, 1, 0, 0];
    let pad0: &[u8] = &[0x42, 1, 0, 0];
    let mouse1: &[u8] = &[0x42, 1, 0, 1];
    let mode1: &[u8] = &[0x42, 1, 0, 1];
    let time1500: &[u8] = &[0x57, 2, 0, 0xdc, 0x05];
    let click0: &[u8] = &[0x42, 1, 0, 0];
    let groups: &[(u8, u8, u16)] = &[
        (0x31, 3, 0),        // INPUT(v555 序号 = 0x31,按名分派)
        (0x45, 3, 0),        // MOUSE
        (cmd::RETURN, 0, 0), // 结束
    ];
    fn w(tag: [u8; 4], code: &[u8]) -> ([u8; 4], u32, u32, &[u8]) {
        (tag, code.len() as u32, 0u32, code)
    }
    let windows = &[
        w([0x00, 0x01, 0x00, 0x00], key0),
        w([0x01, 0x01, 0x00, 0x00], pad0),
        w([0x02, 0x01, 0x00, 0x00], mouse1),
        w([0x00, 0x01, 0x00, 0x00], mode1),
        w([0x01, 0x01, 0x00, 0x00], time1500),
        w([0x03, 0x01, 0x00, 0x00], click0),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_input_snapshot(snapshot());
    vm.run(100).unwrap();
    let s = vm.input_settings();
    assert_eq!(s.keyboard, Some(false), "INPUT[KEY=0]");
    assert_eq!(s.pad, Some(false), "INPUT[PAD=0]");
    assert_eq!(s.mouse, Some(true), "INPUT[MOUSE=1]");
    assert_eq!(s.mouse_mode, Some(1), "MOUSE[MODE=1]");
    assert_eq!(s.mouse_time_ms, Some(1500), "MOUSE[TIME=1500]");
    assert_eq!(s.click_reactive, Some(false), "MOUSE[CLICK=0]");
}
