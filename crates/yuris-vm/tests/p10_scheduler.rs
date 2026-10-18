//! P10 调度不变量测试(成果 161 → **成果 168 按官方调度重述**)。
//!
//! 引擎真值(成果 164,REA Confirmed):
//! 1. **单一扁平任务链**:主线只是链中一员,链序 = 创建序(主线首);
//! 2. 每个逻辑帧每任务最多一步,到期判定 = FRAME 递减 **且** TIME 期限;
//! 3. `TASK` 命令置续帧标志 ⇒ 一次上屏内连跑 K 个逻辑帧(链间不渲染);
//! 4. 命令泵无条数预算(跑到处理器让出);
//! 5. 同输入同泵序 ⇒ 事件流(含域标)逐字节确定。
//!
//! 与旧版(成果 161 的 `pump_frame` 双轨 API)的差异:主线不再是「域」而是
//! 链中节点,主线 WAIT 计数/期限由调度器统一递减 —— 因此这里的断言全部
//! 改用 `run_frame_chain` + 链报告。

use std::collections::HashMap;

use yuris_format::ystb::YstbFile;
use yuris_vm::{GroupVm, VmSuspend};

mod common;
use common::{make_ystb, DUMMY, SAMPLE_KEY};

fn idx(name: &str) -> u8 {
    u8::try_from(
        yuris_core::CommandProfile::v555()
            .index_of(name)
            .unwrap_or_else(|| panic!("{name} 不在 v555 表内")),
    )
    .unwrap()
}

/// 整数字面量窗程序(`42 01 00 N`)。
fn int_lit(n: u8) -> Vec<u8> {
    vec![0x42, 1, 0, n]
}

/// 字符串字面量窗程序(`4d len16 "text"`,len 含引号;求值后去引号)。
fn str_lit(s: &str) -> Vec<u8> {
    let quoted = format!("\"{s}\"");
    let mut v = vec![0x4d, quoted.len() as u8, 0];
    v.extend_from_slice(quoted.as_bytes());
    v
}

/// WAIT 窗 tag:slot 0 = FRAME,slot 1 = TIME(`cmd_wait` 按 tag 低字节归类)。
fn wait_tag(slot: u8) -> [u8; 4] {
    [slot, 0x00, 0x01, 0x00]
}

/// 构造相位测试脚本(单脚本):
/// g0 TASK(ID=T1, #=body, E=1) → g1 主线 WAIT → g2 主线 END
/// g3 任务体 WAIT → g4 任务体 END
fn two_domain_script(main_wait_slot: u8, main_ms: u8, task_wait_slot: u8, task_ms: u8) -> Vec<u8> {
    let groups: &[(u8, u8, u16)] = &[
        (idx("TASK"), 3, 0),   // g0
        (idx("WAIT"), 2, 0),   // g1 主线(FRAME + TIME 双槽)
        (idx("END"), 0, 0),    // g2
        (idx("WAIT"), 2, 0),   // g3 任务体(双槽)
        (idx("END"), 0, 0),    // g4
    ];
    let w_id = str_lit("T1");
    let w_entry = str_lit("body");
    let w_e = int_lit(1);
    let w_main_wait = int_lit(main_ms);
    let w_task_wait = int_lit(task_ms);
    // WAIT 双槽:FRAME(slot 0)与 TIME(slot 1);只给目标槽时另一槽留空窗
    // (len=0 → 求值跳过 → 该条件不参与)。
    let empty: &[u8] = &[];
    let main_frame = if main_wait_slot == 0 { w_main_wait.as_slice() } else { empty };
    let main_time = if main_wait_slot == 1 { w_main_wait.as_slice() } else { empty };
    let task_frame = if task_wait_slot == 0 { w_task_wait.as_slice() } else { empty };
    let task_time = if task_wait_slot == 1 { w_task_wait.as_slice() } else { empty };
    let windows = &[
        ([0x00, 0x03, 0x00, 0x00], w_id.len() as u32, 0, w_id.as_slice()),
        ([0x01, 0x01, 0x00, 0x00], w_e.len() as u32, 0, w_e.as_slice()),
        ([34, 0x03, 0x00, 0x00], w_entry.len() as u32, 0, w_entry.as_slice()),
        (wait_tag(0), main_frame.len() as u32, 0, main_frame),
        (wait_tag(1), main_time.len() as u32, 0, main_time),
        (wait_tag(0), task_frame.len() as u32, 0, task_frame),
        (wait_tag(1), task_time.len() as u32, 0, task_time),
    ];
    make_ystb(SAMPLE_KEY, groups, windows)
}

fn labels_body_at(pc: u32) -> HashMap<Vec<u8>, (u32, u16)> {
    HashMap::from([(b"body".to_vec(), (pc, 0))])
}

/// 不变量 1(官方 FRAME/TIME 双条件):任务 `WAIT TIME` 未到期不醒;到期才推进。
///
/// TIME 口径 = **链起点 u32 绝对毫秒**(官方 `timeGetTime` 比较)。
#[test]
fn task_time_wait_honors_deadline() {
    let script = YstbFile::from_bytes(
        &two_domain_script(1, 30, 1, 20), // 主线 TIME 30ms;任务 TIME 20ms
        SAMPLE_KEY,
    )
    .unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_labels(labels_body_at(3));

    // 链 1(t=0):主线 TASK(续帧) → WAIT TIME=30 让出;
    // 续帧让新任务在**第二个逻辑帧**首步 → WAIT TIME=20 让出。
    let r1 = vm.run_frame_chain(0);
    assert_eq!(r1.logical_frames, 2, "TASK 开火 ⇒ 链至少 2 个逻辑帧");
    assert!(r1.tasks_stepped >= 2, "主线 + 任务各一步");
    assert!(vm.task_exists(b"T1"));

    // 链 2(t=10):两域都未到期(30ms/20ms 期限)。
    let r2 = vm.run_frame_chain(10);
    assert_eq!(r2.tasks_stepped, 0, "TIME 未到期 ⇒ 本帧无任务步进");

    // 链 3(t=25):任务到期并 END;主线仍休息。
    let r3 = vm.run_frame_chain(25);
    assert!(r3.tasks_stepped >= 1);
    assert!(vm.task_finished_count() >= 1, "任务体 END = finished");

    // 链 4(t=40):主线到期,END 收尾。
    let r4 = vm.run_frame_chain(40);
    assert!(r4.main_finished, "主线 END 应标记 main_finished");
}

/// 不变量 2(主线休息不冻结任务):主线 TIME 休息期间,任务**每帧照常推进**。
///
/// 官方是单链:主线休息 = 跳过主线节点,不是跳过整帧。
#[test]
fn main_time_wait_does_not_freeze_tasks() {
    // 任务体 = 自循环:WAIT FRAME=1 → GO body(每帧恰一步,永不结束)。
    let groups: &[(u8, u8, u16)] = &[
        (idx("TASK"), 3, 0), // g0
        (idx("WAIT"), 1, 0), // g1 主线 TIME
        (idx("END"), 0, 0),  // g2
        (idx("WAIT"), 1, 0), // g3 任务体
        (idx("GO"), 1, 0),   // g4 任务体自循环
    ];
    let w_id = str_lit("T1");
    let w_entry = str_lit("body");
    let w_e = int_lit(1);
    let w_main = int_lit(50);
    let w_task = int_lit(1);
    let w_go = str_lit("body");
    let windows = &[
        ([0x00, 0x03, 0x00, 0x00], w_id.len() as u32, 0, w_id.as_slice()),
        ([0x01, 0x01, 0x00, 0x00], w_e.len() as u32, 0, w_e.as_slice()),
        ([34, 0x03, 0x00, 0x00], w_entry.len() as u32, 0, w_entry.as_slice()),
        (wait_tag(1), w_main.len() as u32, 0, w_main.as_slice()),
        (wait_tag(0), w_task.len() as u32, 0, w_task.as_slice()),
        (DUMMY, w_go.len() as u32, 0, w_go.as_slice()),
    ];
    let bytes = make_ystb(SAMPLE_KEY, groups, windows);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_labels(labels_body_at(3));

    // 5 个上屏帧全部落在主线 TIME(50ms)窗口内:任务须持续推进。
    let mut task_steps = 0u64;
    for i in 0..5u32 {
        let r = vm.run_frame_chain(i);
        assert!(!r.main_finished, "第 {i} 链主线应仍在 TIME 休息");
        task_steps += r.tasks_stepped;
    }
    assert!(
        task_steps >= 5,
        "主线休息期任务须每帧推进(实测 {task_steps} 步)"
    );
    assert!(vm.task_exists(b"T1"));
}

/// 不变量 3(基帧 RETURN = 挂起等重入):恰休一个逻辑帧,下一拍从同 pc 复跑。
#[test]
fn main_bare_yield_rests_one_frame() {
    let groups: &[(u8, u8, u16)] = &[(idx("RETURN"), 0, 0)];
    let bytes = make_ystb(SAMPLE_KEY, groups, &[]);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    let mut vm = GroupVm::load(script).unwrap();

    let r = vm.run_frame_chain(0);
    assert_eq!(r.logical_frames, 1, "裸让出不触发续帧");
    assert!(
        vm.main_wait_pending(),
        "基帧 RETURN = 恰休一帧(成果 132/161 D1)"
    );

    // 下一链(t=1):苏醒,同一 RETURN 再执行一次(Wait(None) 超集,pc 不动)。
    let r2 = vm.run_frame_chain(1);
    assert!(r2.tasks_stepped >= 1, "休息 1 帧后主线重入");
    assert!(vm.main_wait_pending(), "重入后仍是让出态");
    assert!(!r2.main_finished);
}

/// 不变量 4:事件域标(主线 0,任务 1)与**链序**(主线先、任务按创建序)。
#[test]
fn event_domains_mark_main_and_tasks() {
    let script = YstbFile::from_bytes(
        &two_domain_script(0, 1, 0, 1), // 双方都 FRAME=1:每帧各让一次
        SAMPLE_KEY,
    )
    .unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_labels(labels_body_at(3));

    vm.run_frame_chain(0);
    let domains = vm.event_domains().to_vec();
    assert_eq!(domains.len(), vm.events().len(), "域标与事件一一对应");
    assert!(
        domains.contains(&1u8),
        "任务域事件必须带非零域标(实测 {domains:?})"
    );
    let first_task = domains.iter().position(|&d| d != 0).unwrap();
    assert!(
        domains[..first_task].iter().all(|&d| d == 0),
        "主线在链首 ⇒ 首个逻辑帧内主线事件先于任务事件(实测 {domains:?})"
    );
}

/// 不变量 5:同输入同泵序 ⇒ 事件流(含域标)逐字节确定。
#[test]
fn pump_is_deterministic() {
    let run = || {
        let script = YstbFile::from_bytes(&two_domain_script(0, 1, 0, 1), SAMPLE_KEY).unwrap();
        let mut vm = GroupVm::load(script).unwrap();
        vm.set_labels(labels_body_at(3));
        let mut frames: Vec<(Vec<yuris_vm::VmEvent>, Vec<u8>)> = Vec::new();
        for i in 0..4u32 {
            vm.run_frame_chain(i * 20);
            frames.push((vm.events().to_vec(), vm.event_domains().to_vec()));
            vm.trim_consumed_events(vm.events().len());
        }
        frames
    };
    let a = run();
    let b = run();
    assert_eq!(a, b, "同输入同泵序必须逐字节一致(确定性验收的地基)");
    // 至少一帧出现了任务事件,否则本测试没断言到域序。
    assert!(a.iter().any(|(evs, doms)| doms.iter().any(|&d| d == 1) && !evs.is_empty()));
}

/// 不变量 6(官方 `0x450ae9`):TASK 续帧让**同一上屏内**连跑多个逻辑帧,
/// 且新任务在续帧里拿到第一步(入口链长快照语义)。
#[test]
fn task_extends_chain_and_new_task_steps_within_it() {
    let script = YstbFile::from_bytes(
        &two_domain_script(0, 1, 0, 1),
        SAMPLE_KEY,
    )
    .unwrap();
    let mut vm = GroupVm::load(script).unwrap();
    vm.set_labels(labels_body_at(3));
    let r = vm.run_frame_chain(0);
    assert!(
        r.logical_frames >= 2,
        "TASK 开火 ⇒ 链 ≥ 2 逻辑帧(实测 {})",
        r.logical_frames
    );
    // 任务在链内已经步进(不是等下一次上屏)。
    assert!(r.tasks_stepped >= 2, "主线 + 任务同链内各一步");
    assert!(!matches!(r.errors.first(), Some(_)));
    let _ = VmSuspend::None;
}
