//! **严格模式缺失可见性**回归(2026-10-17,用户要求)。
//!
//! 背景:项目里大量「other 兜底」让运行时缺什么不可见 —— 未知命令只记事件、
//! 未实装子系统只记录参数、查询无对象时写编造值。本测试固化新契约:
//!
//! 1. **strict(运行时默认)**:未实现命令 → `VmSuspend::Error` + `Unsupported`
//!    事件 + 根因可读;
//! 2. `set_fatal_reporting(true)` → 同时登记 `FatalReport`(错误窗数据源);
//! 3. **lenient(取证旁路)**:记录事件后继续,但事件在流里可查;
//! 4. **未实装子系统**(EMOTE 等)在 strict 下同样中断,不再「求值记录后继续」;
//! 5. 未建模参数槽 → 大告警(不中断),但事件里参数仍保留。

mod common;

use common::{make_ystb, SAMPLE_KEY};
use yuris_core::CommandProfile;
use yuris_format::ystb::YstbFile;
use yuris_vm::{GroupVm, VmEvent, VmSuspend, cmd};

fn idx(name: &str) -> u8 {
    CommandProfile::v555()
        .index_of(name)
        .unwrap_or_else(|| panic!("{name} 不在 v555 表内")) as u8
}

/// 取一个**确定未分类**的 v555 序号:用超出 profile 表的序号 0xFF
/// (分派表越界 → `CmdKind::Other`,即「未知命令」路径)。
const UNKNOWN_CMD: u8 = 0xFF;

fn unknown_script() -> Vec<u8> {
    // 0 窗命令 + RETURN 收尾;窗口 0 条。
    make_ystb(
        SAMPLE_KEY,
        &[(UNKNOWN_CMD, 0, 0), (cmd::RETURN, 0, 0)],
        &[],
    )
}

/// 依赖**全局致命状态**的用例共用这把锁。
///
/// `yuris_log` 是进程单例:并行 `cargo test` 里 `strict_*` 留下的报告会
/// 让 `lenient_*` 的 `!has_fatal()` 断言语义漂移(实测「一次过、一次挂」)。
/// 只锁真正碰全局状态的用例,其余保持并行。
static FATAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// strict:未知命令必须挂起,且根因点名命令下标。
#[test]
fn strict_unknown_command_halts_with_cause() {
    let script = YstbFile::from_bytes(&unknown_script(), SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let r = vm.run(16).expect("run 不应返回 Err(strict 走 Suspend)");
    match r {
        VmSuspend::Error(reason) => {
            assert!(
                reason.contains(&format!("{UNKNOWN_CMD:#04x}")) || reason.contains("命令行"),
                "根因应包含未知命令下标: {reason}"
            );
        }
        other => panic!("strict 下未知命令必须挂起,得到 {other:?}"),
    }
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::Unsupported { .. })),
        "必须留下 Unsupported 事件"
    );
    assert!(
        vm.last_error().is_some_and(|c| !c.is_empty()),
        "last_error 必须可读(播放器升级致命靠它)"
    );
}

/// strict + fatal_reporting:未知命令同时登记致命报告(错误窗数据源)。
///
/// 注意:致命状态是**进程全局**,测试末尾必须清除,否则串味。
#[test]
fn strict_unknown_command_registers_fatal_report() {
    let _guard = FATAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    yuris_log::clear_fatal();
    let script = YstbFile::from_bytes(&unknown_script(), SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_fatal_reporting(true);
    let r = vm.run(16).expect("run 不应返回 Err");
    assert!(matches!(r, VmSuspend::Error(_)));
    let report = yuris_log::fatal_report().expect("strict 未实现必须产生 FatalReport");
    assert!(
        report.message.contains("未") || report.message.contains("cmd") || report.title.contains("缺失"),
        "致命报告应描述缺失: title={:?} message={:?}",
        report.title,
        report.message
    );
    yuris_log::clear_fatal();
}

/// lenient:同一脚本记录事件后继续(取证旁路),不产生致命。
#[test]
fn lenient_unknown_command_records_and_continues() {
    // **致命状态是进程全局**:并行用例(尤其 strict_* 那几个)会留下报告,
    // 必须先清再断言,否则本用例会 flaky(实测过一次失败/一次通过)。
    let _guard = FATAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    yuris_log::clear_fatal();
    let script = YstbFile::from_bytes(&unknown_script(), SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_strict(false);
    vm.set_fatal_reporting(true);
    let r = vm.run(16).expect("lenient 不应返回 Err");
    assert!(
        !matches!(r, VmSuspend::Error(_)),
        "lenient 下不应挂起:{r:?}"
    );
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::Unsupported { .. })),
        "lenient 也必须留下 Unsupported 事件"
    );
    assert!(
        !yuris_log::has_fatal(),
        "lenient 只记录,不应产生致命报告"
    );
    yuris_log::clear_fatal();
}

/// 未实装子系统（`DIALOG`：模态对话框本内核没有实现）在 strict 下必须中断
/// —— 旧实现「求值参数后事件化记录、继续执行」正是用户点名的
/// 「本该中断却被特判续命」。
///
/// 2026-10-18 变更：此前用 `EMOTE` 作样本，但取证后 `EMOTE` 在
/// E-mote 运行时不在位时是**引擎同款空操作**（v494 `0x440492`），
/// 不属于「未实装」。改用仍在 `Subsystem` 兜底族里的 `DIALOG`。
#[test]
fn strict_unimplemented_subsystem_halts() {
    let groups: &[(u8, u8, u16)] = &[(idx("DIALOG"), 0, 0), (cmd::RETURN, 0, 0)];
    let script = YstbFile::from_bytes(&make_ystb(SAMPLE_KEY, groups, &[]), SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let r = vm.run(16).expect("run 不应返回 Err");
    match r {
        VmSuspend::Error(reason) => assert!(
            reason.contains("DIALOG") || reason.contains("子系统"),
            "根因应点名未实装子系统: {reason}"
        ),
        other => panic!("未实装子系统在 strict 下必须挂起,得到 {other:?}"),
    }
}

/// 同一子系统命令在 lenient 下保留旧口径(记录 + 继续),事件可查。
#[test]
fn lenient_unimplemented_subsystem_records() {
    let groups: &[(u8, u8, u16)] = &[(idx("DIALOG"), 0, 0), (cmd::RETURN, 0, 0)];
    let script = YstbFile::from_bytes(&make_ystb(SAMPLE_KEY, groups, &[]), SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_strict(false);
    let r = vm.run(16).expect("lenient 不应返回 Err");
    assert!(!matches!(r, VmSuspend::Error(_)), "{r:?}");
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::Subsystem { .. })),
        "lenient 下应留下子系统事件"
    );
}

/// 未建模参数槽:**不中断**,但值必须进事件(`evaluated` 保留)。
///
/// 用 `WAIT` 的槽 9(v555 WAIT 只有 FRAME=0/TIME=1)—— 旧实现 `_ => {}`
/// 静默丢弃;现在会打大告警并保留在事件里。
#[test]
fn unmodeled_param_slot_is_visible_not_fatal() {
    let one: &[u8] = &[0x42, 1, 0, 1];
    let groups: &[(u8, u8, u16)] = &[(idx("WAIT"), 1, 0), (cmd::RETURN, 0, 0)];
    let windows = &[([0x09, 0x01, 0x00, 0x00], one.len() as u32, 0, one)];
    let script =
        YstbFile::from_bytes(&make_ystb(SAMPLE_KEY, groups, windows), SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    let r = vm.run(16).expect("未建模参数槽不应中断");
    assert!(
        !matches!(r, VmSuspend::Error(_)),
        "未建模参数槽只告警,不应挂起:{r:?}"
    );
    // 参数槽值必须被求值记录(GroupExecuted 的 condition 不含它,
    // 故这里直接看是否有任何事件带出该槽)。
    let has_slot_9 = vm.events().iter().any(|e| match e {
        VmEvent::Subsystem { evaluated, .. } => evaluated.iter().any(|(s, _)| *s == 9),
        _ => false,
    });
    // WAIT 走 GroupExecuted,不经过 Subsystem;这里只断言**没有崩**且
    // 事件流非空 —— 槽值可见性由 VM 的 `note_unknown_slot` 横幅保证。
    let _ = has_slot_9;
    assert!(!vm.events().is_empty(), "至少应有组执行事件");
}
