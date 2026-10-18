//! P11 桥接测试:**新任务调度器(`task.rs`,未接线)能否承接真实脚本**。
//!
//! 做法 = 官方同构的最小桥:官方命令泵是「CALL 一条处理器,返回非零即让出」
//! (FUN_00404388);本测试用公开 API `GroupVm::run(1)`(= 最多执行一组)充当
//! 「一条命令」,把 `VmSuspend` 映射为 `StepOutcome`:
//! - `None`(预算用尽,已执行一组)→ `Continue`(处理器返回 0)
//! - `Wait{..}` → 先写 `TaskNode::wait_frames`(= 官方 WAIT 处理器写任务
//!   +0x18 的副作用)再返回 `Yield`(处理器返回非零)
//! - `Complete` → `Finish`;`Error` → `Err`
//!
//! 脚本 = 手写合成 YSTB(真字节码、真分派、真事件),不经 mock:
//! - sA:`INT @100=1` → `WAIT FRAME=2` → `INT @101=2` → `END`
//! - sB:`INT @200=7` → `WAIT FRAME=1` → `INT @201=8` → `END`
//!
//! 预期帧序(调度器 FRAME 递减语义:先减后判,成果 161 勘误):
//! 链1 a/b 各执行首组 + WAIT 让出(wait=2/1);链2 a 休(2→1)、
//! b 醒(1→0)跑到 END;链3 a 醒跑到 END。
//!
//! TASK 续帧不走本文件:现行 TASK 命令落在 `GroupVm` 自己的任务系统里
//! (接线时改发 `Effect`);Effect 通道由测试 2 的 FinishTask 演示 —— 与
//! TASK/TASKEND 走同一条副作用通道。
//!
//! 本文件不改现行任何路径;`GroupVm::run()` 语义零改动。

use yuris_format::ystb::YstbFile;
use yuris_vm::task::{Effect, StepOutcome, TaskExecutor, TaskNode, TaskScheduler, TaskSeed};
use yuris_vm::{cmd, GroupVm, VmEvent, VmSuspend};
use yuris_value::{VarRef, VarSpace, Value};

mod common;
use common::{make_ystb, SAMPLE_KEY};

fn var(space: u8, id: u16) -> VarRef {
    VarRef { space: VarSpace::from_prefix(space), id }
}

/// `INT @id = n`(声明事件,可作执行标记):w0 = var,w1 = pushint8 n。
fn int_group(id: u16, n: i8) -> (
    (u8, u8, u16),
    ([u8; 4], u32, u32, Vec<u8>),
    ([u8; 4], u32, u32, Vec<u8>),
) {
    let w0 = vec![0x48, 3, 0, 0x40, (id & 0xff) as u8, (id >> 8) as u8];
    let w1 = vec![0x42, 1, 0, n as u8];
    (
        (0x32, 2, 0),
        ([0x00, 0x01, 0x00, 0x00], w0.len() as u32, 0, w0),
        ([0x00, 0x01, 0x00, 0x00], w1.len() as u32, 0, w1),
    )
}

/// `WAIT FRAME=n`(tag B0=0 = FRAME 型;runtime_loop.rs 同款窗口)。
fn wait_frame_group(n: i8) -> ((u8, u8, u16), ([u8; 4], u32, u32, Vec<u8>)) {
    let w = vec![0x42, 1, 0, n as u8];
    ((cmd::WAIT, 1, 0), ([0x00, 0x00, 0x01, 0x00], w.len() as u32, 0, w))
}

fn end_group() -> (u8, u8, u16) {
    (cmd::END, 0, 0)
}

/// 展开的"淡入淡出"动画脚本:每步 = 一次 A 步进标记(INT 声明)+ WAIT 定拍,
/// 步进间恰隔 `wait_n` 个逻辑帧 —— 官方 logo 淡变的脚本形态(A += δ;WAIT)。
fn fade_script(steps: &[(u16, i8)], wait_n: i8) -> (Vec<(u8, u8, u16)>, Vec<([u8; 4], u32, u32, Vec<u8>)>) {
    let mut groups = Vec::new();
    let mut windows = Vec::new();
    for (id, v) in steps {
        let (g, w0, w1) = int_group(*id, *v);
        groups.push(g);
        windows.push(w0);
        windows.push(w1);
        let (gw, ww) = wait_frame_group(wait_n);
        groups.push(gw);
        windows.push(ww);
    }
    groups.push(end_group());
    (groups, windows)
}

fn load_script(
    groups: Vec<(u8, u8, u16)>,
    windows: Vec<([u8; 4], u32, u32, Vec<u8>)>,
) -> GroupVm {
    let window_refs: Vec<([u8; 4], u32, u32, &[u8])> =
        windows.iter().map(|(t, l, o, w)| (*t, *l, *o, w.as_slice())).collect();
    let bytes = make_ystb(SAMPLE_KEY, &groups, &window_refs);
    let script = YstbFile::from_bytes(&bytes, SAMPLE_KEY).unwrap();
    GroupVm::load(script).unwrap()
}

/// 桥:任务节点 → 独立 `GroupVm`(一节点一脚本);`run(1)` = 官方「CALL 一条
/// 命令」。`finish_effects` = 任务走完时产生的调度副作用;`first_call_effects`
/// = 任务首条命令时产生的副作用(模拟 TASK 开火)—— 三者都演示官方
/// 「处理器直写全局」走 Effect 通道。
struct BridgeExec {
    vms: Vec<(Vec<u8>, GroupVm)>,
    finish_effects: Vec<(Vec<u8>, Effect)>,
    first_call_effects: Vec<(Vec<u8>, Effect)>,
    calls: std::collections::HashMap<Vec<u8>, usize>,
    pending: Vec<Effect>,
}

impl BridgeExec {
    fn build(
        finish_effects: Vec<(Vec<u8>, Effect)>,
        specs: &[(&str, Vec<(u8, u8, u16)>, Vec<([u8; 4], u32, u32, Vec<u8>)>)],
    ) -> (TaskScheduler, Self) {
        let mut s = TaskScheduler::with_pump_cap(None);
        let mut vms = Vec::new();
        for (id, groups, windows) in specs {
            s.spawn(TaskSeed { id: id.as_bytes().to_vec(), script_id: 0, pc: 0, enabled: true, is_main: false });
            vms.push((id.as_bytes().to_vec(), load_script(groups.clone(), windows.clone())));
        }
        (
            s,
            BridgeExec {
                vms,
                finish_effects,
                first_call_effects: Vec::new(),
                calls: std::collections::HashMap::new(),
                pending: Vec::new(),
            },
        )
    }

    fn vm(&self, id: &str) -> &GroupVm {
        &self
            .vms
            .iter()
            .find(|(k, _)| k.as_slice() == id.as_bytes())
            .unwrap()
            .1
    }
}

impl TaskExecutor for BridgeExec {
    fn step_command(&mut self, task: &mut TaskNode) -> Result<StepOutcome, String> {
        let n = self.calls.entry(task.id.clone()).or_insert(0);
        *n += 1;
        let first = *n == 1;
        if first {
            if let Some(i) =
                self.first_call_effects.iter().position(|(id, _)| id == &task.id)
            {
                let (_, e) = self.first_call_effects.remove(i);
                self.pending.push(e);
            }
        }
        let vm = &mut self
            .vms
            .iter_mut()
            .find(|(id, _)| id == &task.id)
            .ok_or_else(|| format!("no vm for task {:?}", String::from_utf8_lossy(&task.id)))?
            .1;
        match vm.run(1) {
            Ok(VmSuspend::None) => Ok(StepOutcome::Continue),
            Ok(VmSuspend::Wait { counter, time_ms }) => {
                // 官方 WAIT 处理器:写任务 +0x18 = FRAME(+0x1c =
                // timeGetTime()+ms)后让出。TIME 型接线层需墙钟换算,
                // 桥接测试脚本不含 TIME。
                let _ = time_ms;
                task.wait_frames = counter.unwrap_or(1) as u32;
                Ok(StepOutcome::Yield)
            }
            Ok(VmSuspend::Complete) => {
                if let Some(i) =
                    self.finish_effects.iter().position(|(id, _)| id == &task.id)
                {
                    let (_, e) = self.finish_effects.remove(i);
                    self.pending.push(e);
                }
                Ok(StepOutcome::Finish)
            }
            Ok(VmSuspend::Error(m)) => Err(m),
            Err(e) => Err(e.to_string()),
        }
    }

    fn drain_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.pending)
    }
}

/// 手写脚本并发:链1 双双让出,链2 b 醒、链3 a 醒;变量与事件逐项可查。
#[test]
fn handwritten_scripts_carry_under_new_scheduler() {
    let (ga, wa0, wa1) = int_group(100, 1);
    let (gw, ww) = wait_frame_group(2);
    let (ga2, wa2, wa3) = int_group(101, 2);
    let (gb, wb0, wb1) = int_group(200, 7);
    let (gw1, ww1) = wait_frame_group(1);
    let (gb2, wb2, wb3) = int_group(201, 8);

    let (mut s, exec) = BridgeExec::build(
        Vec::new(),
        &[
            ("a", vec![ga, gw, ga2, end_group()], vec![wa0, wa1, ww, wa2, wa3]),
            ("b", vec![gb, gw1, gb2, end_group()], vec![wb0, wb1, ww1, wb2, wb3]),
        ],
    );
    let mut exec = exec;

    // —— 链1:a、b 各执行 INT 后 WAIT 让出 ——
    let r1 = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r1.logical_frames, 1, "无 TASK ⇒ 一链一逻辑帧");
    assert_eq!(
        exec.vm("a").store().get(&var(0x40, 100)).ok(),
        Some(&Value::Int(1)),
        "sA g0 已执行"
    );
    assert_eq!(exec.vm("b").store().get(&var(0x40, 200)).ok(), Some(&Value::Int(7)));
    assert_eq!(exec.vm("a").store().get(&var(0x40, 101)).ok(), None, "sA g2 未到");
    assert_eq!(
        s.find_by_id(b"a").unwrap().wait_frames, 2,
        "桥把 WAIT FRAME=2 写回节点(官方 WAIT 处理器写 +0x18)"
    );
    assert_eq!(s.find_by_id(b"b").unwrap().wait_frames, 1);

    // —— 链2:a 休(2→1);b 醒(1→0)跑到 END ——
    let r2 = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r2.logical_frames, 1);
    assert_eq!(exec.vm("a").store().get(&var(0x40, 101)).ok(), None, "a 仍在休");
    assert_eq!(exec.vm("b").store().get(&var(0x40, 201)).ok(), Some(&Value::Int(8)));
    assert!(s.find_by_id(b"b").unwrap().finished, "sB END ⇒ Finish");

    // —— 链3:a 醒(1→0)跑到 END ——
    let r3 = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r3.logical_frames, 1);
    assert_eq!(exec.vm("a").store().get(&var(0x40, 101)).ok(), Some(&Value::Int(2)));
    assert!(s.find_by_id(b"a").unwrap().finished, "sA END ⇒ Finish");

    // 事件(真分派产物):两脚本各发出 INT 声明事件。
    for id in ["a", "b"] {
        assert!(
            exec.vm(id).events()
                .iter()
                .any(|e| matches!(e, VmEvent::Declaration { command: 0x32, .. })),
            "任务 {id} 应有 INT 声明事件"
        );
    }
    // 命令泵总量确定性:链1 a2+b2、链2 b2、链3 a2 = 8。
    assert_eq!(
        r1.commands_pumped + r2.commands_pumped + r3.commands_pumped,
        8
    );
}

/// 走完的任务经 Effect 通道杀同胞任务(官方 TASKEND 通道;同帧生效)。
#[test]
fn script_finish_can_kill_sibling_via_effect() {
    let (ga, wa0, wa1) = int_group(100, 1);
    let (gb, wb0, wb1) = int_group(200, 7);
    let (gw1, ww1) = wait_frame_group(1);
    let (gb2, wb2, wb3) = int_group(201, 8);

    let (mut s, exec) = BridgeExec::build(
        vec![(b"a".to_vec(), Effect::FinishTask(b"b".to_vec()))],
        &[
            ("a", vec![ga, end_group()], vec![wa0, wa1]),
            ("b", vec![gb, gw1, gb2, end_group()], vec![wb0, wb1, ww1, wb2, wb3]),
        ],
    );
    let mut exec = exec;

    let r = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r.logical_frames, 1, "FinishTask 不触发续帧");
    assert_eq!(exec.vm("a").store().get(&var(0x40, 100)).ok(), Some(&Value::Int(1)));
    assert!(s.find_by_id(b"a").unwrap().finished);
    assert!(
        s.find_by_id(b"b").unwrap().finished,
        "a 走完 ⇒ FinishTask(b) 同帧生效,链序在后的 b 被跳过"
    );
    assert_eq!(
        exec.vm("b").store().get(&var(0x40, 200)).ok(),
        None,
        "b 一条命令都没执行到(无 INT 事件/变量)"
    );
    assert!(exec.vm("b").events().is_empty());
}

/// 淡变步进 = 每逻辑帧恰好一步(WAIT FRAME=1 定拍):3 步动画在第 1/2/3 链
/// 各走一步,第 4 链 END 收尾 —— 步进帧序与总时长(K 逻辑帧)由调度器锁定。
#[test]
fn fade_steps_advance_once_per_logical_frame() {
    let mut s = TaskScheduler::with_pump_cap(None);
    s.spawn(TaskSeed { id: b"f".to_vec(), script_id: 0, pc: 0, enabled: true, is_main: false });
    let (groups, windows) = fade_script(&[(300, 1), (301, 2), (302, 3)], 1);
    let mut exec = BridgeExec {
        vms: vec![(b"f".to_vec(), load_script(groups, windows))],
        finish_effects: Vec::new(),
        first_call_effects: Vec::new(),
        calls: std::collections::HashMap::new(),
        pending: Vec::new(),
    };

    // 步 i 恰落在第 i 链;此前一帧不出现(时长口径 = 逻辑帧数)。
    let steps: [(u16, i64); 3] = [(300, 1), (301, 2), (302, 3)];
    for (i, (id, v)) in steps.iter().enumerate() {
        let r = s.run_frame_chain(&mut exec, 0);
        assert_eq!(r.logical_frames, 1, "无 TASK ⇒ 一链一逻辑帧");
        assert_eq!(
            exec.vm("f").store().get(&var(0x40, *id)).ok(),
            Some(&Value::Int(*v)),
            "淡变第 {} 步应恰在第 {} 逻辑帧执行",
            i + 1,
            i + 1
        );
        assert!(!s.find_by_id(b"f").unwrap().finished, "未到 END");
    }
    let r4 = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r4.logical_frames, 1);
    assert!(s.find_by_id(b"f").unwrap().finished, "3 步 + 收尾 = 4 逻辑帧");
}

/// 淡变节奏随 WAIT FRAME 缩放(FRAME=2 = 两逻辑帧一步):步 2 恰在第 3 链
/// (步 1 后第 2 帧)出现,总时长 = 5 逻辑帧。
#[test]
fn fade_pacing_scales_with_wait_frame() {
    let mut s = TaskScheduler::with_pump_cap(None);
    s.spawn(TaskSeed { id: b"f".to_vec(), script_id: 0, pc: 0, enabled: true, is_main: false });
    let (groups, windows) = fade_script(&[(400, 1), (401, 2)], 2);
    let mut exec = BridgeExec {
        vms: vec![(b"f".to_vec(), load_script(groups, windows))],
        finish_effects: Vec::new(),
        first_call_effects: Vec::new(),
        calls: std::collections::HashMap::new(),
        pending: Vec::new(),
    };

    for chain in 1..=5 {
        let r = s.run_frame_chain(&mut exec, 0);
        assert_eq!(r.logical_frames, 1);
        match chain {
            1 => assert_eq!(exec.vm("f").store().get(&var(0x40, 400)).ok(), Some(&Value::Int(1))),
            2 => assert_eq!(exec.vm("f").store().get(&var(0x40, 401)).ok(), None, "FRAME=2 ⇒ 本帧休"),
            3 => assert_eq!(exec.vm("f").store().get(&var(0x40, 401)).ok(), Some(&Value::Int(2))),
            _ => {}
        }
    }
    assert!(s.find_by_id(b"f").unwrap().finished, "2 步 + FRAME=2 ⇒ 5 逻辑帧");
}

/// TASK 密集段的官方压缩语义:续帧逻辑帧同样推进淡变(WAIT 递减不分
/// 「上屏帧/续帧」)。一次 TASK 开火 ⇒ 2 步淡变被压进**同一次上屏**
/// (2 逻辑帧),这正是官方 boot 转场不出现黑段的机制。
#[test]
fn fade_advances_during_task_extra_frames() {
    let mut s = TaskScheduler::with_pump_cap(None);
    s.spawn(TaskSeed { id: b"f".to_vec(), script_id: 0, pc: 0, enabled: true, is_main: false });
    s.spawn(TaskSeed { id: b"t".to_vec(), script_id: 0, pc: 0, enabled: true, is_main: false });
    let (groups, windows) = fade_script(&[(500, 1), (501, 2)], 1);
    // t 的脚本:一条 INT(首条命令开火续帧)后走完。
    let (gt, wt0, wt1) = int_group(600, 9);
    let mut exec = BridgeExec {
        vms: vec![
            (b"f".to_vec(), load_script(groups, windows)),
            (b"t".to_vec(), load_script(vec![gt, end_group()], vec![wt0, wt1])),
        ],
        finish_effects: Vec::new(),
        pending: Vec::new(),
        first_call_effects: vec![(b"t".to_vec(), Effect::RequestExtraFrame)],
        calls: std::collections::HashMap::new(),
    };

    let r = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r.logical_frames, 2, "TASK 开火一次 ⇒ 链 = 2 逻辑帧");
    // 两步淡变都在这一次上屏内完成(官方压缩语义;WAIT 递减发生在续帧上)。
    assert_eq!(exec.vm("f").store().get(&var(0x40, 500)).ok(), Some(&Value::Int(1)));
    assert_eq!(exec.vm("f").store().get(&var(0x40, 501)).ok(), Some(&Value::Int(2)));
    let r2 = s.run_frame_chain(&mut exec, 0);
    assert_eq!(r2.logical_frames, 1);
    assert!(s.find_by_id(b"f").unwrap().finished, "尾帧 END 收尾");
}
