//! 流程控制族(从 `run_main` 巨型 match 拆出;语义逐行照搬,未改行为)。
//!
//! 拆分背景见 [`crate::exec`] 模块文档。**控制流约定**:方法体里不再出现
//! 「目标为外层 while」的 `continue` / `return Ok(VmSuspend…)` —— 分别替换为
//! `return Ok(Action::Keep)` / `return Ok(Action::Suspend(…))`,由 `run_main`
//! 的循环统一执行(语义 1:1)。

use yuris_core::Result;
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::{Value, VariableStore};

use crate::event::{GoOutcome, JumpKind, VmEvent, VmState, VmSuspend};
use crate::exec::Action;
use crate::util::value_truthy;
use crate::{GosubFrame, GroupVm, IfFrame, LoopFrame};
impl GroupVm {
    /// 处理 `Go`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_go(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        match self.exec_go(&windows)? {
            GoOutcome::Jump(target) => {
                let from = self.pc;
                self.push_event(VmEvent::Jump {
                    from,
                    to: target,
                    kind: JumpKind::Go,
                });
                self.push_event(VmEvent::GroupExecuted {
                    pc: from,
                    command: g.command_type,
                    condition: None,
                });
                self.pc = target;
                return Ok(Action::Keep);
            }
            GoOutcome::CrossScript { script_id, target } => {
                // 跨脚本 GO:事件归属 = 派发点(引擎真值:P1 对拍
                // 实锤,引擎在旧脚本 pc 记本组后才切上下文)。
                // switch_script 会改写 self.pc/self.ctx.script_id,
                // 故 from/from_script 必须在切换前捕获。
                let from = self.pc;
                let from_script = self.ctx.script_id;
                match self.switch_script(script_id, target) {
                    Ok(()) => {
                        self.push_event(VmEvent::GroupExecuted {
                            pc: from,
                            command: g.command_type,
                            condition: None,
                        });
                        self.push_event(VmEvent::ScriptSwitch {
                            from_script,
                            to_script: script_id,
                        });
                        self.push_event(VmEvent::Jump {
                            from,
                            to: target,
                            kind: JumpKind::Go,
                        });
                        return Ok(Action::Keep);
                    }
                    Err(e) => {
                        self.set_error_state();
                        return Ok(Action::Suspend(VmSuspend::Error(e.to_string())));
                    }
                }
            }
            GoOutcome::NoJump => {
                // 非 strict:未命中事件已记,顺序继续
                self.pc += 1;
                return Ok(Action::Keep);
            }
            GoOutcome::Halted(reason) => {
                self.set_error_state();
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
        }
    }

    /// 处理 `Gosub`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_gosub(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // w0 = 条件窗口(引擎 FUN_00442d01);求值失败 → 诚实挂起
        let cond = self.eval_window_condition(windows.first())?;
        let Some(cond) = cond else {
            if let Some(reason) = self.halt_or_record(self.pc, g.command_type, "GOSUB 无条件窗口")
            {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        };
        let truthy = value_truthy(&cond)?;
        if !truthy {
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: Some(cond),
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // 标签解析:找 M-串窗口(引擎查 hash 表)。
        //
        // **2026-10-17 用户要求**:引擎侧未命中是「静默不跳」(Confirmed),
        // 但同样意味着**脚本链缺了一段**。现在:
        // - strict(运行时默认):按未实现/未命中处理 —— 挂起 + 错误窗;
        // - lenient:保留引擎口径(记录组事件后顺序继续)。
        let resolved = self.resolve_label_in(&windows)?;
        let Some((target, target_script)) = resolved else {
            let reason = format!(
                "GOSUB 标签未命中(变量标签窗求值/查表未命中;开 YURIS_TRACE_LABELS=1 看查表轨迹)"
            );
            if let Some(reason) = self.halt_or_record(self.pc, g.command_type, reason) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: Some(cond),
            });
            self.pc += 1;
            return Ok(Action::Keep);
        };
        let from = self.pc;
        let from_script = self.ctx.script_id;
        let mut locals = VariableStore::new();
        self.seed_frame_locals(&mut locals, &windows, g.param)?;
        self.frames.push(GosubFrame {
            return_pc: from + 1,
            script_id: from_script,
            locals,
            // 引擎:帧 +1 存 IF 栈深;循环记录栈增长亦随 RETURN 丢弃
            // (成果 51.5)。
            if_nest_depth: self.if_nest.len(),
            loop_depth: self.loops.len(),
        });
        self.push_event(VmEvent::Call { from, to: target });
        self.push_event(VmEvent::GroupExecuted {
            pc: from,
            command: g.command_type,
            condition: Some(cond),
        });
        // 跨脚本 GOSUB:压帧后切换上下文到目标脚本,并记 ScriptSwitch。
        if target_script != from_script {
            self.push_event(VmEvent::ScriptSwitch {
                from_script: from_script,
                to_script: target_script,
            });
            self.switch_script(target_script, target)?;
        } else {
            self.pc = target;
        }
        return Ok(Action::Keep);
    }

    /// 处理 `ReturnCode`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_returncode(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        // RETURNCODE：调用点返回标记，**不改控制流**（Likely；
        // 证据链见 `commands.rs::CmdKind::ReturnCode` 与
        // `docs/engine/command-layer.md` §12）。
        //
        // 形态实测（全语料 28299 组）：恒 1 个空窗口，`tag` 为逐
        // 脚本递增序号 1,2,3,…。本 VM 只计数并把序号记进事件流 ——
        // 引擎侧真实副作用写在任务结构（`task+0xc`/`task+0x10`、
        // 事件队列）上，脚本侧观察不到，在此不臆造。
        self.returncode_hits += 1;
        // **行结束标记(2026-10-17,成果 170;Confirmed by corpus)**:
        // `RETURNCODE` 直接跟在最后一段 `WORD` 之后,把累积的文本缓冲
        // 提交成一行(语料 28992 WORD / 28299 RETURNCODE ≈ 1.02 片段/行;
        // s149 g723-726 = 三个 WORD + 一个 RETURNCODE 组成一句话)。
        // 旧实现只计数,文本永不换行提交。
        self.text_channel.commit_line();
        let code = windows.first().map(|w| w.tag).unwrap_or(0);
        self.push_event(VmEvent::Subsystem {
            pc: self.pc,
            command: g.command_type,
            evaluated: vec![(0, format!("retcode:{code}"))],
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `ReturnBreak`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_returnbreak(
        &mut self,
        _g: CommandGroup,
        _windows: &[CommandSlot],
    ) -> Result<Action> {
        // RETURNBREAK：编译期标记 / 空操作（Confirmed —— v555 引擎
        // 处理器 0x4634d8 = `xor eax,eax; ret`）。
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `Return`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_return(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        // 返回值虚拟族(P5 定性,成果 50):实参按 B0 槽号求值
        // (callee 上下文),写入调用者帧返回值区:
        //   B0 0x00/0x01-0x0f → INT  ret[@60][max(1,B0)]
        //   B0 0x10-0x1f      → FLT  ret[@61][max(1,B0-0x10)]
        //   B0 0x20-0x2f      → STR  ret[$62][max(1,B0-0x20)]
        // 槽从 1 起(引擎 0044b418 实锤,成果 54:写循环
        // `iVar9=1..=count`,0x160+8*i;B0=0 = 条件窗形态
        // 「无显式槽号」→ 默认槽 1)。语料:RETURN B0=0 共 215 窗
        // (189 单窗);@60/$62 读者下标全 ≥1(0 次出现,探针
        // probe_ret_readers.py)。实证:es._strlen RETURN B0=1 →
        // 调用者 @60[1];es._label.info.exist RETURN B0=0 → @60[1]
        // (s13 g476 → g2 读者)。
        //
        // depth 语义(0044b418 + 004428c0 交叉,成果 56):RETURN
        // 写 `obj+0x140+depth*4` = 读侧基 0x144 的 record[depth-1]
        // = **调用者记录**(+0x160/+0x1e8/+0x278 值区,+0x2ef/0x300/
        // 0x311 计数),弹帧后按同一记录 +4/+8 恢复;`if (0 < iVar8)`
        // 不成立 = depth 0(基帧 record[0] 上 RETURN)→ 脚本结束
        // (FUN_00451724)。VM frames.len() ↔ 引擎 depth:
        // len≤1 = 基帧上 RETURN → 结束;len>1 → 弹帧写调用者帧。
        //
        // **基帧结束语义(2026-10-15,成果 132 勘误)**:旧实现在此
        // 置 Finished + Complete —— 错。引擎 `0x44c390` 处理器在
        // depth==0 时跳出,外层 `FUN_00451640` 判**不成立则
        // `FUN_00451724`,后者复读 pc/脚本号,清源 `0x42f7bc` 再经
        // `0x452364` 解链 + 按优先级重排队** —— 任务只是**挂起等重
        // 入**(`WAIT[..] continue` 同构),主循环下帧从原 (pc,脚本)
        // 复跑。只有 `Fundamental`/整体终态(FUN_00465d08 路径)才
        // 真杀任务。做成 Wait(None) 超集:pc/脚本号保持不变。
        if self.frames.len() <= 1 {
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: None,
            });
            return Ok(Action::Suspend(VmSuspend::Wait {
                counter: None,
                time_ms: None,
            }));
        }
        let mut ret_vals: Vec<(u32, Value)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let b0 = (w.tag & 0xff) as u32;
            if b0 >= 0x30 {
                continue; // 区段外
            }
            if let Some(v) = self.eval_window_condition(Some(w))? {
                ret_vals.push((b0, v));
            }
        }
        let Some(frame) = self.frames.pop() else {
            unreachable!("frames.len()>1 已保证可弹");
        };
        // 引擎 RETURN 语义(成果 51.5):IF 嵌套栈深恢复为帧记录值,
        // 子程序内遗留的未弹 IF 项 / 循环记录整体丢弃。
        self.if_nest.truncate(frame.if_nest_depth);
        self.loops.truncate(frame.loop_depth);
        {
            let from = self.pc;
            let cur_script = self.ctx.script_id;
            // 返回帧所在脚本号;切帧(跨脚本返回)。
            let cross = frame.script_id != cur_script;
            self.push_event(VmEvent::Jump {
                from,
                to: frame.return_pc,
                kind: JumpKind::Return,
            });
            // 本组事件归属 = 派发点脚本(P1 引擎对拍);跨脚本时
            // 须在切换前发 ScriptSwitch,否则事件流重建脚本号错位。
            self.push_event(VmEvent::GroupExecuted {
                pc: from,
                command: g.command_type,
                condition: None,
            });
            if cross {
                self.push_event(VmEvent::ScriptSwitch {
                    from_script: cur_script,
                    to_script: frame.script_id,
                });
                self.switch_script(frame.script_id, frame.return_pc)?;
            }
            // 返回值写入调用者帧(弹出后 frames.last() = 调用者)
            for (b0, v) in ret_vals {
                // **槽号 = (B0 & 0x0f) + 1(2026-10-16 勘误,成果 139)**
                //
                // 旧实现按类型分别算:`b0.max(1)`(INT)、
                // `(b0-0x10).max(1)`(FLT)、`(b0-0x20).max(1)`(STR)——
                // 这**只在 `B0 & 0x0f == 0` 时才与引擎一致**,其余全部错位:
                //
                // ```text
                // B0=0x20 → 旧: max(1, 0x20-0x20)=1   新: (0)+1=1   ✓ 同
                // B0=0x21 → 旧: max(1, 1)         =1   新: (1)+1=2   ✗ 旧**撞号覆盖**
                // B0=0x02 → 旧: max(1, 2)         =2   新: (2)+1=3   ✗ 旧差 1
                // B0=0x03 → 旧: 3                      新: 4         ✗
                // B0=0x04 → 旧: 4                      新: 5         ✗
                // ```
                //
                // **实测证据(成果 139)**:`es.SP.PRO.CHECKCG`(s65 g29)
                // 以 `RETURN rstr=$0e42 rstr=$0e43 int=@0e44 int=@0e45 int=@0e46`
                // 返回**5 个值**(B0 = 0x20/0x21/0x02/0x03/0x04),而调用点
                // `s66 pc3402..3406` 逐个读回:
                //
                // ```text
                // $0f42 = $62[1]   $0f43 = $62[2]
                // @0f44 = @60[3]   @0f45 = @60[4]   @0f46 = @60[5]
                // ```
                // ⇒ 恰好是 `(B0 & 0x0f) + 1`。旧实现下 `0x21` 覆盖了 `0x20`
                // 写过的槽 1、而 3/4/5 号槽**从未被写** —— 于是精灵 CG 路径
                // (`$0f42`/`$0f43`)**恒为空串**,`CG[FILE=""]` 得到 1×1 黑层,
                // 片头 LOGO 与一切走 `es.SP.*` 的图都画不出来。
                //
                // 为什么此前没暴露:`es._strlen` / `_strleft` 这类**单返回值**
                // 函数 B0 恒为 0x00/0x20(即 `&0x0f == 0`),两种算法同值 ——
                // 只有**多返回值**函数才会踩到,而链上第一个多返回值函数
                // 恰好在 LOGO 路径上。
                //
                // 证据等级:**Confirmed**(5 个采样点逐一吻合 + 端到端复现)。
                let (r, idx, et): (yuris_value::VarRef, u32, yuris_value::ElemType) = match b0 {
                    0x00..=0x0f => (
                        yuris_value::VarRef {
                            space: yuris_value::VarSpace::At,
                            id: 60,
                        },
                        (b0 & 0x0f) + 1,
                        yuris_value::ElemType::Int,
                    ),
                    0x10..=0x1f => (
                        yuris_value::VarRef {
                            space: yuris_value::VarSpace::At,
                            id: 61,
                        },
                        (b0 & 0x0f) + 1,
                        yuris_value::ElemType::Float,
                    ),
                    0x20..=0x2f => (
                        yuris_value::VarRef {
                            space: yuris_value::VarSpace::Dollar,
                            id: 62,
                        },
                        (b0 & 0x0f) + 1,
                        yuris_value::ElemType::Str,
                    ),
                    unknown_b0 => {
                        // 返回值打包槽类型未建模(0x30..=0xff):返回值**丢失**。
                        // 以前是 `continue` 静默跳过;现在至少让缺失可见。
                        self.note_unknown_slot(
                            g.command_type,
                            unknown_b0 as u8,
                            "RETURN 返回值打包类型(B0 高半字节未建模,值已丢弃)",
                        );
                        continue;
                    }
                };
                if let Some(caller) = self.frames.last_mut() {
                    if !caller.locals.has_array(&r) {
                        caller.locals.declare_array(&r, et, &[16]);
                    }
                    let cv: Value = match (et, v) {
                        (yuris_value::ElemType::Int, Value::Int(i)) => Value::Int(i),
                        (yuris_value::ElemType::Int, Value::Float(f)) => {
                            Value::Int(f.round() as i64)
                        }
                        (yuris_value::ElemType::Float, Value::Int(i)) => Value::Float(i as f64),
                        (yuris_value::ElemType::Float, Value::Float(f)) => Value::Float(f),
                        (yuris_value::ElemType::Str, Value::Str(mut s)) => {
                            if s.len() >= 2 && s.first() == Some(&b'"') && s.last() == Some(&b'"') {
                                s = s[1..s.len() - 1].to_vec();
                            }
                            Value::Str(s)
                        }
                        (_, other) => other,
                    };
                    let _ = caller.locals.set_elem(&r, &[idx as i64], cv);
                }
            }
            self.pc = frame.return_pc;
            return Ok(Action::Keep);
        }
    }

    /// 处理 `If`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_if(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        if g.window_count != 3 {
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                format!("IF 窗口数 {} != 3(语料恒 3)", g.window_count),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        let cond = self.eval_window_condition(windows.first())?;
        let Some(cond) = cond else {
            if let Some(reason) = self.halt_or_record(self.pc, g.command_type, "IF 无条件窗口")
            {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        };
        let truthy = value_truthy(&cond)?;
        // 嵌套栈:end 目标 = w2.len(编译期组号)
        self.if_nest.push(IfFrame {
            if_pc: self.pc,
            end_target: windows[2].len,
        });
        if !truthy {
            // 假 → 跳 w1.len ?: w2.len
            let to = if windows[1].len != 0 {
                windows[1].len
            } else {
                windows[2].len
            };
            let from = self.pc;
            self.push_event(VmEvent::Jump {
                from,
                to: to as usize,
                kind: JumpKind::IfFalse,
            });
            self.push_event(VmEvent::GroupExecuted {
                pc: from,
                command: g.command_type,
                condition: Some(cond),
            });
            self.pc = to as usize;
            return Ok(Action::Keep);
        }
        self.push_event(VmEvent::GroupExecuted {
            pc: self.pc,
            command: g.command_type,
            condition: Some(cond),
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `IfBlend`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_ifblend(
        &mut self,
        g: CommandGroup,
        _windows: &[CommandSlot],
    ) -> Result<Action> {
        let Some(top) = self.if_nest.last().copied() else {
            if let Some(reason) = self.halt_or_record(self.pc, g.command_type, "IFBLEND 无配对 IF")
            {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        };
        let from = self.pc;
        self.push_event(VmEvent::Jump {
            from,
            to: top.end_target as usize,
            kind: JumpKind::Else,
        });
        self.push_event(VmEvent::GroupExecuted {
            pc: from,
            command: g.command_type,
            condition: None,
        });
        self.pc = top.end_target as usize;
        return Ok(Action::Keep);
    }

    /// 处理 `Loop`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_loop(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00445a00:w0 = 计数表达式(INT, B2=1);
        // 压循环帧 {body_start=pc+1, limit=w0, counter=1};深度上限 0x40
        if self.loops.len() >= 0x40 {
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                "循环嵌套超过 64 层(引擎 0x18ce0 同族)",
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        let limit = match self.eval_window_condition(windows.first())? {
            Some(Value::Int(n)) if n >= 0 => Some(n as u64),
            Some(Value::Int(_)) => None, // 引擎负值 → 0xffffffff/-1 无限标记(Likely)
            Some(v) => {
                let reason = format!("LOOP 计数非 INT: {v:?}");
                self.set_error_state();
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            None => {
                if let Some(reason) =
                    self.halt_or_record(self.pc, g.command_type, "LOOP 无计数窗口")
                {
                    return Ok(Action::Suspend(VmSuspend::Error(reason)));
                }
                self.pc += 1;
                return Ok(Action::Keep);
            }
        };
        // 退出点 = w1.len(编译期组号 = 配对 LOOPEND;引擎 rec+8
        // = DAT_00661428,语料实证 s190 g332 w1.len=338→g338、
        // s7 g93 w1.len=97→g97,均为 LOOPEND 组)。
        let exit_target = if windows.len() > 1 {
            windows[1].len as usize
        } else {
            0
        };
        self.loops.push(LoopFrame {
            body_start: self.pc + 1,
            exit_target,
            limit,
            counter: 1,
            if_nest_depth: self.if_nest.len(),
        });
        if std::env::var_os("YURIS_TRACE_LOOPS").is_some() {
            let stack: Vec<String> = self
                .loops
                .iter()
                .map(|f| format!("b{}", f.body_start))
                .collect();
            yuris_log::trace_forced!(
                "[loop] s{} pc{} limit={:?} exit={} depth={} stack=[{}]",
                self.ctx.script_id,
                self.pc,
                limit,
                exit_target,
                self.loops.len(),
                stack.join(" ")
            );
        }
        // 引擎 00445a00 limit==0 路径:counter=0 且 PC = rec+8
        // (退出点)—— **不进循环体**,记录留栈由 LOOPEND 弹出
        // (对拍 [546] 实证:s7 g93 limit=0 → g97 LOOPEND)。
        if limit == Some(0) {
            if let Some(f) = self.loops.last_mut() {
                f.counter = 0;
            }
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: None,
            });
            self.pc = exit_target;
            return Ok(Action::Keep);
        }
        // 注意:limit>0 恒压帧;LOOPEND「counter+1 >= limit → 弹帧」
        // 循环体内的 GOSUB 返回后 LOOPEND 有配对(端到端实测踩坑,勿回退)。
        self.push_event(VmEvent::GroupExecuted {
            pc: self.pc,
            command: g.command_type,
            condition: None,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `LoopBreak`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_loopbreak(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        // 引擎 CMDH_00445b9c:LV 参数 = 回退层数(默认 1);
        // 保留目标层记录(counter=0/limit=0),丢弃其上 LV-1 层,
        // PC = 该层 rec+8 = 退出点(LOOP 组号,配对 LOOPEND);
        // 随后 LOOPEND 执行(发事件)并弹栈(成果 55b:rec+8 =
        // w1.len 反编译+语料双证,取代旧动态配对)。
        let level = self.loop_level_from(windows.first())?;
        let Some(level) = level else {
            self.pc += 1;
            return Ok(Action::Keep);
        };
        if level < 1 || level > self.loops.len() {
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                format!("LOOPBREAK 层级 {level} 越界(深度 {})", self.loops.len()),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        let keep = self.loops.len() - level; // 保留该层(含)
        let exit = self.loops[keep].exit_target;
        let from = self.pc;
        self.push_event(VmEvent::GroupExecuted {
            pc: from,
            command: g.command_type,
            condition: None,
        });
        self.push_event(VmEvent::Jump {
            from,
            to: exit,
            kind: JumpKind::LoopBreak,
        });
        self.loops.truncate(keep + 1);
        if let Some(f) = self.loops.last_mut() {
            f.counter = 0;
            f.limit = Some(0);
        }
        // **展开循环体内残留的 IF 帧(成果 133)**:LOOPBREAK 跳过体内剩余的
        // `IFEND`(实测 `es.SP.FINISH.SET` g4765 IF / g4766 LOOPBREAK /
        // g4767 IFEND),残留帧会让之后的 IFBLEND/ELSE 跳到错误目标
        // (实测绕回循环尾 → `LOOPEND 无配对 LOOP`)。
        // 引擎同族语义:块记录随执行位恢复(RETURN 处理器 truncate,成果 51.5)。
        let depth = self.loops.get(keep).map(|f| f.if_nest_depth);
        if let Some(d) = depth {
            if self.if_nest.len() > d {
                self.if_nest.truncate(d);
            }
        }
        self.pc = exit;
        return Ok(Action::Keep);
    }

    /// 处理 `LoopContinue`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_loopcontinue(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        // 引擎 CMDH_00445c54:保留目标层记录(counter/limit 不动),
        // 丢弃其上 LV-1 层,PC = 该层 rec+8(LOOPEND 组)→
        // LOOPEND 递增 counter 并回体顶/退出(成果 55b)。
        let level = self.loop_level_from(windows.first())?;
        let Some(level) = level else {
            self.pc += 1;
            return Ok(Action::Keep);
        };
        if level < 1 || level > self.loops.len() {
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                format!("LOOPCONTINUE 层级 {level} 越界(深度 {})", self.loops.len()),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        let keep = self.loops.len() - level;
        let exit = self.loops[keep].exit_target;
        let from = self.pc;
        self.push_event(VmEvent::GroupExecuted {
            pc: from,
            command: g.command_type,
            condition: None,
        });
        self.push_event(VmEvent::Jump {
            from,
            to: exit,
            kind: JumpKind::LoopContinue,
        });
        self.loops.truncate(keep + 1);
        // 同 LOOPBREAK:体内残留 IF 帧一并展开(成果 133)。
        let depth = self.loops.get(keep).map(|f| f.if_nest_depth);
        if let Some(d) = depth {
            if self.if_nest.len() > d {
                self.if_nest.truncate(d);
            }
        }
        self.pc = exit;
        return Ok(Action::Keep);
    }

    /// 处理 `LoopEnd`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_loopend(
        &mut self,
        g: CommandGroup,
        _windows: &[CommandSlot],
    ) -> Result<Action> {
        // 引擎 CMDH_00445ce8:counter+1;counter >= limit → 弹帧顺序继续;
        // 否则 PC = body_start(64 位计数,0xffffffff/-1 = 无限标记)
        if let Some(sel) = std::env::var_os("YURIS_TRACE_LOOPEND") {
            let s = sel.to_string_lossy();
            let want = s.split(',').filter_map(|x| x.trim().parse::<usize>().ok());
            if want.into_iter().any(|p| p == self.pc) {
                let (c, l) = self
                    .loops
                    .last()
                    .map(|f| (f.counter, f.limit))
                    .unwrap_or((0, None));
                yuris_log::trace_forced!(
                    "[loopend] s{} pc{} counter={} limit={:?} depth={}",
                    self.ctx.script_id,
                    self.pc,
                    c,
                    l,
                    self.loops.len()
                );
            }
        }
        let Some(top) = self.loops.last().copied() else {
            if let Some(reason) =
                self.halt_or_record(self.pc, g.command_type, "LOOPEND 无配对 LOOP")
            {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        };
        let counter = top.counter + 1;
        if let Some(limit) = top.limit {
            if counter > limit {
                self.loops.pop();
                self.push_event(VmEvent::GroupExecuted {
                    pc: self.pc,
                    command: g.command_type,
                    condition: None,
                });
                self.pc += 1;
                return Ok(Action::Keep);
            }
        }
        if let Some(f) = self.loops.last_mut() {
            f.counter = counter;
        }
        let from = self.pc;
        // 循环回跳路径同样记本组事件(引擎每派发必有一事件;
        // P1 对拍实锤:engine 39 pc118 LOOPEND 每次迭代都有事件)
        self.push_event(VmEvent::GroupExecuted {
            pc: from,
            command: g.command_type,
            condition: None,
        });
        self.push_event(VmEvent::Jump {
            from,
            to: top.body_start,
            kind: JumpKind::LoopContinue,
        });
        self.pc = top.body_start;
        return Ok(Action::Keep);
    }

    /// 处理 `Wait`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_wait(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00455d58:求值参数;FRAME 参数(槽 0)→ obj+0x18
        // 帧计数,让出;TIME 参数(槽 1)→ obj+0x1c = timeGetTime()+ms,让出;
        // 两者都无 → 不等待。
        let mut counter = None;
        let mut time_ms = None;
        for (i, w) in windows.iter().enumerate() {
            if w.len == 0 {
                continue;
            }
            let Some(v) = self.eval_window_condition(Some(w))? else {
                continue;
            };
            // YSCM WAIT 参数序:FRAME(槽0) TIME(槽1);按窗口出现的槽序归类
            let slot = (w.tag & 0xff) as u8;
            match slot {
                0 => counter = v.as_int_opt().map(|n| n as u64),
                1 => time_ms = v.as_int_opt().map(|n| n as u64),
                unknown => {
                    // WAIT 的未知槽:参数被忽略(可能变成「不等待」)。
                    self.note_unknown_slot(
                        g.command_type,
                        unknown,
                        "WAIT 参数槽(只建模 FRAME=0 / TIME=1)",
                    );
                }
            }
            let _ = i;
        }
        if counter.is_none() && time_ms.is_none() {
            // 无参数 → 引擎直接返回 0(不等待)
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: None,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        self.push_event(VmEvent::GroupExecuted {
            pc: self.pc,
            command: g.command_type,
            condition: None,
        });
        self.pc += 1; // 先推进:等完后继续执行
        return Ok(Action::Suspend(VmSuspend::Wait { counter, time_ms }));
    }

    /// 处理 `Else`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_else(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_0043d34c(反编译,成果 55):
        //   w0 无条件(B2=0)→ 顺序(进 else 块);
        //   w0 求值真 → 顺序;假 → PC = w1.len != 0 ? w1.len
        //   : 嵌套栈顶 end 目标。w1.len 存编译期组号 = ELSE 链中
        //   **下一个 ELSE**(语料实证:s126 g66 w1.len=70 → g70;
        //   g70 w1.len=0 → 落 IFEND g72);旧实现直接跳 end 目标,
        //   跳过链中后续 ELSE 块(对拍 [518] 分歧根因)。
        //   嵌套栈不弹(IFEND 负责弹,引擎同)。
        let cond = if windows.len() > 0 && windows[0].len > 0 {
            self.eval_window_condition(Some(&windows[0]))?
        } else {
            None
        };
        let truthy = if let Some(v) = cond {
            value_truthy(&v)?
        } else {
            true // 无条件 ELSE → 进块
        };
        let Some(top) = self.if_nest.last().copied() else {
            if let Some(reason) = self.halt_or_record(self.pc, g.command_type, "ELSE 无配对 IF")
            {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        };
        if truthy {
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: None,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // 假 → 下一 ELSE(w1.len);无 → 嵌套栈顶 end 目标
        let to = if windows.len() > 1 && windows[1].len != 0 {
            windows[1].len as usize
        } else {
            top.end_target as usize
        };
        let from = self.pc;
        self.push_event(VmEvent::Jump {
            from,
            to,
            kind: JumpKind::Else,
        });
        self.push_event(VmEvent::GroupExecuted {
            pc: from,
            command: g.command_type,
            condition: None,
        });
        self.pc = to;
        return Ok(Action::Keep);
    }

    /// 处理 `End`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_end(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 FUN_0043da44:设标志 DAT_008725dc=1(脚本结束),可选返回码→
        // DAT_00872228。本 VM 以 Complete 结束。
        //
        // **返回码求值失败以前被 `.ok().flatten()` 吞掉**(END 照常结束,
        // 返回码静默变 None)。现在至少留痕:END 的返回码影响调用方分支。
        let ret = if let Some(w) = windows.first() {
            match self.eval_window_condition(Some(w)) {
                Ok(v) => v,
                Err(e) => {
                    yuris_log::alert_once(
                        module_path!(),
                        format!("end-retcode-fail:{}:{}", self.ctx.script_id, self.pc),
                        format!(
                            "END 返回码求值失败,按「无返回码」结束\n\
                             现场: script {} pc {}\n\
                             错误: {e}\n\
                             处置: 调用方将看到无返回值;检查该表达式",
                            self.ctx.script_id, self.pc
                        ),
                    );
                    None
                }
            }
        } else {
            None
        };
        self.push_event(VmEvent::GroupExecuted {
            pc: self.pc,
            command: g.command_type,
            condition: ret,
        });
        self.state = VmState::Finished;
        return Ok(Action::Suspend(VmSuspend::Complete));
    }

    /// 处理 `IfEnd`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_ifend(&mut self, g: CommandGroup, _windows: &[CommandSlot]) -> Result<Action> {
        if self.if_nest.pop().is_none() {
            if let Some(reason) = self.halt_or_record(self.pc, g.command_type, "IFEND 无配对 IF")
            {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        self.push_event(VmEvent::GroupExecuted {
            pc: self.pc,
            command: g.command_type,
            condition: None,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }
}
