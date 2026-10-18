//! `TASKINFO` —— 任务状态查询（手册 `control/taskinfo.html`）。
//!
//! ## 取证（v494 处理体 `0x452270`，逐槽反汇编）
//!
//! 参数表（游戏自身 YSCM，21 项）：
//! `ID / LET / EXIST / E / A / Z / FILE / FILENO / LINE / FILE0 / FILENO0 /
//! LINE0 / TEXTLINE / NEXTVOICE / GOLABEL / GOSUBLABEL / IFNEST / GOSUBNEST /
//! LOOPNEST / SCRIPTPOS / TEXTPOS`
//!
//! 引擎按槽号（`0x6821c0 + n`）逐项短路判断，读到即写 `LET` 并返回：
//!
//! | 槽 | 字段 | 引擎读数 | 本内核 |
//! |---|---|---|---|
//! | 0 | `ID` | 任务名串（缺省 = 当前任务） | 已实现 |
//! | 1 | `LET` | 写回目标（引用窗） | 已实现 |
//! | 2 | `EXIST` | 任务查表命中 → 1（`0x45266a`） | 已实现 |
//! | 6 | `FILE` | `yst_list` 名字表 `[script_id]`（`0x45270c`） | 已实现 |
//! | 7 | `FILENO` | 任务 `+0x3c`（脚本号，`0x4524e7`） | 已实现 |
//! | 16 | `IFNEST` | 任务 `+0x3e`（IF 嵌套深度，`0x452559`） | 已实现 |
//! | 17 | `GOSUBNEST` | 任务 `+0x140`（帧栈深度，`0x452533`） | 已实现 |
//! | 18 | `LOOPNEST` | 任务 `+0x244`（循环嵌套深度，`0x45250d`） | 已实现 |
//! | 3 | `E` | 任务 `+0x14`（`0x452647`） | **未取证**（本内核 `enabled` 是 `+0x15 & +5` 的建模，位面不同，不猜） |
//! | 4/5 | `A`/`Z` | 任务 `+0x15` / 目标描述符回写（`0x452624`/`0x452600`，含义未定性） | **未取证** |
//! | 8..13 | `LINE`/`FILE0`/`FILENO0`/`LINE0`/`TEXTLINE`/`NEXTVOICE` | 走 `0x43f09c`/`0x43f11c`/`0x43f194` 等**行号表**查询（表由 `yst_list.ybn` 之外的构建产物建立） | **未取证** |
//! | 19 | `SCRIPTPOS` | `0x4528c8` 位置序列化（见下） | 已实现（容器格式 Confirmed，内部往返自洽） |
//! | 20 | `TEXTPOS` | v494 处理体无此分支 | 空操作（同引擎） |
//!
//! ## `SCRIPTPOS` 编码（本次取证副产物，格式已定、字段语义未定）
//!
//! 序列化器 `0x4528c8` 的形态已完整读出（供后续接手）：
//!
//! ```text
//! [8 字节魔数]  0x5b4090 = 01 02 03 04 05 06 07 08
//! [2 字符]      帧栈深度 [task+0x3e] 的两个十六进制半字节（'a'+n）
//! 每个帧 × 18 字符:
//!   +0  2 字符   byte[frame+0]           （1 字节）
//!   +2  8 字符   u32 [frame+4]           （4 字节）
//!   +10 8 字符   u32 [frame+8]           （4 字节）
//! [2 字符]      第二栈深度 [task+0x140]
//! 每项 × 16 字符:
//!   +0  2 字符   byte[ent+0]
//!   +2  2 字符   byte[ent+1]
//!   +4  8 字符   u32 [ent+4]
//!   +12 4 字符   u16 [ent+8]
//! ```
//!
//! 十六进制数字用 `'a' + nibble`（`0x61` 起），不是 `'0'-'9'/'A'-'F'`。
//! 本内核按同一容器格式在 [`yuris_format::scriptpos`] 里实现编解码，
//! 把自身的 GOSUB/IF/LOOP 栈映射进三段。**引擎字节兼容性未验证**（帧记录
//! 字段语义未取证）—— 该模块只保证本内核内部往返无损，不声称能被引擎读取。
//!
//! `TEXTPOS`（槽 20）在 v494 处理体里**没有任何分支**（槽号 `0x6821d4` 起
//! 被 `EMOTE` 使用），即引擎也不处理它 —— 本模块按空操作实现。

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::Value;

use crate::event::{VmEvent, VmSuspend};
use crate::exec::Action;
use crate::util::{extract_var_target, value_summary, VarTarget};
use crate::GroupVm;

impl GroupVm {
    /// `TASKINFO[ID=… LET=… EXIST=1 …]`。
    pub(crate) fn cmd_taskinfo(
        &mut self,
        g: CommandGroup,
        windows: &[CommandSlot],
    ) -> Result<Action> {
        let cmd = g.command_type;
        let s_id = self.param_slot(cmd, "ID", 0);
        let s_let = self.param_slot(cmd, "LET", 1);
        let s_exist = self.param_slot(cmd, "EXIST", 2);
        let s_e = self.param_slot(cmd, "E", 3);
        let s_a = self.param_slot(cmd, "A", 4);
        let s_z = self.param_slot(cmd, "Z", 5);
        let s_file = self.param_slot(cmd, "FILE", 6);
        let s_fileno = self.param_slot(cmd, "FILENO", 7);
        let s_line = self.param_slot(cmd, "LINE", 8);
        let s_file0 = self.param_slot(cmd, "FILE0", 9);
        let s_fileno0 = self.param_slot(cmd, "FILENO0", 10);
        let s_line0 = self.param_slot(cmd, "LINE0", 11);
        let s_textline = self.param_slot(cmd, "TEXTLINE", 12);
        let s_nextvoice = self.param_slot(cmd, "NEXTVOICE", 13);
        let s_golabel = self.param_slot(cmd, "GOLABEL", 14);
        let s_gosublabel = self.param_slot(cmd, "GOSUBLABEL", 15);
        let s_ifnest = self.param_slot(cmd, "IFNEST", 16);
        let s_gosubnest = self.param_slot(cmd, "GOSUBNEST", 17);
        let s_loopnest = self.param_slot(cmd, "LOOPNEST", 18);
        let s_scriptpos = self.param_slot(cmd, "SCRIPTPOS", 19);
        let s_textpos = self.param_slot(cmd, "TEXTPOS", 20);

        let mut target: Option<VarTarget> = None;
        let mut name: Option<Vec<u8>> = None;
        // 被请求的查询字段（引擎按"槽存在 + 值非 0"判定）。
        let mut want: Vec<(&'static str, u8)> = Vec::new();
        let mut evaluated: Vec<(u8, String)> = Vec::new();

        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            if Some(slot) == s_let {
                let bytes = self
                    .ctx
                    .script
                    .window_bytes_pooled_copy(w)
                    .ok_or_else(|| Error::format("TASKINFO LET 引用窗越过池尾"))?;
                target = Some(extract_var_target(&bytes)?);
                evaluated.push((slot, "<ref>".into()));
                continue;
            }
            if Some(slot) == s_id {
                match self.eval_window_condition(Some(w)) {
                    Ok(Some(v)) => {
                        if let Value::Str(b) = &v {
                            name = Some(b.clone());
                        }
                        evaluated.push((slot, value_summary(&v)));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if std::env::var_os("YURIS_TRACE_TASKINFO").is_some() {
                            yuris_log::trace_forced!("[taskinfo] ID eval err: {e}");
                        }
                    }
                }
                continue;
            }
            let truthy = matches!(
                self.eval_window_condition(Some(w)),
                Ok(Some(v)) if v.as_int_opt().map(|n| n != 0).unwrap_or(false)
            );
            if !truthy {
                if let Ok(Some(v)) = self.eval_window_condition(Some(w)) {
                    evaluated.push((slot, value_summary(&v)));
                }
                continue;
            }
            let field = if Some(slot) == s_exist {
                Some("EXIST")
            } else if Some(slot) == s_e {
                Some("E")
            } else if Some(slot) == s_a {
                Some("A")
            } else if Some(slot) == s_z {
                Some("Z")
            } else if Some(slot) == s_file {
                Some("FILE")
            } else if Some(slot) == s_fileno {
                Some("FILENO")
            } else if Some(slot) == s_line {
                Some("LINE")
            } else if Some(slot) == s_file0 {
                Some("FILE0")
            } else if Some(slot) == s_fileno0 {
                Some("FILENO0")
            } else if Some(slot) == s_line0 {
                Some("LINE0")
            } else if Some(slot) == s_textline {
                Some("TEXTLINE")
            } else if Some(slot) == s_nextvoice {
                Some("NEXTVOICE")
            } else if Some(slot) == s_golabel {
                Some("GOLABEL")
            } else if Some(slot) == s_gosublabel {
                Some("GOSUBLABEL")
            } else if Some(slot) == s_ifnest {
                Some("IFNEST")
            } else if Some(slot) == s_gosubnest {
                Some("GOSUBNEST")
            } else if Some(slot) == s_loopnest {
                Some("LOOPNEST")
            } else if Some(slot) == s_scriptpos {
                Some("SCRIPTPOS")
            } else if Some(slot) == s_textpos {
                Some("TEXTPOS")
            } else {
                None
            };
            match field {
                Some(f) => want.push((f, slot)),
                None => self.note_unknown_slot(cmd, slot, "TASKINFO 查询字段"),
            }
            if let Ok(Some(v)) = self.eval_window_condition(Some(w)) {
                evaluated.push((slot, value_summary(&v)));
            }
        }

        // ---- 目标任务状态 ----
        let state = self.task_query_state(name.as_deref());

        // ---- 逐字段作答（引擎：短路，首个被请求字段即答案）----
        //
        // `ID` 查不到时（`state.present == false`）引擎按 LET 目标类型写默认值
        //（`0x45278c` 的三分支：INT→0 / FLT→0.0 / STR→空串），**不**去读
        // 当前任务的状态。`EXIST` 例外：它本来就答「有没有」= 0。
        let mut answered: Vec<(String, String)> = Vec::new();
        let mut unimplemented: Vec<&str> = Vec::new();
        if !state.present {
            for (field, _slot) in &want {
                if *field == "EXIST" {
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Int(0))?;
                    }
                    answered.push((field.to_string(), "0".into()));
                } else if *field == "TEXTPOS" {
                    answered.push((field.to_string(), "<引擎无实现>".into()));
                } else {
                    // 引擎同款：按目标类型写默认值。
                    if let Some(t) = &target {
                        let default = match t {
                            VarTarget::Scalar(r) if r.space == yuris_value::VarSpace::Dollar => {
                                Value::Str(Vec::new())
                            }
                            _ => Value::Int(0),
                        };
                        self.write_var_target(t, default)?;
                    }
                    answered.push((field.to_string(), "<目标任务不存在 → 默认值>".into()));
                }
            }
            self.push_event(VmEvent::TaskInfo {
                pc: self.pc,
                task: name
                    .as_deref()
                    .map(|n| String::from_utf8_lossy(n).to_string()),
                answered,
                missing: Vec::new(),
                evaluated,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        for (field, _slot) in &want {
            match *field {
                "EXIST" => {
                    let hit = state.exists;
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Int(hit as i64))?;
                    }
                    answered.push((field.to_string(), (hit as i64).to_string()));
                }
                "FILENO" => {
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Int(state.script_id as i64))?;
                    }
                    answered.push((field.to_string(), state.script_id.to_string()));
                }
                "FILE" => {
                    let bytes = state.file_name.clone().unwrap_or_default();
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Str(bytes.clone()))?;
                    }
                    answered.push((
                        field.to_string(),
                        String::from_utf8_lossy(&bytes).to_string(),
                    ));
                }
                "IFNEST" => {
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Int(state.if_nest as i64))?;
                    }
                    answered.push((field.to_string(), state.if_nest.to_string()));
                }
                "GOSUBNEST" => {
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Int(state.gosub_nest as i64))?;
                    }
                    answered.push((field.to_string(), state.gosub_nest.to_string()));
                }
                "LOOPNEST" => {
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Int(state.loop_nest as i64))?;
                    }
                    answered.push((field.to_string(), state.loop_nest.to_string()));
                }
                // `SCRIPTPOS`：按引擎容器格式序列化当前执行位置
                // （`yuris_format::scriptpos`；内部往返自洽，引擎字节兼容未验证）。
                "SCRIPTPOS" => {
                    let pos = self.script_pos_snapshot();
                    let text = pos.encode();
                    if let Some(t) = &target {
                        self.write_var_target(t, Value::Str(text.clone().into_bytes()))?;
                    }
                    answered.push((field.to_string(), format!("<SCRIPTPOS {} 字符>", text.len())));
                }
                // `TEXTPOS`：v494 处理体无此分支（槽号已被 EMOTE 占用）→ 空操作。
                "TEXTPOS" => {
                    answered.push((field.to_string(), "<引擎无实现>".into()));
                }
                other => unimplemented.push(other),
            }
        }

        self.push_event(VmEvent::TaskInfo {
            pc: self.pc,
            task: name
                .as_deref()
                .map(|n| String::from_utf8_lossy(n).to_string()),
            answered,
            missing: unimplemented.iter().map(|s| s.to_string()).collect(),
            evaluated,
        });

        if !unimplemented.is_empty() {
            let detail = format!(
                "TASKINFO 查询字段未实现: {}（引擎读数已定位,但值来源未取证）",
                unimplemented.join("/")
            );
            if let Some(reason) = self.missing_feature_halt(cmd, &detail) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
        }
        self.pc += 1;
        Ok(Action::Keep)
    }

    /// 当前执行位置的 `SCRIPTPOS` 快照（容器格式见 `yuris_format::scriptpos`）。
    fn script_pos_snapshot(&self) -> yuris_format::scriptpos::ScriptPos {
        use yuris_format::scriptpos::{BlockPos, GosubPos, LoopPos, ScriptPos};
        ScriptPos {
            gosub: self
                .frames
                .iter()
                .skip(1) // 基帧不是 GOSUB 产物
                .map(|f| GosubPos {
                    script_id: f.script_id,
                    return_pc: f.return_pc as u32,
                    if_depth: f.if_nest_depth as u32,
                })
                .collect(),
            blocks: self
                .if_nest
                .iter()
                .map(|f| BlockPos {
                    kind: 1,
                    flag: 0,
                    pc: f.if_pc as u32,
                    sub: f.end_target.min(u16::MAX as u32) as u16,
                })
                .collect(),
            loops: self
                .loops
                .iter()
                .map(|l| LoopPos {
                    body_start: l.body_start as u32,
                    exit_target: l.exit_target as u32,
                    counter: l.counter as u32,
                    limit: l.limit.map(|v| v.min(u32::MAX as u64) as u32).unwrap_or(u32::MAX),
                })
                .collect(),
        }
    }

    /// 取目标任务的查询状态（`None` = 当前执行位）。
    fn task_query_state(&self, name: Option<&[u8]>) -> TaskQueryState {
        match name {
            // 带 `ID`：查**注册表**（`task_names` / `task_slots`）。
            //
            // 注意不能用 `scheduler.nodes()`：那里只有主线 `@main` —— 命名任务
            // 的执行位由 `TASK` 写进 `task_slots`，名字进 `task_names`
            //（见 `exec/misc.rs` 的 TASK 分支）。引擎侧 `TASKINFO EXIST`
            // 查的也是这张按名索引的层表。
            Some(id) => {
                let exists = self.task_exists(id);
                let mut st = TaskQueryState {
                    exists,
                    present: exists,
                    ..TaskQueryState::default()
                };
                if let Some(sched) = self.scheduler.as_ref() {
                    if let Some(n) = sched.nodes().iter().find(|n| n.id == id) {
                        st.script_id = n.script_id;
                        st.enabled = n.enabled;
                        st.pc = n.pc;
                    }
                }
                if let Some(slot) = self.task_slots.get(id) {
                    st.if_nest = slot.if_nest.len();
                    st.gosub_nest = slot.frames.len().saturating_sub(1);
                    st.loop_nest = slot.loops.len();
                }
                st.file_name = self.ystl.as_ref().and_then(|t| {
                    t.get(st.script_id as u32)
                        .map(|e| e.name.clone())
                });
                st
            }
            // 无 `ID` = 当前任务（引擎 `0x4522b6` 取当前任务指针）。
            None => {
                let mut st = TaskQueryState {
                    exists: true,
                    present: true,
                    script_id: self.ctx.script_id,
                    pc: self.pc,
                    if_nest: self.if_nest.len(),
                    gosub_nest: self.frames.len().saturating_sub(1),
                    loop_nest: self.loops.len(),
                    ..TaskQueryState::default()
                };
                st.file_name = self
                    .ystl
                    .as_ref()
                    .and_then(|t| t.get(st.script_id as u32).map(|e| e.name.clone()));
                st
            }
        }
    }
}

/// 一次 `TASKINFO` 的目标状态快照。
#[derive(Debug, Default)]
struct TaskQueryState {
    /// 目标任务**存在**（`EXIST` 的答案）。
    exists: bool,
    /// 目标任务**被找到**（`ID` 命中，或是「无 ID = 当前任务」）。
    ///
    /// 与 `exists` 的区别：引擎在 `ID` 查不到时（`0x45278c`）按 `LET` 目标的
    /// 类型写**默认值**（INT → 0、FLT → 0.0、STR → 空串），并对**所有**查询
    /// 字段都用默认值 —— 而不是把「当前任务」的状态当成答案。
    present: bool,
    script_id: u16,
    enabled: bool,
    pc: usize,
    if_nest: usize,
    gosub_nest: usize,
    loop_nest: usize,
    file_name: Option<Vec<u8>>,
}
