//! 混版本封包的分派回归（2026-10-18）。
//!
//! ## 立论
//!
//! 一个封包里可以同时存在**不同编译器版本**产出的脚本。实测某作 160 个脚本
//! 里 5 个是 v479 头、155 个是 v494 头。
//!
//! ## 引擎真值（勘误 2026-10-18，用户指出「必须按游戏来」）
//!
//! 引擎只装载**一张**命令表：启动链读游戏自己的 `ysc.ybn`，填进 `0x7aae40`
//! 的 119 项分派表（初始化器 `FUN_0045cd30`），分派是
//! `CALL [handler[cmd_index]]`。**没有第二张表，也没有按脚本头版本切表的
//! 逻辑。**
//!
//! 因此本文件锁住两条契约：
//! 1. **有游戏 YSCM 时，所有脚本一律用游戏表**（与脚本头版本无关）；
//! 2. 无游戏 YSCM（工具/合成测试）时，未知版本 → 未解析空表（不借用别的版本）。
//!
//! 项目里 `SDK_479/495/V555` 那几张表是**给人查命令清单的参考资料**
//! （`docs/opcode/yscm-commands-*.json` 的镜像），不是运行期语义 ——
//! 拿它兜底会让同一封包的脚本被两套序号解释，且覆盖不到商业游戏私有的
//! 命令增删 ⇒ 静默错解。

mod common;

use common::{make_ystb_version, SAMPLE_KEY};
use yuris_core::CommandProfile;
use yuris_format::ystb::YstbFile;
use yuris_vm::{CmdKind, CommandDispatch, GroupVm};

/// 有游戏 YSCM 时，**序号始终按游戏表解析**，不看脚本头版本。
///
/// 构造两条脚本，`command_type` 都是 30：
/// - v479 头（v479 表里 30 = `IF`）
/// - v494 头（v494 表里 30 = `G_FLT`）
///
/// 注入 v494 的 YSCM 后，**两者都应**解析成 `G_FLT`。
#[test]
fn game_table_wins_over_script_header_version() {
    let game = CommandProfile::v494();
    let gflt_494 = u8::try_from(game.index_of("G_FLT").unwrap()).unwrap();
    assert_eq!(gflt_494, 30);

    for header_version in [479u32, 494] {
        let bytes = make_ystb_version(SAMPLE_KEY, header_version, &[(30, 0, 0)], &[]);
        let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
        let mut vm = GroupVm::load(script).unwrap();
        vm.set_command_profile(Some(game.clone()));
        assert_eq!(
            vm.kind_of(30),
            CmdKind::DeclOther,
            "v{header_version} 头脚本的序号 30 必须按**游戏表**解析成 G_FLT"
        );
    }
}

/// 无游戏 YSCM 时：`load()` 按脚本头版本取自内置表（工具/合成测试的入口）。
#[test]
fn without_game_table_builtin_is_used_on_load() {
    let p479 = CommandProfile::sdk_479();
    let if_idx = u8::try_from(p479.index_of("IF").unwrap()).unwrap();
    let bytes = make_ystb_version(SAMPLE_KEY, 479, &[(if_idx, 0, 0)], &[]);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let vm = GroupVm::load(script).unwrap();
    assert!(vm.is_dispatch_resolved(), "有内置表时 load 应建立有效分派");
    assert_eq!(vm.kind_of(if_idx), CmdKind::If);
}

/// 未知版本 + 无 YSCM → 未解析表（任何命令都显式报未实现，绝不借用别的版本）。
#[test]
fn unknown_version_without_yscm_stays_unresolved() {
    let bytes = make_ystb_version(SAMPLE_KEY, 9876, &[(30, 0, 0)], &[]);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let vm = GroupVm::load(script).unwrap();
    assert!(!vm.is_dispatch_resolved(), "未知版本无 YSCM 必须保持未解析");
    assert_eq!(vm.kind_of(30), CmdKind::Other);
}

/// `dispatch_for_version`：有游戏表时，无论传什么版本号都返回**同一张表**。
#[test]
fn dispatch_is_game_table_regardless_of_version() {
    let game = CommandProfile::v494();
    let bytes = make_ystb_version(SAMPLE_KEY, 494, &[], &[]);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_command_profile(Some(game));
    let a = vm.dispatch_for_version(494);
    let b = vm.dispatch_for_version(479);
    let c = vm.dispatch_for_version(555);
    assert_eq!(a.profile().engine_version(), 494);
    assert_eq!(
        b.profile().engine_version(),
        494,
        "版本号被忽略（引擎只有一张表）"
    );
    assert_eq!(c.profile().engine_version(), 494);
    assert_eq!(a.profile().source(), b.profile().source());
    assert!(std::sync::Arc::ptr_eq(&a, &b), "同一张表应共享同一个 Arc");
}

/// `CommandDispatch` 自身：两版的同一序号指向不同命令（错位的事实基础，
/// 也是「为什么必须只用游戏表」的量化依据）。
#[test]
fn same_index_different_command_across_versions() {
    let d479 = CommandDispatch::new(CommandProfile::sdk_479());
    let d494 = CommandDispatch::new(CommandProfile::v494());
    assert_eq!(d479.name_of(30), Some("IF"));
    assert_eq!(d494.name_of(30), Some("G_FLT"));
    assert_ne!(d479.kind(30), d494.kind(30));
}
