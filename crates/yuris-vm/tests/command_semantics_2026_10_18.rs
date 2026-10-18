//! 2026-10-18 新增命令语义的回归（成果 188）。
//!
//! 每条测试都对应一处**引擎处理体取证**，注释里写明地址与依据：
//! - `ERROR`（v494 `0x4411f8`）：错误输出级别配置，不动 pc/变量/场景
//! - `FILEACT`（v494 `0x441234`）：文件操作意图，VM 不碰平台 I/O
//! - `SYSTEM`（v494 `0x4500d0`）：屏保 / 后台执行 / 屏幕尺寸 / 计时器
//! - `EMOTE`（v494 `0x440468`）：E-mote 运行时不在位 → 空操作
//! - `GOSUB`/`GO` 标签未命中：告警 + 跳过（不再冒充「未实现」）

mod common;

use common::{make_ystb, SAMPLE_KEY};
use yuris_core::CommandProfile;
use yuris_format::ystb::YstbFile;
use yuris_vm::{CmdKind, GroupVm, VmEvent, VmSuspend};

/// 按**命令名**取 v555 序号（测试用 v555 头脚本）。
fn idx(name: &str) -> u8 {
    let p = CommandProfile::v555();
    u8::try_from(p.index_of(name).unwrap_or_else(|| panic!("{name} 不在表里"))).unwrap()
}

/// 跑一小段脚本，返回 `(suspend, vm)`。
fn run(groups: &[(u8, u8, u16)], windows: &[([u8; 4], u32, u32, &[u8])]) -> (VmSuspend, GroupVm) {
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let s = vm.run(200).unwrap();
    (s, vm)
}

/// `ERROR[COM_CG=0]`（SDK 示例 `es_ERIS.yst:33` 的真实形态）：
/// **不进错误态、不挂起**，只记录配置。
///
/// 引擎处理体 `0x4411f8` 全函数没有任何 pc / 调用栈 / 变量 / 场景写操作。
#[test]
fn error_is_config_not_abort() {
    // 槽 3 = COM_CG（v555 参数表的第一个 COM_*），值 0
    let w_zero: &[u8] = &[0x42, 1, 0, 0];
    let groups: &[(u8, u8, u16)] = &[(idx("ERROR"), 1, 0), (idx("RETURN"), 0, 0)];
    let windows = &[([0x03, 0x00, 0x00, 0x00], w_zero.len() as u32, 0, w_zero)];
    let (s, vm) = run(groups, windows);
    assert!(
        !matches!(s, VmSuspend::Error(_)),
        "ERROR 是配置命令，不得挂起:{s:?}"
    );
    assert!(
        vm.executed_groups() >= 2,
        "ERROR 之后应继续执行(实际 {} 组)",
        vm.executed_groups()
    );
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::ErrorConfig { .. })),
        "应留下 ErrorConfig 事件"
    );
    assert_eq!(
        vm.kind_of(idx("ERROR")),
        CmdKind::Error,
        "ERROR 必须分类到 Error，而不是 Subsystem"
    );
}

/// `FILEACT[FOLDER=… CREATE=1]`：VM 只发意图事件，**不碰平台文件系统**。
#[test]
fn fileact_emits_intent_only() {
    // 槽 2 = FOLDER（字符串）、槽 3 = CREATE(1)
    // M-串载荷 = `"save"` = 6 字节（含两侧引号）
    let w_folder: &[u8] = &[0x4d, 6, 0, b'"', b's', b'a', b'v', b'e', b'"'];
    let w_create: &[u8] = &[0x42, 1, 0, 1];
    let groups: &[(u8, u8, u16)] = &[(idx("FILEACT"), 2, 0), (idx("RETURN"), 0, 0)];
    let windows = &[
        ([0x02, 0x00, 0x00, 0x00], w_folder.len() as u32, 0, w_folder),
        ([0x03, 0x00, 0x00, 0x00], w_create.len() as u32, 0, w_create),
    ];
    let (s, vm) = run(groups, windows);
    assert!(!matches!(s, VmSuspend::Error(_)), "FILEACT 不得挂起:{s:?}");
    let ev = vm
        .events()
        .iter()
        .find_map(|e| match e {
            VmEvent::FileAct {
                folder, create, ..
            } => Some((folder.clone(), *create)),
            _ => None,
        })
        .expect("应发出 FileAct 事件");
    assert_eq!(ev.0.as_deref(), Some("save"), "FOLDER 应原样带出");
    assert_eq!(ev.1, Some(1), "CREATE 应原样带出");
}

/// `SYSTEM[SX=… SY=…]`：两者都 > 0 才改分辨率（引擎 `0x450151` 的两个 jle）。
#[test]
fn system_screen_size_needs_both_positive() {
    let w: &[u8] = &[0x42, 1, 0, 100]; // pushint8 100
    // 只给 SX=100、SY=0：引擎不应用
    let groups: &[(u8, u8, u16)] = &[(idx("SYSTEM"), 2, 0), (idx("RETURN"), 0, 0)];
    let zero: &[u8] = &[0x42, 1, 0, 0];
    let windows = &[
        ([0x02, 0x00, 0x00, 0x00], w.len() as u32, 0, w),
        ([0x03, 0x00, 0x00, 0x00], zero.len() as u32, 0, zero),
    ];
    let (s, vm) = run(groups, windows);
    assert!(!matches!(s, VmSuspend::Error(_)), "{s:?}");
    let (w_now, h_now) = vm.screen_size();
    assert_ne!(
        (w_now, h_now),
        (100, 0),
        "SY=0 时不得把分辨率改成 (100, 0)"
    );
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::SystemConfig { .. })),
        "应留下 SystemConfig 事件"
    );
}

/// `SYSTEM[SCREENSAVER=1]` 置位可读回（引擎 `0x5b4748`）。
#[test]
fn system_screensaver_flag_roundtrip() {
    let one: &[u8] = &[0x42, 1, 0, 1];
    let groups: &[(u8, u8, u16)] = &[(idx("SYSTEM"), 1, 0), (idx("RETURN"), 0, 0)];
    let windows = &[([0x00, 0x00, 0x00, 0x00], one.len() as u32, 0, one)];
    let (_s, vm) = run(groups, windows);
    assert!(vm.screensaver_suppressed(), "SCREENSAVER=1 应置位");
    assert_eq!(vm.emote_runtime(), false, "E-mote 运行时缺省不在位");
}

/// `EMOTE`：运行时不在位 + 无参数 → 空操作，不挂起。
///
/// 引擎 `0x440492`：`0x802a28`（YSEmote.dll 装载成功标志）为 0 时直接返回 0。
#[test]
fn emote_without_runtime_is_noop() {
    let groups: &[(u8, u8, u16)] = &[(idx("EMOTE"), 0, 0), (idx("RETURN"), 0, 0)];
    let (s, vm) = run(groups, &[]);
    assert!(!matches!(s, VmSuspend::Error(_)), "EMOTE 空操作不得挂起:{s:?}");
    assert_eq!(vm.executed_groups(), 2, "EMOTE 与 RETURN 都应执行");
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::Emote { runtime: false, .. })),
        "应记录 runtime=false 的 Emote 事件"
    );
}

/// `EMOTEINFO`：引擎侧共享 stub（`xor eax,eax; ret`）→ 空操作。
///
/// **注意**：`EMOTEINFO` 只在 494/495 的命令表里（v555 表无此项），故本测试
/// 用 v494 头脚本 —— 与真实语料一致。
#[test]
fn emoteinfo_is_noop_like_engine_stub() {
    let p = CommandProfile::v494();
    let emoteinfo = u8::try_from(p.index_of("EMOTEINFO").unwrap()).unwrap();
    let ret = u8::try_from(p.index_of("RETURN").unwrap()).unwrap();
    let groups: &[(u8, u8, u16)] = &[(emoteinfo, 0, 0), (ret, 0, 0)];
    let bytes = common::make_ystb_version(SAMPLE_KEY, 494, groups, &[]);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let s = vm.run(200).unwrap();
    assert!(!matches!(s, VmSuspend::Error(_)), "{s:?}");
    assert_eq!(vm.executed_groups(), 2);
}

/// 标签未命中：发 `LabelMiss` + 继续（**不**发 `Unsupported`、不进错误态）。
#[test]
fn label_miss_is_alert_and_skip() {
    // M-串载荷 = `"NOSUCHLB"` = 10 字节
    let name: &[u8] = &[
        0x4d, 10, 0, b'"', b'N', b'O', b'S', b'U', b'C', b'H', b'L', b'B', b'"',
    ];
    let groups: &[(u8, u8, u16)] = &[
        (idx("GO"), 1, 0),
        (idx("LET"), 2, 0),
        (idx("RETURN"), 0, 0),
    ];
    // LET 目标 @7000 ← 1（用于证明「跳过 GO 后确实继续执行」）
    // `0x48` = pushvar：opcode + u16 长度 + 4 字节 id（共 6 字节）
    let var_ref: &[u8] = &[0x48, 3, 0, 0x58, 0x1b, 0x00]; // @7000 (0x1b58)
    let one: &[u8] = &[0x42, 1, 0, 1];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], name.len() as u32, 0, name),
        ([0x00, 0x01, 0x00, 0x00], var_ref.len() as u32, 0, var_ref),
        ([0x00, 0x01, 0x00, 0x00], one.len() as u32, 0, one),
    ];
    let (s, vm) = run(groups, windows);
    assert!(!matches!(s, VmSuspend::Error(_)), "标签未命中不得挂起:{s:?}");
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::LabelMiss { skipped: true, .. })),
        "应发 LabelMiss(skipped=true)"
    );
    assert!(
        !vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::Unsupported { .. })),
        "标签未命中不得记为 Unsupported(那是内核缺功能的语义)"
    );
    assert!(vm.executed_groups() >= 3, "跳过 GO 后应继续执行后续组");
}
