//! P1 验收：**运行期命令分派**按命令名（跨版本稳定），不按序号（跨版本漂移）。
//!
//! 立论（`docs/cross-version-commands.md`）：两个引擎版本共有 116 条命令，
//! 其中 **104 条序号不同**。旧实现 `match g.command_type { cmd::GO => … }`
//! 把某一版的序号当宇宙常量，跨版本必然错位。
//!
//! 本文件不需要游戏样本：用 `common::make_ystb_version` 合成两版脚本，
//! 断言**同语义 → 同结果**。

use yuris_core::CommandProfile;
use yuris_format::ystb::YstbFile;
use yuris_value::{VarRef, VarSpace, Value};
use yuris_vm::{CmdKind, CommandDispatch, GroupVm, VmEvent};

mod common;
use common::{make_ystb_version, SAMPLE_KEY};

/// 命令名 → 该 profile 的序号（YSTB part1 用 u8）。
fn idx(p: &CommandProfile, name: &str) -> u8 {
    u8::try_from(p.index_of(name).expect("命令应存在")).expect("序号应在 u8 内")
}

fn var(space: u8, id: u16) -> VarRef {
    VarRef {
        space: VarSpace::from_prefix(space),
        id,
    }
}

/// 一段「INT 声明 + 初值」脚本的**语义**（与版本无关）。
///
/// `w0` = 变量引用 `@6293`，`w1` = pushint8 7 —— 取自已实证的真实组窗口形态。
const W0: &[u8] = &[0x48, 3, 0, 0x40, 0x95, 0x18];
const W1: &[u8] = &[0x42, 1, 0, 7];

/// 用指定 profile 的序号把上述语义编成 YSTB（header 版本 = profile 版本）。
fn build_script(profile: &CommandProfile) -> Vec<u8> {
    let groups: &[(u8, u8, u16)] = &[(idx(profile, "INT"), 2, 0), (idx(profile, "RETURN"), 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], W0.len() as u32, 0, W0),
        ([0x00, 0x01, 0x00, 0x00], W1.len() as u32, 0, W1),
    ];
    make_ystb_version(SAMPLE_KEY, profile.engine_version(), groups, windows)
}

fn run_script(bytes: &[u8]) -> (Option<Value>, Vec<u8>, bool) {
    let script = YstbFile::from_bytes(bytes, SAMPLE_KEY).expect("ystb");
    let mut vm = GroupVm::load(script).expect("vm");
    vm.run(100).expect("run");
    let value = vm.store().get(&var(0x40, 6293)).ok().cloned();
    // 事件里记录的命令序号（= 该版 YSCM 下标）
    let decl_cmds: Vec<u8> = vm
        .events()
        .iter()
        .filter_map(|e| match e {
            VmEvent::Declaration { command, .. } => Some(*command),
            _ => None,
        })
        .collect();
    let unsupported = vm
        .events()
        .iter()
        .any(|e| matches!(e, VmEvent::Unsupported { .. }));
    (value, decl_cmds, unsupported)
}

/// **核心验收**：同一段语义，用两版各自序号编码 → 结果必须一致。
#[test]
fn same_semantics_same_result_across_engine_versions() {
    let p555 = CommandProfile::v555();
    let p494 = CommandProfile::v494();

    // 前提取证：两版的 INT / RETURN 序号确实不同（否则本测试无意义）
    assert_ne!(p555.index_of("INT"), p494.index_of("INT"));
    assert_ne!(p555.index_of("RETURN"), p494.index_of("RETURN"));
    assert_eq!(p555.index_of("INT"), Some(0x32));
    assert_eq!(p494.index_of("INT"), Some(0x33));
    assert_eq!(p555.index_of("RETURN"), Some(0x4f));
    assert_eq!(p494.index_of("RETURN"), Some(0x4d));

    let b555 = build_script(&p555);
    let b494 = build_script(&p494);

    let (v555, ev555, uns555) = run_script(&b555);
    let (v494, ev494, uns494) = run_script(&b494);

    assert!(!uns555, "555 脚本不应有未实现命令事件");
    assert!(!uns494, "494 脚本不应有未实现命令事件（旧实现此处必然失败）");
    assert_eq!(v555, Some(Value::Int(7)), "555 侧初值应为 7");
    assert_eq!(v494, Some(Value::Int(7)), "494 侧初值应为 7");

    // 事件里的命令序号各自等于本版 YSCM 下标（真值可追溯）
    assert_eq!(ev555, vec![idx(&p555, "INT")]);
    assert_eq!(ev494, vec![idx(&p494, "INT")]);
}

/// 用**游戏自身 YSCM**（运行期 profile）覆盖时，任意序号都按其名字分派。
#[test]
fn injected_profile_overrides_builtin() {
    // 造一个「挪位」profile：把 INT 挪到 0x7a，模拟另一版本的 YSCM。
    let mut names: Vec<String> = CommandProfile::v555()
        .names()
        .iter()
        .cloned()
        .collect();
    let int_old = names.iter().position(|n| n == "INT").unwrap();
    names.swap(int_old, 0x78);
    let shifted = CommandProfile::from_names(names, 777, "test: shifted profile");

    assert_eq!(idx(&shifted, "INT"), 0x78);

    let groups: &[(u8, u8, u16)] = &[
        (idx(&shifted, "INT"), 2, 0),
        (idx(&shifted, "RETURN"), 0, 0),
    ];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], W0.len() as u32, 0, W0),
        ([0x00, 0x01, 0x00, 0x00], W1.len() as u32, 0, W1),
    ];
    // header 版本用 777（未知）→ load() 走「未解析」，随后由 profile 覆盖。
    let bytes = make_ystb_version(SAMPLE_KEY, 777, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    assert!(
        !vm.is_dispatch_resolved(),
        "未知引擎版本 + 无 YSCM → 分派应为未解析（不得猜测）"
    );
    vm.set_command_profile(Some(shifted));
    assert!(vm.is_dispatch_resolved());
    vm.run(100).unwrap();

    assert_eq!(vm.store().get(&var(0x40, 6293)).unwrap(), &Value::Int(7));
    assert!(vm
        .events()
        .iter()
        .all(|e| !matches!(e, VmEvent::Unsupported { .. })));
}

/// 未知版本且未注入 YSCM：命令**显式报未实现**，不静默执行错误语义。
#[test]
fn unknown_version_without_yscm_is_explicitly_unsupported() {
    let bytes = make_ystb_version(
        SAMPLE_KEY,
        999,
        &[(0x32, 0, 0)], // 序号 0x32，但 999 版无内置表
        &[],
    );
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    assert!(!vm.is_dispatch_resolved());
    let r = vm.run(10);
    match r {
        Ok(yuris_vm::VmSuspend::Error(msg)) => {
            assert!(msg.contains("未逆向") || msg.contains("未实现"), "错误须可诊断: {msg}");
        }
        other => panic!("未知版本应挂起报错，实得 {other:?}"),
    }
    assert!(vm
        .events()
        .iter()
        .any(|e| matches!(e, VmEvent::Unsupported { .. })));
}

/// 内置版本表：官方 SDK 的版本必须都能解析出分派表。
#[test]
fn builtin_sdk_versions_dispatch_by_name() {
    for (ver, name, kind) in [
        (479, "GO", CmdKind::Go),
        (479, "WAIT", CmdKind::Wait),
        (494, "VARACT", CmdKind::VarAct),
        (495, "VARACT", CmdKind::VarAct),
        (555, "VARACT", CmdKind::VarAct),
    ] {
        let d = CommandDispatch::for_engine_version(ver)
            .unwrap_or_else(|| panic!("版本 {ver} 应有内置分派表"));
        let i = u16::from(idx(d.profile(), name));
        assert_eq!(d.kind(i), kind, "{ver}:{name}");
    }
    // 未知版本 → None（不猜）
    assert!(CommandDispatch::for_engine_version(31337).is_none());
}
