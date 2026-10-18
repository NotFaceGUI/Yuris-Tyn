//! 执行驱动:主线/任务的统一命令泵(官方 `FUN_00404388` 同构)。
//!
//! 从 `lib.rs` 拆出(2026-10-17 重构)。本模块承载:
//! - [`GroupVm::step_group`]:**单组**执行(旧 `run_main` 循环体,唯一定义点);
//! - [`GroupVm::run`]:旧契约(`budget` 组 → `VmSuspend`),golden/单测直连;
//! - [`GroupVm::run_frame_chain`]:官方 vsync 链(TASK 续帧,链后一次上屏);
//! - [`TaskExecutor`] 实装:任务节点 ↔ 执行位交换(`swap_exec`)。
//!
//! ## 为什么主线也是节点
//!
//! 官方帧步进沿**单一扁平链**取件(`FUN_004046cc`),主线脚本只是链中一员。
//! 旧实现把主线放在 `self` 自身上、任务放在 `tasks: Vec<VmTask>` 里,形成
//! 「主线域恒先 + 任务域」双轨 —— 顺序与续帧语义都与官方不同。
//! 现在链序由 [`TaskScheduler`] 决定,`self` 只当「当前执行位的寄存器」:
//! 主线节点直接跑在 `self` 上,任务节点用 [`GroupVm::swap_exec`] 换入换出。
//!
//! ## 旧 `run()` 为什么保留
//!
//! `run(budget)` 是 golden 快照与既有单测的唯一入口,且 `p11_task_bridge`
//! 拿它当「一条命令」的适配器(`run(1)`)。它**不消费续帧标志**(旧 API 没有
//! vsync 边界),只经 [`TaskScheduler::step_tasks_only`] 推进任务节点 —— 语义
//! 与官方单帧步进一致,但不改变既有调用者的契约。

use yuris_core::{Error, Result};

use crate::event::{VmState, VmSuspend};
use crate::exec::Action;
use crate::task::{ChainReport, Effect, StepOutcome, TaskExecutor, TaskNode};
use crate::{GosubFrame, GroupVm, SavedExec};

/// 组级 trace 选择器快照(`YURIS_TRACE_GROUPS`;None = 关)。
///
/// **热路径**:每组调用一次,`env::var_os` 每组一次在 Windows 上是可观测
/// 常数开销(成果 167);环境进程内不变 ⇒ 进程级快照,语义不变。
/// `YURIS_TRACE_HOT` 门控(链内 (脚本,pc) 热点直方图;定位异常泵)。
///
/// 只在**链结束时**打印 top 12 —— 不逐组输出,开销仅限于一次 HashMap 自增。
fn trace_hot_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| std::env::var_os("YURIS_TRACE_HOT").is_some())
}

/// `YURIS_TRACE_CHAIN` 门控(单链组数>5k 时打一行;定位慢帧来源域)。
fn trace_chain_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| std::env::var_os("YURIS_TRACE_CHAIN").is_some())
}

fn trace_groups_snapshot() -> Option<String> {
    static GATE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| {
        std::env::var_os("YURIS_TRACE_GROUPS").map(|v| v.to_string_lossy().into_owned())
    })
    .clone()
}

impl GroupVm {
    /// **交换当前执行位**(上下文切换的核心原语)。
    ///
    /// 把 `self` 的执行位与传入的 `e` **整体互换**,返回 `self` 原来的那份。
    /// 只用 `swap`、不做「取空 → 装回」:因为 [`SavedExec`] 里的
    /// `ScriptCtx` 含有 `YstbFile`(字段私有、无空构造),造不出占位值。
    pub(crate) fn swap_exec(&mut self, mut e: SavedExec) -> SavedExec {
        std::mem::swap(&mut self.ctx, &mut e.ctx);
        std::mem::swap(&mut self.pc, &mut e.pc);
        std::mem::swap(&mut self.frames, &mut e.frames);
        std::mem::swap(&mut self.if_nest, &mut e.if_nest);
        std::mem::swap(&mut self.loops, &mut e.loops);
        e
    }

    /// 为一个任务体建立**独立执行位**(入口标签 → 脚本号 + 组下标)。
    ///
    /// `frames` 里放**一个基帧**:任务体的 `RETURN` 落到基帧 = 该任务结束
    /// (与主线同一判据 `frames.len() <= 1`,见 `CmdKind::Return`)。
    pub(crate) fn build_task_exec(&mut self, script_id: u16, pc: usize) -> Result<SavedExec> {
        // 同脚本:直接克隆当前 ctx(不必绑定 host —— 单脚本夹具也能调度)。
        // 跨脚本:走 host(与主线 `switch_script` 同一加载/缓存路径)。
        let ctx = if script_id == self.ctx.script_id {
            self.ctx.clone()
        } else {
            let Some(host) = &mut self.host else {
                return Err(Error::format(format!(
                    "TASK 入口落在 script {script_id} 但未绑定 ScriptHost"
                )));
            };
            host.load(script_id)?
        };
        Ok(SavedExec {
            ctx,
            pc,
            frames: vec![GosubFrame {
                return_pc: pc,
                script_id,
                locals: yuris_value::VariableStore::new(),
                if_nest_depth: 0,
                loop_depth: 0,
            }],
            if_nest: Vec::new(),
            loops: Vec::new(),
        })
    }

    /// **执行一个组**(官方「CALL 处理器」的同构;`run_main` 循环体唯一实现)。
    ///
    /// - pc 越界 = 脚本尾 → [`VmSuspend::Complete`];
    /// - 返回 [`Action`] 由调用方执行控制流(推进/跳转/让出);
    /// - 错误现场(脚本号 + 组号 + 原因)在此快照进 `err_cause`。
    pub(crate) fn step_group(&mut self) -> Result<Action> {
        if self.pc >= self.ctx.groups.len() {
            self.state = VmState::Finished;
            return Ok(Action::Suspend(VmSuspend::Complete));
        }
        self.executed_groups += 1;
        // 诊断(`YURIS_WATCH_VAR`):把当前组登记给 yuris-value 的写入日志,
        // 使「谁写的」能指到 (脚本, pc)。未开诊断时是空操作。
        yuris_value::set_watch_site(self.ctx.script_id, self.pc);
        if trace_hot_enabled() {
            *self.hot_hist.entry((self.ctx.script_id, self.pc)).or_insert(0) += 1;
            let ct = self.ctx.groups[self.pc].command_type;
            *self.hot_cmd.entry(ct).or_insert(0) += 1;
            *self.hot_script.entry(self.ctx.script_id).or_insert(0) += 1;
        }
        let g = self.ctx.groups[self.pc];
        // 诊断(`YURIS_TRACE_GROUPS`):值可取脚本号;`1`/空 = 全打。
        if let Some(sel) = trace_groups_snapshot() {
            let only_this = sel.trim().parse::<u16>().ok().filter(|n| *n > 1);
            if only_this.is_none_or(|n| n == self.ctx.script_id) {
                yuris_log::trace_forced!(
                    "[g] s{} pc{} cmd{}",
                    self.ctx.script_id,
                    self.pc,
                    g.command_type
                );
            }
        }
        let first = self.ctx.first_slots[self.pc];
        let n = g.window_count as usize;
        self.executed_windows += n;
        // **热路径(成果 167 保持)**:窗口切片走调用级复用缓冲。
        // 用 `mem::take` 把缓冲搬出 `self`,dispatch 期间 `&mut self` 与
        // `&windows` 不冲突;容量跨组/跨帧复用,稳态零分配。
        let mut windows = std::mem::take(&mut self.windows_buf);
        windows.clear();
        windows.extend_from_slice(&self.ctx.script.slots()[first..first + n]);
        let kind = self.kind_of(g.command_type);
        let snap_script = self.ctx.script_id;
        let snap_pc = self.pc;
        self.cur_command = Some(g.command_type);
        let res = self.dispatch(kind, g, &windows);
        self.cur_command = None;
        self.windows_buf = windows;
        match res {
            Ok(action) => Ok(action),
            Err(e) => {
                if self.err_cause.is_none() {
                    self.err_cause = Some(format!("s{snap_script} pc={snap_pc}: {e}"));
                }
                Err(e)
            }
        }
    }

    /// 主线执行(单执行位,不含链调度);[`Self::run`] 与任务桥的共同实体。
    pub(crate) fn run_main(&mut self, budget: usize) -> Result<VmSuspend> {
        match self.state {
            VmState::Finished => return Ok(VmSuspend::Complete),
            VmState::Error => {
                // 挡回消息**带根因**(`err_cause`;2026-10-17 之前只有一句
                // 泛文,同文本每帧 error 级刷屏把日志淹没,一行根因翻不到)。
                let cause = match &self.err_cause {
                    Some(c) => c.clone(),
                    None => "原因未经记录(无 [vm-err] 现场?)".to_string(),
                };
                return Err(Error::format(format!(
                    "VM 处于 Error 态;根因: {cause}。骨架版不支持自动恢复"
                )));
            }
            VmState::Idle | VmState::Running => {}
        }
        self.state = VmState::Running;

        let mut used = 0usize;
        while self.pc < self.ctx.groups.len() {
            if used >= budget {
                return Ok(VmSuspend::None);
            }
            used += 1;
            match self.step_group()? {
                Action::Next => self.pc += 1,
                Action::Goto(target) => self.pc = target,
                Action::Keep => {}
                Action::Break => break,
                Action::Suspend(VmSuspend::Error(m)) => {
                    if self.err_cause.is_none() {
                        self.err_cause = Some(format!("s{} pc={}: {m}", self.ctx.script_id, self.pc));
                    }
                    return Ok(VmSuspend::Error(m));
                }
                Action::Suspend(s) => return Ok(s),
            }
        }
        self.state = VmState::Finished;
        Ok(VmSuspend::Complete)
    }

    /// 执行至多 `budget` 个组,返回挂起信息(**旧契约,golden 直连**)。
    ///
    /// - 配额耗尽 → [`VmSuspend::None`](VmSuspend::None)(状态 `Running`)
    /// - pc 走完所有组 → [`VmSuspend::Complete`](VmSuspend::Complete)(`Finished`)
    /// - Unsupported(strict)/求值错误 → [`VmSuspend::Error`](VmSuspend::Error)
    ///
    /// 主线让出后推进**一逻辑帧的任务节点**(跳过主线,见
    /// [`TaskScheduler::step_tasks_only`])—— 与旧 `step_tasks` 的可观测
    /// 行为一致(`p2_commands` 的任务步进断言)。TASK 产生的调度副作用
    /// (建任务/结束/启停)在 `run_main` 返回后统一落地。
    pub fn run(&mut self, budget: usize) -> Result<VmSuspend> {
        let r = self.run_main(budget);
        if crate::wait_trace_enabled() {
            if let Ok(VmSuspend::Wait { counter, time_ms }) = &r {
                yuris_log::trace_forced!(
                    "[wait] main s{} pc{} counter={counter:?} time_ms={time_ms:?}",
                    self.ctx.script_id,
                    self.pc
                );
            }
        }
        self.apply_pending_effects();
        if !self.task_names.is_empty() && matches!(r, Ok(VmSuspend::Wait { .. }) | Ok(VmSuspend::Complete))
        {
            self.step_tasks_legacy_once();
        }
        r
    }

    /// **官方 vsync 链**:跑 K 个逻辑帧(TASK 续帧决定 K),链后由调用方
    /// 消费事件 + 渲染一次。`now_ms` = 调用方时钟(u32 绝对毫秒,回绕语义
    /// 与 `timeGetTime` 一致;D-1 口径,见 `docs/engine/task-scheduler-rebuild.md`)。
    ///
    /// 这是 player-core `Player::tick` 的入口;`run()` 保持旧契约不动。
    pub fn run_frame_chain(&mut self, now_ms: u32) -> ChainReport {
        // 上一轮旧 `run()` 遗留的 TASK 副作用先落地,保证链的节点表最新。
        self.apply_pending_effects();
        self.chain_now_ms = now_ms;
        // 组数基线(诊断:本链泵了多少组)。
        let g0 = self.executed_groups;
        let mut sched = self
            .scheduler
            .take()
            .expect("scheduler 不在位:上一轮链未收尾?");
        // 热点现场(仅显式开诊断时分配):记录链内 (脚本,pc) 命中次数,
        // 用于定位「单逻辑帧 85 万组」这类异常泵(成果 168 挂号 1)。
        let hot = trace_hot_enabled();
        if hot {
            self.hot_hist.clear();
            self.hot_cmd.clear();
            self.hot_script.clear();
        }
        let report = sched.run_frame_chain(self, now_ms);
        self.scheduler = Some(sched);
        self.chain_now_ms = 0;
        let mut report = report;
        report.groups_pumped = (self.executed_groups - g0) as u64;
        if trace_chain_enabled() && report.groups_pumped > 5_000 {
            yuris_log::trace_forced!(
                "[chain] frames={} groups={} cmds={} tasks={}",
                report.logical_frames,
                report.groups_pumped,
                report.commands_pumped,
                report.tasks_stepped
            );
        }
        if hot && report.groups_pumped > 5_000 {
            let mut v: Vec<_> = self.hot_hist.iter().map(|(k, n)| (*k, *n)).collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            let top: Vec<String> = v
                .iter()
                .take(12)
                .map(|((sid, pc), n)| format!("s{sid}pc{pc}×{n}"))
                .collect();
            yuris_log::trace_forced!("[hot] groups={} top={}", report.groups_pumped, top.join(" "));
            // **命令类型直方图 + 脚本分布**(成果 184 的诊断面):
            // 判断「85 万组」是「脚本真的这么长」还是「某处理器该让出却未让出」
            // —— 若某类命令占绝对多数且集中在少数 pc,就是后者。
            let mut cmds: Vec<(u8, u64)> = self.hot_cmd.iter().map(|(k, v)| (*k, *v)).collect();
            cmds.sort_by(|a, b| b.1.cmp(&a.1));
            let total: u64 = cmds.iter().map(|(_, n)| *n).sum::<u64>().max(1);
            let cmd_txt: Vec<String> = cmds
                .iter()
                .take(16)
                .map(|(c, n)| {
                    let nm = self
                        .command_dispatch()
                        .name_of(*c as u16)
                        .unwrap_or("?")
                        .to_string();
                    format!("{nm}(cmd{c})×{n}({:.1}%)", *n as f64 * 100.0 / total as f64)
                })
                .collect();
            yuris_log::trace_forced!("[hot-cmd] {}", cmd_txt.join(" "));
            let mut scripts: Vec<(u16, u64)> =
                self.hot_script.iter().map(|(k, v)| (*k, *v)).collect();
            scripts.sort_by(|a, b| b.1.cmp(&a.1));
            let scr_txt: Vec<String> = scripts
                .iter()
                .take(10)
                .map(|(s, n)| format!("s{s}×{n}({:.1}%)", *n as f64 * 100.0 / total as f64))
                .collect();
            yuris_log::trace_forced!(
                "[hot-script] distinct={} {}",
                self.hot_hist.len(),
                scr_txt.join(" ")
            );
        }
        report
    }

    /// VM 时钟(u32 绝对毫秒;`timeGetTime` 等价物,见 [`GroupVm::timer_ms`])。
    pub fn now_ms_u32(&self) -> u32 {
        self.timer_ms() as u32
    }

    /// **推进一个上屏帧的逻辑时钟**(播放器每 tick 调一次,先于链)。
    ///
    /// 粒度必须 = **上屏帧**而非链内逻辑帧:一次 vsync 链可以跑 K 个逻辑帧
    /// (TASK 续帧),但墙钟上仍然只过了 1/60s —— 若按链内帧累加,
    /// `WAIT TIME`/`@1[1]` 会跑得比真实时间快 K 倍(实测:标题动画瞬间走完
    /// 又黑回去)。这是逻辑时钟的正确落点(成果 172)。
    pub fn advance_frame_clock(&mut self) {
        self.logical_frames = self.logical_frames.wrapping_add(1);
    }

    /// **按真实经过时间推进逻辑时钟**(成果 181)。
    ///
    /// 原先把「一个上屏帧 = 1/60 秒逻辑时间」写死。但当渲染只有 ~30fps
    /// (实测均值 32.6ms)时,逻辑时间只跑到真实时间的一半 ——
    /// 脚本里 `WAIT TIME=200` 会变成 400ms 真实等待,用户观感
    /// 「点击后要停一下才显示」。
    ///
    /// 现在按 **elapsed / (1000/60)** 推进对应的逻辑帧数(`steps`),
    /// 让逻辑时间跟随墙钟、同时保持**帧量化**(帧内不抖动)。
    /// 调用方负责限幅(卡顿时不追赶)。
    pub fn advance_frame_clock_by(&mut self, steps: u32) {
        self.logical_frames = self.logical_frames.wrapping_add(steps.max(1) as u64);
    }

    /// 落地队列里的调度副作用(旧 `run()` 路径;链内由调度器同拍应用)。
    fn apply_pending_effects(&mut self) {
        if self.pending_effects.is_empty() {
            return;
        }
        let effects = std::mem::take(&mut self.pending_effects);
        let mut sched = self.scheduler.take().expect("scheduler 不在位");
        for e in effects {
            match e {
                Effect::RequestExtraFrame => {
                    // 旧 API 无 vsync 链:续帧标志没有消费点,丢弃。
                }
                Effect::SpawnTask(seed) => {
                    sched.upsert(seed);
                }
                Effect::SetEnabled(id, on) => {
                    sched.set_enabled(&id, on);
                }
                Effect::FinishTask(id) => {
                    sched.finish_by_id(&id);
                }
            }
        }
        self.scheduler = Some(sched);
    }

    /// 旧 `run()` 的一次任务帧(单逻辑帧;不消费续帧标志)。
    fn step_tasks_legacy_once(&mut self) {
        let now = self.now_ms_u32();
        let mut sched = self.scheduler.take().expect("scheduler 不在位");
        sched.step_tasks_only(self, now);
        self.scheduler = Some(sched);
    }
}

/// 命令执行器:任务节点 ↔ 执行位。
///
/// 主线节点(`is_main`)直接跑在 `self` 上;任务节点从 [`GroupVm::task_slots`]
/// 取执行位、`swap_exec` 换入,步进后换回。`task.pc`/`script_id` 每步同步,
/// 供调度器诊断与 TASKINFO 使用。
impl TaskExecutor for GroupVm {
    fn step_command(&mut self, task: &mut TaskNode) -> std::result::Result<StepOutcome, String> {
        // —— 换入任务执行位(主线不换)——
        let mut saved: Option<SavedExec> = None;
        if !task.is_main {
            let Some(slot) = self.task_slots.remove(&task.id) else {
                // 无执行位(纯注册任务 / TASKEND 已回收):本帧视为走完。
                return Ok(StepOutcome::Finish);
            };
            saved = Some(self.swap_exec(slot)); // saved ← 主线
        }
        self.cur_domain = if task.is_main { 0 } else { 1 };
        let r = self.step_group();
        self.cur_domain = 0;
        let is_finish = matches!(
            &r,
            Ok(Action::Break) | Ok(Action::Suspend(VmSuspend::Complete))
        );
        // 任务侧的 pc / 脚本号:必须在换回主线**之前**取值。
        let task_pc = self.pc;
        let task_sid = self.ctx.script_id;
        let out = match r {
            Err(e) => Err(e.to_string()),
            Ok(Action::Next) => {
                // 官方 `FUN_00404388`:处理器返回后由步进器推进 pc
                // (跳转处理器已自写 pc,故只对 Next 自增)。
                self.pc += 1;
                Ok(StepOutcome::Continue)
            }
            Ok(Action::Goto(target)) => {
                self.pc = target;
                Ok(StepOutcome::Continue)
            }
            Ok(Action::Keep) => Ok(StepOutcome::Continue),
            Ok(Action::Break) => Ok(StepOutcome::Finish),
            Ok(Action::Suspend(VmSuspend::Wait { counter, time_ms })) => {
                // 官方 WAIT 处理器只写任务记录 +0x18 / +0x1c 后返回 -1。
                task.wait_frames = counter.unwrap_or(1) as u32;
                if let Some(ms) = time_ms {
                    task.deadline_ms = Some(self.chain_now_ms.wrapping_add(ms as u32));
                }
                Ok(StepOutcome::Yield)
            }
            Ok(Action::Suspend(VmSuspend::Complete)) => Ok(StepOutcome::Finish),
            Ok(Action::Suspend(VmSuspend::None)) => Ok(StepOutcome::Continue),
            Ok(Action::Suspend(VmSuspend::Error(m))) => {
                if self.err_cause.is_none() {
                    self.err_cause = Some(format!("s{task_sid} pc={task_pc}: {m}"));
                }
                Err(m)
            }
        };
        // —— 换回主线 / 存回任务执行位 ——
        task.pc = task_pc;
        task.script_id = task_sid;
        if let Some(main_side) = saved {
            let task_side = self.swap_exec(main_side); // self ← 主线;task_side ← 任务
            if !is_finish && out.is_ok() {
                self.task_slots.insert(task.id.clone(), task_side);
            }
            // 任务体的 Finished/Error 只属于该任务,复位状态机。
            self.state = VmState::Running;
        }
        out
    }

    fn drain_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.pending_effects)
    }
}

/// 任务摘要行(`task_summary`;旧格式保持,`p2_commands` 逐字段断言)。
pub(crate) fn task_summary_line(vm: &GroupVm, n: &TaskNode) -> String {
    let (depth, loops) = vm
        .task_slots()
        .get(&n.id)
        .map(|s| (s.frames.len(), s.loops.len()))
        .unwrap_or((0, 0));
    format!(
        "{} s{} pc{} steps={} wait={} fin={} en={} frames={depth} loops={loops}",
        String::from_utf8_lossy(&n.id),
        n.script_id,
        n.pc,
        n.steps,
        n.wait_frames,
        n.finished,
        n.enabled
    )
}
