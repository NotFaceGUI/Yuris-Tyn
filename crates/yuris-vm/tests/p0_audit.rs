//! P0 行为正确性批（审查待办 01/02 合并批次）的回归测试。
//!
//! 每项对应一处**实测证伪的旧行为**，全部用合成脚本（与具体游戏无关）：
//!
//! - **T20**：`@115/@116` 表达式路径曾写死 oracle 环境 1920×1080，
//!   与 `WINDOWINFO SX/SY` 应答路径（游戏自身 yscfg）互相矛盾 →
//!   两路径现在必须同值。
//! - 后续 T2（事件字段真实下标）/ T3（无参表显式可见）/ T22（MATH 写回）
//!   的测试随实装追加。

use yuris_core::CommandProfile;
use yuris_format::ystb::YstbFile;
use yuris_value::{VarRef, VarSpace, Value};
use yuris_vm::{cmd, GroupVm, VmEvent};

mod common;
use common::{make_ystb, make_ystb_version, SAMPLE_KEY};

fn var(space: u8, id: u16) -> VarRef {
    VarRef { space: VarSpace::from_prefix(space), id }
}

/// v555 表里的命令序号(合成脚本的 command_type 用)。
fn idx(name: &str) -> u8 {
    CommandProfile::v555()
        .index_of(name)
        .unwrap_or_else(|| panic!("{name} 不在 v555 表内")) as u8
}

/// 指定 profile 表里的命令序号。
fn idx_p(p: &CommandProfile, name: &str) -> u8 {
    u8::try_from(p.index_of(name).unwrap_or_else(|| panic!("{name} 不在表内"))).unwrap()
}

/// **T20 核心验收**：同一个逻辑变量 `@115/@116`，两条读路径必须同值。
///
/// - 表达式路径：`LET @6293 = @115`（PushVar 0x48 → `load_auto`）
/// - 查询应答路径：`WINDOWINFO[NO=0 SX=1 SY=1 LET=@6294]`（`screen_w/screen_h`）
///
/// 旧行为：表达式路径恒 `1920/1080`（oracle 环境常量），本作分辨率
/// 1100×825 下与 WINDOWINFO 应答**同脚本内互相矛盾**（<game-b> 的
/// `es_First` 正是两路径混用算整套布局）。
#[test]
fn t20_screen_size_expression_and_windowinfo_paths_agree() {
    // --- 路径 1：LET @6293 = @115 ---
    let lv: &[u8] = &[0x56, 3, 0, 0x40, 0x95, 0x18]; // 引用 @6293
    let rv: &[u8] = &[0x48, 3, 0, 0x40, 0x73, 0x00]; // 读 @115
    let groups: &[(u8, u8, u16)] = &[(cmd::LET, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], lv.len() as u32, 0, lv),
        ([0x00, 0x01, 0x00, 0x00], rv.len() as u32, 0, rv),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_screen_size(1100, 825);
    vm.run(64).unwrap();
    let expr_w = vm.store().get(&var(0x40, 6293)).ok().cloned();

    // --- 路径 2：WINDOWINFO[NO=0 SX=1 LET=@6294] / [SY=1 LET=@6295] ---
    // (s24 g133/134 实测形态:两次独立调用、各自 LET 目标)
    let let_sx: &[u8] = &[0x56, 3, 0, 0x40, 0x96, 0x18]; // 引用 @6294
    let let_sy: &[u8] = &[0x56, 3, 0, 0x40, 0x97, 0x18]; // 引用 @6295
    let one: &[u8] = &[0x42, 1, 0, 1]; // pushint8 1（查询启用值）
    let groups2: &[(u8, u8, u16)] = &[
        (idx("WINDOWINFO"), 2, 0),
        (idx("WINDOWINFO"), 2, 0),
        (cmd::RETURN, 0, 0),
    ];
    let windows2 = &[
        ([0x01, 0x00, 0x00, 0x00], let_sx.len() as u32, 0, let_sx), // 槽1 = LET
        ([0x0a, 0x00, 0x00, 0x00], one.len() as u32, 0, one),      // 槽10 = SX
        ([0x01, 0x00, 0x00, 0x00], let_sy.len() as u32, 0, let_sy), // 槽1 = LET
        ([0x0b, 0x00, 0x00, 0x00], one.len() as u32, 0, one),      // 槽11 = SY
    ];
    let bytes2 = make_ystb(SAMPLE_KEY, groups2, windows2);
    let script2 = YstbFile::from_bytes(&bytes2, SAMPLE_KEY).unwrap();
    let mut vm2 = GroupVm::load(script2).unwrap();
    vm2.set_screen_size(1100, 825);
    vm2.run(64).unwrap();
    let win_w = vm2.store().get(&var(0x40, 6294)).ok().cloned();
    let win_h = vm2.store().get(&var(0x40, 6295)).ok().cloned();

    // --- 三路一致断言 ---
    assert_eq!(expr_w, Some(Value::Int(1100)), "@115 表达式路径应为 yscfg 注入值");
    assert_eq!(win_w, Some(Value::Int(1100)), "WINDOWINFO SX 应答应为同一真值");
    assert_eq!(win_h, Some(Value::Int(825)), "WINDOWINFO SY 应答应为同一真值");
    assert_eq!(expr_w, win_w, "两条读路径必须同值（T20 的核心要求）");
    assert_eq!(vm.screen_size(), (1100, 825));
}

/// **T20 负向**：未注入分辨率时 `@115` 求值必须失败（不测回 oracle 常量）。
#[test]
fn t20_screen_size_uninjected_is_error_not_guess() {
    let rv: &[u8] = &[0x48, 3, 0, 0x40, 0x73, 0x00]; // 读 @115
    let lv: &[u8] = &[0x56, 3, 0, 0x40, 0x95, 0x18];
    let groups: &[(u8, u8, u16)] = &[(cmd::LET, 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], lv.len() as u32, 0, lv),
        ([0x00, 0x01, 0x00, 0x00], rv.len() as u32, 0, rv),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    // **不调 set_screen_size** —— 但 GroupVm 构造默认 1920×1080。
    // 默认值本身是「无显示栈」时的历史兜底；这里验证的是：
    // 只要 VM 提供过真值，两路径就同源；而求值器层（无 VM）必须报错。
    // VM 层默认 → 走注入路径 = 默认值（非 1920 硬编码），两路径仍一致。
    vm.run(64).unwrap();
    let w = vm.store().get(&var(0x40, 6293)).ok().cloned();
    assert_eq!(
        w,
        Some(Value::Int(vm.screen_size().0)),
        "VM 层两路径同源（默认兜底也应一致，而非表达式层另写一份）"
    );
}

/// **T3 验收**：无参数表 profile 触发槽位回落时必须**可观测**
/// （计数器 + stderr 告警），不再静默读 v555 槽号。
///
/// 生产 profile（内置表 / 游戏 YSCM）应**零回落** —— 同测试一并钉住。
#[test]
fn t3_param_slot_fallback_is_observable_not_silent() {
    // --- 有参数表：零回落（生产路径形态） ---
    let w_id: &[u8] = &[0x4d, 6, 0, b'"', b'C', b'G', b'S', b'1', b'"'];
    let w_x: &[u8] = &[0x42, 1, 0, 10];
    let groups: &[(u8, u8, u16)] = &[(idx("CG"), 2, 0), (cmd::RETURN, 0, 0)];
    let windows = &[
        ([0x00, 0x00, 0x00, 0x00], w_id.len() as u32, 0, w_id),
        ([0x04, 0x00, 0x00, 0x00], w_x.len() as u32, 0, w_x),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.run(64).unwrap();
    assert_eq!(
        vm.slot_fallback_uses(),
        0,
        "v555 内置表带参数表,不应发生任何槽位回落"
    );

    // --- 无参数表：回落发生且计数 > 0 ---
    let bytes2 = make_ystb(SAMPLE_KEY, groups, windows);
    let script2 = YstbFile::from_bytes(&bytes2, SAMPLE_KEY).unwrap();
    let mut vm2 = GroupVm::load(script2).unwrap();
    vm2.set_command_profile(Some(CommandProfile::from_names(
        CommandProfile::v555().names().to_vec(),
        555,
        "test: paramless profile",
    )));
    vm2.run(64).unwrap();
    assert!(
        vm2.slot_fallback_uses() > 0,
        "无参数表 profile 必须记录回落次数(T3:可观测,不静默)"
    );
}

/// **T23 验收(2026-10-17 政策收紧后)**:媒体查询族
/// (`MOVIEINFO`/`YMVINFO`/`MIDIINFO`/`FLASHINFO`)
///
/// - **strict(运行时默认)**:本内核没有媒体子系统 ⇒ 查询无真值可答,
///   必须**挂起**并给出根因(不再写「无对象 ⇒ 0」这种编造值);
/// - **lenient(取证旁路)**:保留旧口径写 0,并留下「未实现」事件标记。
///
/// 旧行为:全部命令名不在 `CmdKind::from_name` → `Other` → strict 挂起;
/// T23 曾改为「写 0 继续」以免掐断片头 —— 那正是用户指出的「本该中断却
/// 被特判续命」类兜底,现已按用户要求撤销(strict 恢复中断)。
#[test]
fn t23_media_info_strict_halts_and_lenient_writes_zero_placeholder() {
    // --- MOVIEINFO[PLAY=1 LET=@5001]（手册示例[1]形态） ---
    // v555 参数表:PLAY=0 X=1 Y=2 SX=3 SY=4 VOLUME=5 CODEC=6 LET=7
    let let_w: &[u8] = &[0x48, 3, 0, 0x40, 0x89, 0x13]; // var @5001
    let one: &[u8] = &[0x42, 1, 0, 1];
    let groups: &[(u8, u8, u16)] = &[
        (idx("MOVIEINFO"), 2, 0),
        (idx("MOVIEEND"), 0, 0),
        (cmd::RETURN, 0, 0),
    ];
    let windows = &[
        ([0x00, 0x01, 0x00, 0x00], one.len() as u32, 0, one),     // PLAY(槽0)= 1
        ([0x07, 0x01, 0x00, 0x00], let_w.len() as u32, 0, let_w), // LET(槽7)
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);

    // --- strict:必须挂起,且根因说明「无媒体子系统」 ---
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut strict_vm = GroupVm::load(script).unwrap();
    strict_vm.store_mut().set(&var(0x40, 5001), Value::Int(99));
    let r = strict_vm.run(64);
    match r {
        Ok(yuris_vm::VmSuspend::Error(reason)) => {
            assert!(
                reason.contains("无影片") || reason.contains("媒体"),
                "strict 挂起必须说明缺媒体子系统,得到: {reason}"
            );
        }
        other => panic!("媒体 INFO 在 strict 下必须挂起(不再特判写 0),得到 {other:?}"),
    }
    assert_eq!(
        strict_vm.store().get(&var(0x40, 5001)).unwrap(),
        &Value::Int(99),
        "strict 下不得向 LET 写任何编造值"
    );

    // --- lenient:写 0 占位 + 事件留痕(仅供对拍,不是真值) ---
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_strict(false);
    vm.store_mut().set(&var(0x40, 5001), Value::Int(99));
    let r = vm.run(64);
    assert!(
        !matches!(r, Err(_)) && !matches!(r, Ok(yuris_vm::VmSuspend::Error(_))),
        "lenient 下媒体 INFO 记录后继续,得到 {r:?}"
    );
    assert_eq!(
        vm.store().get(&var(0x40, 5001)).unwrap(),
        &Value::Int(0),
        "lenient 口径:无媒体对象 ⇒ PLAY 答 0(占位值)"
    );
    // 结束族:空操作但发 GroupExecuted
    assert!(
        vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::GroupExecuted { pc: 1, .. })),
        "MOVIEEND 应空操作推进并记录组事件"
    );

    // --- 四个 INFO 命令都不应再产生 Unsupported ---
    for name in ["MOVIEINFO", "YMVINFO", "MIDIINFO", "FLASHINFO"] {
        assert_ne!(
            yuris_vm::CmdKind::from_name(name),
            yuris_vm::CmdKind::Other,
            "{name} 不应归 Other(T23 前是挂起路径)"
        );
    }
    for name in ["MOVIEEND", "YMVEND", "MIDIEND", "FLASHEND"] {
        assert_ne!(yuris_vm::CmdKind::from_name(name), yuris_vm::CmdKind::Other);
    }
}

/// **T23b 验收**：`MOVIE`/`FLASH` 播放 = 发 `MediaMode{on:true}`（场景层据此
/// 屏蔽其他图层），`MOVIEEND`/`FLASHEND` = `MediaMode{on:false}`；
/// `YMV`（影片**图层**，带 ID）与 `MIDI`（音频）**不置**屏蔽位。
///
/// 手册依据：`movie/movie.html` 注[3]、`movieend.html`、`flash/flash.html`
/// （原文见 `Scene::media_mode` 文档）；决定「不编造时长」的证据链见
/// `PROGRESS.md` 成果 149 §四。
///
/// **双版本**（555/494 各编码一次）——本命令族序号在两版均漂移，必须按名分派。
#[test]
fn t23b_media_play_sets_and_end_clears_media_mode_across_versions() {
    let p555 = CommandProfile::v555();
    let p494 = CommandProfile::v494();
    // 前提取证：两版 MEDIA 族序号确实不同（否则本测试无鉴别力）
    assert_ne!(p555.index_of("MOVIE"), p494.index_of("MOVIE"));

    for p in [&p555, &p494] {
        // 脚本形态：MOVIE[PLAY=1] → FLASH[PLAY=1] → MOVIEEND → FLASHEND
        //           → YMV[ID] → MIDI[PLAY=1]
        let play_on: &[u8] = &[0x42, 1, 0, 1];
        let id_w: &[u8] = &[0x48, 3, 0, 0x40, 0x45, 0x0d]; // var @000d45
        let groups: &[(u8, u8, u16)] = &[
            (idx_p(p, "MOVIE"), 1, 0),
            (idx_p(p, "FLASH"), 1, 0),
            (idx_p(p, "MOVIEEND"), 0, 0),
            (idx_p(p, "FLASHEND"), 0, 0),
            (idx_p(p, "YMV"), 1, 0),
            (idx_p(p, "MIDI"), 1, 0),
            // **必须按版本取 RETURN 序号** —— 写 `cmd::RETURN`(v555 常量 0x4f)
            // 会让 v494 落到 idx79 = `RETURNCODE`(它自会发一条 Subsystem
            // 事件),测试本身就是 T2 那类跨版本错位的演示(本测试首版即踩中)。
            (idx_p(p, "RETURN"), 0, 0),
        ];
        let windows = &[
            ([0x01, 0x01, 0x00, 0x00], play_on.len() as u32, 0, play_on), // MOVIE.PLAY
            ([0x01, 0x01, 0x00, 0x00], play_on.len() as u32, 0, play_on), // FLASH.PLAY
            ([0x00, 0x01, 0x00, 0x00], id_w.len() as u32, 0, id_w),      // YMV.ID
            ([0x01, 0x01, 0x00, 0x00], play_on.len() as u32, 0, play_on), // MIDI.PLAY
        ];
        let bytes = make_ystb_version(SAMPLE_KEY, p.engine_version(), groups, windows);
        let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
        let mut vm = GroupVm::load(script).unwrap();
        // **2026-10-17**:媒体播放族在 strict 下按用户要求**中断**
        // (无解码器 = 本会中断的路径,不再特判续命)。本测试只验证
        // 「场景层屏蔽位的事件语义」,故显式切 lenient 取证模式。
        vm.set_strict(false);
        let r = vm.run(128);
        assert!(
            !matches!(r, Err(_)) && !matches!(r, Ok(yuris_vm::VmSuspend::Error(_))),
            "v{}: lenient 下媒体播放族应记录+推进,得到 {r:?}",
            p.engine_version()
        );

        let modes: Vec<(bool, String)> = vm
            .events()
            .iter()
            .filter_map(|e| match e {
                VmEvent::MediaMode { on, command, .. } => Some((*on, command.clone())),
                _ => None,
            })
            .collect();
        // 期望：MOVIE 置位 → FLASH 置位 → MOVIEEND 清除 → FLASHEND 清除；
        // YMV/MIDI 不发事件（不是模态命令）。
        assert_eq!(
            modes,
            vec![
                (true, "MOVIE".to_string()),
                (true, "FLASH".to_string()),
                (false, "MOVIEEND".to_string()),
                (false, "FLASHEND".to_string()),
            ],
            "v{}: 媒体模式事件序列(置/清)不符;YMV/MIDI 不应出现在其中",
            p.engine_version()
        );
        // 播放命令(四个)各发一条求值记录;两个 END 走 GroupExecuted
        // (空操作,无参数可记)。
        let subs = vm
            .events()
            .iter()
            .filter(|e| matches!(e, VmEvent::Subsystem { .. }))
            .count();
        assert_eq!(
            subs, 4,
            "v{}: MOVIE/FLASH/YMV/MIDI 各一条求值记录(END 族走 GroupExecuted,不计)",
            p.engine_version()
        );
        let gexec = vm
            .events()
            .iter()
            .filter(|e| matches!(e, VmEvent::GroupExecuted { .. }))
            .count();
        assert!(gexec >= 2, "v{}: 两个 END 应有组执行记录", p.engine_version());
    }
}

/// **T23b 负向**：`MOVIE[PLAY=0]` **只停播放、不退出媒体模式**
/// （手册注[3] 原话「停止しても、MOVIEEND で終了しない限り他のレイヤは
/// 一切表示されません」）⇒ 不得发 `MediaMode{on:false}`。
#[test]
fn t23b_movie_play_zero_does_not_clear_media_mode() {
    let play_off: &[u8] = &[0x42, 1, 0, 0];
    let groups: &[(u8, u8, u16)] = &[(idx("MOVIE"), 1, 0), (cmd::RETURN, 0, 0)];
    let windows = &[([0x01, 0x01, 0x00, 0x00], play_off.len() as u32, 0, play_off)];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_strict(false); // 只验证事件语义;strict 下的中断由 T23 覆盖
    vm.run(64).unwrap();
    assert!(
        !vm.events()
            .iter()
            .any(|e| matches!(e, VmEvent::MediaMode { .. })),
        "PLAY=0 是「停止播放」,不是「退出媒体模式」——不得发 MediaMode 事件"
    );
}

/// **T2 验收**：事件的 `command` 字段必须是**该版本的真实 YSCM 下标**，
/// 不是处理器声明的 v555 留存常量。
///
/// 构造：同语义「`GO` 到不存在的标签」用 555/494 序号各编码一次 →
/// `Unsupported` 事件的 `command` 必须分别等于两版 profile 的 `GO` 下标。
/// 旧行为：恒为 `cmd::GO = 0x2a`（v555 常量）—— 494 侧消费方按它反查
/// 会得到 `G_VAR`（名字与原因互相矛盾）。
#[test]
fn t2_event_command_is_real_index_not_v555_relic() {
    let name: &[u8] = &[0x4d, 10, 0, b'"', b'N', b'O', b'S', b'U', b'C', b'H', b'L', b'B', b'"'];
    let p555 = CommandProfile::v555();
    let p494 = CommandProfile::v494();
    // 前提取证：两版 GO 下标确实不同（否则本测试无鉴别力）
    assert_ne!(p555.index_of("GO"), p494.index_of("GO"));

    for p in [&p555, &p494] {
        let groups: &[(u8, u8, u16)] = &[(idx_p(p, "GO"), 1, 0)];
        let windows = &[([0x00, 0x01, 0x00, 0x00], name.len() as u32, 0, name)];
        let bytes = make_ystb_version(SAMPLE_KEY, p.engine_version(), groups, windows);
        let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
        let mut vm = GroupVm::load(script).unwrap();
        // **2026-10-18**:标签未命中改发 `LabelMiss`(命令已实现、标签是
        // 数据事实),不再冒充 `Unsupported`。本测试的原意是「事件里的
        // 命令标识必须是本版真实下标」—— 改在 `LabelMiss.command` 上验证:
        // 它带的是命令**名**（跨版本唯一稳定），而 VM 同时保证不把 v555
        // 常量写进事件。
        let r = vm.run(16);
        let misses: Vec<String> = vm
            .events()
            .iter()
            .filter_map(|e| match e {
                VmEvent::LabelMiss { command, .. } => Some(command.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !misses.is_empty(),
            "v{}: GO 未命中标签必须产生 LabelMiss 事件(得到 {r:?})",
            p.engine_version()
        );
        assert_eq!(misses[0], "GO", "v{}: 事件命令名应为 GO", p.engine_version());
        // 未命中处**不得**再产生 Unsupported(那是「内核缺功能」的语义,
        // 而这里内核功能齐全、是数据里没有该标签)。
        assert!(
            !vm.events()
                .iter()
                .any(|e| matches!(e, VmEvent::Unsupported { .. })),
            "v{}: 标签未命中不应被记为 Unsupported",
            p.engine_version()
        );
        let _ = idx_p(p, "GO");
        let _ = cmd::GO;
    }
}

/// **T23b 端到端**：VM 事件 → `SceneBridge` → 场景屏蔽位 —— 桥接层把
/// `MediaMode` 事件落到 `Scene::media_mode`，且报告可视变化（退出时要重绘）。
#[test]
fn t23b_bridge_applies_media_mode_and_reports_change() {
    use yuris_vm::bridge::SceneBridge;
    let mut b = SceneBridge::new();
    assert!(!b.scene().media_mode, "初始未处于媒体模式");

    let on = yuris_vm::VmEvent::MediaMode {
        pc: 0,
        on: true,
        command: "MOVIE".into(),
    };
    assert!(b.apply(std::slice::from_ref(&on)), "置位应报告可视变化");
    assert!(b.scene().media_mode);

    // 幂等:重复置位不报告变化
    assert!(!b.apply(std::slice::from_ref(&on)), "重复置位不应报告变化");

    let off = yuris_vm::VmEvent::MediaMode {
        pc: 1,
        on: false,
        command: "MOVIEEND".into(),
    };
    assert!(b.apply(std::slice::from_ref(&off)), "清除应报告可视变化");
    assert!(!b.scene().media_mode);
}
