//! P2 回归:参数槽位通用化 + 帧实参播种 + 三处语义勘误。
//!
//! 每项对应一个**实测证伪**的旧行为(证据见各测试注释与 PROGRESS.md
//! 成果 96-100)。语料为合成脚本,与具体游戏无关。

use yuris_core::command_params::SDK_495_COMMAND_PARAMS;
use yuris_core::commands::SDK_495_COMMAND_NAMES;
use yuris_core::CommandProfile;
use yuris_format::ystb::YstbFile;
use yuris_vm::{cmd, CommandDispatch, GroupVm, VmEvent};
use yuris_value::{ElemType, VarRef, VarSpace, Value};
mod common;
use common::{make_ystb, make_ystb_version, SAMPLE_KEY};

fn var(space: VarSpace, id: u16) -> VarRef {
    VarRef { space, id }
}

/// v555 表里的命令序号(合成脚本的 command_type 用)。
fn idx(name: &str) -> u8 {
    CommandProfile::v555()
        .index_of(name)
        .unwrap_or_else(|| panic!("{name} 不在 v555 表内")) as u8
}

/// SDK 495 表里的命令序号(CG 槽位测试注入的是 SDK profile)。
fn idx_sdk(name: &str) -> u8 {
    CommandProfile::sdk_495()
        .index_of(name)
        .unwrap_or_else(|| panic!("{name} 不在 SDK 表内")) as u8
}

/// **成果 96**:CG 的 FILE 槽按**参数名**解析,不写死数字。
///
/// 旧行为:处理器写死 `(46, Value::Str) => file`。而 `FILE` 槽逐版本漂移
/// (实测 v555=46 / v494·495=33 / v479=29)→ 在槽 33 的版本上永远读不到文件,
/// 纹理不装载 → 画面全黑(实测 `Cg` 事件 `file: None` + 场景 0 图层)。
#[test]
fn cg_file_slot_resolved_by_param_name_not_constant() {
    let w_id: &[u8] = &[0x4d, 6, 0, b'"', b'C', b'G', b'S', b'1', b'"'];
    let w_file: &[u8] = &[
        0x4d, 13, 0, b'"', b'c', b'g', b'/', b't', b'e', b's', b't', b'.', b'p', b'n', b'g',
        b'"',
    ];
    let w_x: &[u8] = &[0x42, 1, 0, 10];
    let w_y: &[u8] = &[0x42, 1, 0, 20];
    let w_wait: &[u8] = &[0x42, 1, 0, 1];

    let groups: &[(u8, u8, u16)] = &[(idx_sdk("CG"), 4, 0), (idx_sdk("WAIT"), 1, 0)];
    let windows = &[
        ([0x00, 0x03, 0x00, 0x00], w_id.len() as u32, 0, w_id),
        ([0x04, 0x01, 0x00, 0x00], w_x.len() as u32, 0, w_x),
        ([0x05, 0x01, 0x00, 0x00], w_y.len() as u32, 0, w_y),
        // FILE @ 槽 33(SDK 495/494 布局,非 v555 的 46)
        ([33, 0x01, 0x00, 0x00], w_file.len() as u32, 0, w_file),
        ([0x00, 0x00, 0x01, 0x00], w_wait.len() as u32, 0, w_wait),
    ];
    let bytes = make_ystb_version(SAMPLE_KEY, 494, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    // 注入「FILE 在槽 33」的 profile(等价于启动链从游戏 ysc.ybn 读到的那张表)
    vm.set_command_profile(Some(CommandProfile::from_names_and_params(
        SDK_495_COMMAND_NAMES,
        SDK_495_COMMAND_PARAMS,
        494,
        "test profile (FILE at slot 33)",
    )));
    let _ = vm.run(64);

    let (id, file, pos) = vm
        .events()
        .iter()
        .find_map(|e| match e {
            VmEvent::Cg { id, file, position, .. } => Some((id.clone(), file.clone(), *position)),
            _ => None,
        })
        .expect("应有 CG 事件");
    assert_eq!(id.as_deref(), Some("CGS1"));
    assert_eq!(
        file.as_deref(),
        Some(&b"cg/test.png"[..]),
        "FILE 必须从**参数名 FILE** 解析出的槽(此处 33)读到"
    );
    assert_eq!(pos.map(|(x, y, _)| (x, y)), Some((10, 20)));
}

/// 内置表都带参数名表,且槽位随版本漂移(v555=46 / SDK495=33)。
#[test]
fn builtin_profiles_expose_param_slots() {
    let v555 = CommandProfile::v555();
    let sdk495 = CommandProfile::sdk_495();
    assert!(v555.has_params() && sdk495.has_params());
    assert_eq!(v555.slot_of_named("CG", "FILE"), Some(46));
    assert_eq!(sdk495.slot_of_named("CG", "FILE"), Some(33));
    assert_eq!(v555.slot_of_named("CG", "MODE"), Some(54));
    assert_eq!(sdk495.slot_of_named("CG", "MODE"), Some(41));
}

/// **成果 97**:GOSUB 帧实参播种按**实参窗槽号**决定(不走 gparam 位图公式)。
///
/// 实测 43001 个 GOSUB/GO 组:gparam 恒等于实参窗个数(= 最大槽号);旧位图
/// 公式在 40778 组上解出的槽数不足以容纳实参 → 帧局部**根本没写入**。
/// 端到端后果:`es._label.info.exist` 收到空标签名 → 场景存在性判定恒假。
#[test]
fn gosub_str_arg_reaches_callee_frame_local() {
    let arg: &[u8] = &[
        0x4d, 10, 0, b'"', b'E', b'S', b'.', b'F', b'I', b'R', b'S', b'T', b'"',
    ];
    let name: &[u8] = &[0x4d, 8, 0, b'"', b'C', b'A', b'L', b'L', b'E', b'E', b'"'];
    let w_wait: &[u8] = &[0x42, 1, 0, 1];
    // 被调方(pc=2):LET $0x0900 = $55[1]  —— 读帧局部 STR 槽 1
    let lhs: &[u8] = &[0x48, 3, 0, 0x24, 0x00, 0x09]; // 左值 $0x0900
    let rhs: &[u8] = &[
        0x56, 3, 0, 0x24, 0x37, 0x00, // push 帧局部 STR 数组 $55
        0x42, 1, 0, 1, // push 1
        0x29, 1, 0, 0x00, // ArrayLoad
    ];
    let groups: &[(u8, u8, u16)] = &[
        (idx("GOSUB"), 2, 1), // gparam=1(= 实参窗个数,实测语义)
        (idx("WAIT"), 1, 0),
        (idx("LET"), 2, 0),
        (idx("RETURN"), 0, 0),
        (idx("WAIT"), 1, 0),
    ];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], name.len() as u32, 0, name),
        // 字符串实参 @ 槽 1(B2=3 → STR 类型标记)
        ([0x21, 0x03, 0x00, 0x00], arg.len() as u32, 0, arg),
        ([0x00, 0x00, 0x01, 0x00], w_wait.len() as u32, 0, w_wait),
        ([0x00, 0x00, 0x00, 0x00], lhs.len() as u32, 0, lhs),
        ([0x00, 0x00, 0x00, 0x00], rhs.len() as u32, 0, rhs),
        ([0x00, 0x00, 0x01, 0x00], w_wait.len() as u32, 0, w_wait),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_labels(std::collections::HashMap::from([(
        b"CALLEE".to_vec(),
        (2u32, vm.script_id()),
    )]));
    let _ = vm.run(64);
    assert_eq!(
        vm.store().get_opt(&var(VarSpace::Dollar, 0x0900)),
        Some(&Value::Str(b"ES.FIRST".to_vec())),
        "被调方经 $55[1] 读到的必须是调用方传入的字符串实参"
    );
}

/// **成果 98**:VARINFO SEARCH 未命中返回 **-1**。
///
/// 脚本侧证伪:语料 `es.BT.NO.GET` 拿到结果后立刻做 `if (@[0x5bc] != -1)`,
/// 其中 `0x52` 是**一元取负**(`insn.rs`: `0x52 => Unary(Neg)`)→ -1 才是
/// 「未命中」哨兵。旧实现返回元素个数,该判定为真 → 被当下标写数组 →
/// 越界崩溃(实测 `idx[0]=301, bound=301`)。
#[test]
fn varinfo_search_miss_returns_minus_one() {
    let w_op: &[u8] = &[0x42, 1, 0, 1];
    let w_key: &[u8] = &[0x4d, 5, 0, b'"', b'z', b'z', b'z', b'"'];
    let w_ref: &[u8] = &[0x56, 3, 0, 0x24, 0x00, 0x10]; // push $0x1000(数组引用)
    let w_let: &[u8] = &[0x48, 3, 0, 0x40, 0x10, 0x20]; // @0x2010
    let w_wait: &[u8] = &[0x42, 1, 0, 1];
    let groups: &[(u8, u8, u16)] = &[(idx("VARINFO"), 4, 0), (idx("WAIT"), 1, 0)];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], w_ref.len() as u32, 0, w_ref), // 槽 0 SET
        ([14, 0x01, 0x00, 0x00], w_op.len() as u32, 0, w_op),     // 槽 14 SEARCH
        ([17, 0x03, 0x00, 0x00], w_key.len() as u32, 0, w_key),   // 槽 17 STR key
        ([1, 0x00, 0x00, 0x00], w_let.len() as u32, 0, w_let),    // 槽 1 LET
        ([0x00, 0x00, 0x01, 0x00], w_wait.len() as u32, 0, w_wait),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.store_mut()
        .declare_array(&var(VarSpace::Dollar, 0x1000), ElemType::Str, &[3]);
    vm.store_mut()
        .set_elem(&var(VarSpace::Dollar, 0x1000), &[0], Value::Str(b"a".to_vec()))
        .unwrap();
    let _ = vm.run(64);
    assert_eq!(
        vm.store().get_opt(&var(VarSpace::At, 0x2010)),
        Some(&Value::Int(-1)),
        "SEARCH 未命中必须写 -1(脚本侧以 `!= -1` 判定)"
    );
}

/// **成果 99**:上界为**运行期变量**的数组声明不得建数组(否则遮蔽同名标量)。
///
/// 实测 `G_STR2 @[0x0afb][@[0x0c61]]`(s27 g3421):旧实现把变量上界当 0 →
/// 建出长度 1 的 STR 数组,遮蔽 s52 的 `S_INT @[0x0afb] = 10` 标量 →
/// 复合赋值读到空串 → 挂起「字符串 - 语义 Unknown」。
#[test]
fn dynamic_bound_declaration_does_not_shadow_scalar() {
    let scalar_w: &[u8] = &[0x48, 3, 0, 0x40, 0x00, 0x20]; // @0x2000
    let mut arr_w: Vec<u8> = Vec::new();
    arr_w.extend_from_slice(&[0x56, 3, 0, 0x40, 0x00, 0x20]); // 被声明对象
    arr_w.extend_from_slice(&[0x48, 3, 0, 0x40, 0x50, 0x20]); // 变量上界(非常量)
    arr_w.extend_from_slice(&[0x29, 1, 0, 0x00]); // ArrayLoad

    let groups: &[(u8, u8, u16)] = &[(idx("S_INT"), 1, 0), (idx("G_STR2"), 1, 0)];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], scalar_w.len() as u32, 0, scalar_w),
        ([0x00, 0x00, 0x00, 0x00], arr_w.len() as u32, 0, arr_w.as_slice()),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let ctx = yuris_vm::host::ScriptCtx::parse(0, script).unwrap();
    // command_type 用的是 v555 序号 → 分派表也必须是 v555 表
    let dispatch = CommandDispatch::new(CommandProfile::v555());
    let mut store = yuris_value::VariableStore::new();
    let n = yuris_vm::consume_declarations_into(&mut store, &ctx, &dispatch).unwrap();
    assert_eq!(n, 1, "只应消费标量声明(数组那条上界非常量 → 跳过)");
    assert!(
        !store.has_array(&var(VarSpace::At, 0x2000)),
        "非常量上界不得建数组"
    );
    assert!(store.get_opt(&var(VarSpace::At, 0x2000)).is_some());
}

/// **成果 100**:错误态经单一入口,且帧实参播种不再静默吞错。
///
/// 旧行为:`seed_frame_locals` 用 `.ok()` 吞掉求值错误 → 错误态设了却没有
/// 任何信息(事件流里看不出是哪一组)。现在必须冒泡出真实原因。
#[test]
fn frame_seed_error_is_not_silently_swallowed() {
    // 实参窗引用一个**未声明**的数组元素 → 求值必失败
    let bad_arg: &[u8] = &[
        0x56, 3, 0, 0x40, 0x77, 0x09, // push @0x0977(未声明的数组)
        0x42, 1, 0, 1,
        0x29, 1, 0, 0x00, // ArrayLoad
    ];
    let name: &[u8] = &[0x4d, 8, 0, b'"', b'C', b'A', b'L', b'L', b'E', b'E', b'"'];
    let w_wait: &[u8] = &[0x42, 1, 0, 1];
    let groups: &[(u8, u8, u16)] = &[(idx("GOSUB"), 2, 1), (idx("WAIT"), 1, 0)];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], name.len() as u32, 0, name),
        ([0x01, 0x00, 0x00, 0x00], bad_arg.len() as u32, 0, bad_arg),
        ([0x00, 0x00, 0x01, 0x00], w_wait.len() as u32, 0, w_wait),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_labels(std::collections::HashMap::from([(
        b"CALLEE".to_vec(),
        (1u32, vm.script_id()),
    )]));
    let r = vm.run(64);
    match r {
        Err(e) => {
            let s = e.to_string();
            assert!(
                s.contains("0977") || s.contains("undefined"),
                "错误信息须指出真因,得到: {s}"
            );
        }
        Ok(yuris_vm::VmSuspend::Error(s)) => {
            assert!(s.contains("0977") || s.contains("undefined"), "得到: {s}");
        }
        other => panic!("实参求值失败必须显式报错,得到 {other:?}"),
    }
}

/// 参数名查不到 / 表缺失时诚实返回 None(处理器据此回落内置常量)。
#[test]
fn unknown_param_returns_none() {
    let p = CommandProfile::v555();
    assert_eq!(p.slot_of_named("CG", "NO_SUCH_PARAM"), None);
    assert_eq!(p.slot_of_named("NO_SUCH_CMD", "FILE"), None);

    let legacy = CommandProfile::from_names(["CG", "WAIT"], 999, "legacy");
    assert!(!legacy.has_params());
    assert_eq!(legacy.slot_of_named("CG", "FILE"), None);
}

/// 保证 `cmd::` 常量表仍可用(旧测试与工具依赖),且与 v555 表一致。
#[test]
fn legacy_cmd_constants_still_work() {
    let p = CommandProfile::v555();
    assert_eq!(p.index_of("CG"), Some(cmd::CG as u16));
    assert_eq!(p.index_of("WAIT"), Some(cmd::WAIT as u16));
}
