//! 变量与查询族(从 `run_main` 巨型 match 拆出;语义逐行照搬,未改行为)。
//!
//! 拆分背景见 [`crate::exec`] 模块文档。**控制流约定**:方法体里不再出现
//! 「目标为外层 while」的 `continue` / `return Ok(VmSuspend…)` —— 分别替换为
//! `return Ok(Action::Keep)` / `return Ok(Action::Suspend(…))`,由 `run_main`
//! 的循环统一执行(语义 1:1)。

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, CommandSlot};
use yuris_value::{Value, VariableStore};

use crate::event::{GoOutcome, JumpKind, VmEvent, VmState, VmSuspend};
use crate::exec::Action;
use crate::util::{
    consume_declarations_into, extract_var_target, mstring, sjis_char_len, value_summary,
    value_truthy, varact_ascii_case, varact_byte_to_char, varact_char_to_byte, VarTarget,
};
use crate::{GosubFrame, GroupVm, IfFrame, LoopFrame, SavedExec, MAX_TASKS};
impl GroupVm {


    /// 处理 `Let`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_let(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        if g.window_count != 2 {
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                format!("LET 窗口数 {} != 2(语料恒 2)", g.window_count),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // 引擎 FUN_00443808:w0 = 左值,w1 = 右值表达式;
        // 复合赋值码 = w0.tag 的 B3 字节(0=赋值 1=+= … 8=^=)
        let code = ((windows[0].tag >> 24) & 0xFF) as u8;
        // **热路径(成果 182)**:LET 语料实测 345k 次/帧。窗口字节在
        // `eval_window_lvalue_slot` 内部按槽位取(零拷贝优先),避免这里
        // 持 `&self.ctx.script` 借用与 `&mut self` 冲突。
        let lval = match self.eval_window_lvalue_slot(&windows[0]) {
            Ok(l) => l,
            Err(e) => {
                self.set_error_state();
                return Ok(Action::Suspend(VmSuspend::Error(e.to_string())));
            }
        };
        // 帧局部变量(id = 声明命令 0x32/0x33/0x34/0x35/0x46)写入当前帧
        // locals;其余走全局(下方按 lval.var.id 分流)。
        // 基帧恒在(成果 56):帧局部 LET 恒有落点(引擎 depth 0 时
        // 写 record[0]),无「无帧」错误路径。
        let rhs = match self.eval_window_condition(Some(&windows[1]))? {
            Some(v) => v,
            None => {
                if let Some(reason) = self.halt_or_record(
                    self.pc,
                    g.command_type,
                    "LET 右值窗口为空",
                ) {
                    return Ok(Action::Suspend(VmSuspend::Error(reason)));
                }
                self.pc += 1;
                return Ok(Action::Keep);
            }
        };
        // 复合赋值需要当前值:左值窗内含 aload(引擎左值窗为
        // 引用+下标+aload 形态)→ 求值得到当前值;
        // 无 aload(纯标量)→ 标量路径直取。eval_window_condition
        // 的结尾 LValue 会被丢弃,这里允许其失败。
        let cur = self.store_current(&lval);
        let newv = match cur {
            Some(c) => match yuris_value::compound_assign(code, &c, &rhs) {
                Ok(v) => v,
                Err(e) => {
                    self.set_error_state();
                    return Ok(Action::Suspend(VmSuspend::Error(format!(
                        "{e}  [LET {} 复合码 {code}(tag {:08x}), 当前值 {}]",
                        lval.var.display(),
                        windows[0].tag,
                        value_summary(&c)
                    ))));
                }
            },
            None => {
                // code==0(纯赋值)允许向未初始化目标写入;复合码缺左值 → 挂起
                if code == 0 {
                    rhs.clone()
                } else if let Some(reason) = self.halt_or_record(
                    self.pc,
                    g.command_type,
                    "复合赋值目标无当前值(未声明?)",
                ) {
                    return Ok(Action::Suspend(VmSuspend::Error(reason)));
                } else {
                    self.pc += 1;
                    return Ok(Action::Keep);
                }
            }
        };
        // 目标:帧局部(id 0x32-0x46 族)→ 当前 GOSUB 帧 locals;否则全局
        let is_frame_local = matches!(lval.var.id, 0x32 | 0x33 | 0x34 | 0x35 | 0x46);
        // 输入系统变量（`@_MOUSE_L` / `@_KEY_ENTER` / `@_INP(code)` …）：
        // 引擎这些 id **无存储**，写入落到输入子系统（`FUN_00447014` 的
        // 状态钳制在 `write_input_var` 里）。
        let is_input_var = lval.var.space == yuris_value::VarSpace::At
            && self.is_input_var_id(lval.var.id);
        let store_res = if is_input_var {
            self.write_input_var(lval.var.id, &lval.indices, newv.clone())
        } else if is_frame_local {
            match self.frames.last_mut() {
                Some(fr) if lval.indices.is_empty() => {
                    fr.locals.set(&lval.var, newv.clone());
                    Ok(())
                }
                Some(fr) => fr.locals.set_elem(&lval.var, &lval.indices, newv.clone()),
                None => Err(Error::format(
                    "LET 帧局部但无帧(基帧恒在,不应发生;成果 56)",
                )),
            }
        } else if lval.indices.is_empty() {
            self.store.set(&lval.var, newv.clone());
            Ok(())
        } else {
            self.store.set_elem(&lval.var, &lval.indices, newv.clone())
        };
        if let Err(e) = store_res {
            self.set_error_state();
            return Ok(Action::Suspend(VmSuspend::Error(e.to_string())));
        }
        // 变量写流水(`YURIS_TRACE_LET=<id,id,…>` 门控):打 s/pc/左值/新值。
        // boot 淡变循环取证的仪器(成果 159 线);量小,只挑 id 时开。
        if let Some(f) = let_trace_filter() {
            let hit = f.iter().any(|x| *x == lval.var.id as i64);
            if hit {
                let idx_dbg: Vec<String> = lval
                    .indices
                    .iter()
                    .map(|i| format!("{i:?}"))
                    .collect();
                yuris_log::trace_forced!(
                    "[let] s{} pc{} {}[{}] -> {} (frame_local={})",
                    self.ctx.script_id,
                    self.pc,
                    lval.var.id,
                    idx_dbg.join(","),
                    value_summary(&newv),
                    is_frame_local
                );
            }
        }
        self.push_event(VmEvent::GroupExecuted {
            pc: self.pc,
            command: g.command_type,
            condition: Some(newv),
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `VarInfo`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_varinfo(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_004550a0(**Confirmed**,2026-09-03 勘误):
        // SET(槽0)/LET(槽1) = 变量引用槽(kind 2 延迟求值),非操作!
        //   旧实现把 SET 误判为写回操作挂起 → 端到端卡死,已修正。
        // 操作槽(引擎 if-else 链按槽号升序,第一个非零者生效):
        //   TYPE(2)/STRTYPE(3)/DIMNUM(4)/DIMSIZE(5..12 = 第 1..8 维)/
        //   LENGTH(13)/SEARCH(14)/STRFIRST(15)/SJISCODE(16)
        // 结果 → 接收器 = **写入 LET 引用目标**(STRTYPE 分支直接证据:
        //   判定 1/2 写入 LET 引用的下标元素;无 LET 槽 → 仅事件记录)。
        // 全部操作槽为 0 → 引擎 fallback = LENGTH(SET 串字节长)。
        let mut set_target: Option<VarTarget> = None;
        let mut let_target: Option<VarTarget> = None;
        let mut op_slot: Option<(u8, Value)> = None;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut pending: Vec<(u8, yuris_format::ystb::CommandSlot)> = Vec::new();
        // 操作槽求值全量保留(SEARCH 需要 key/起始下标等参数值)
        let mut slot_vals: Vec<(u8, Value)> = Vec::new();
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            match slot {
                0 | 1 => pending.push((slot, w.clone())),
                _ => {
                    let Some(v) = self.eval_window_condition(Some(w))? else {
                        continue;
                    };
                    let enabled = matches!(&v, Value::Int(n) if *n != 0);
                    if enabled && op_slot.is_none() {
                        op_slot = Some((slot, v.clone()));
                    }
                    evaluated.push((slot, value_summary(&v)));
                    slot_vals.push((slot, v));
                }
            }
        }
        // 引用槽窗口解码失败 = 结构异常,诚实挂起
        for (slot, w) in pending {
            let bytes = self
                .ctx
                .script
                .window_bytes_pooled_copy(&w)
                .ok_or_else(|| Error::format("VARINFO 引用窗越过池尾"))?;
            let target = extract_var_target(&bytes)?;
            match slot {
                0 => set_target = Some(target),
                _ => let_target = Some(target),
            }
        }
        // 执行查询(未实现/结构异常 → strict 挂起 / trace 记录后跳过)
        let query = op_slot.as_ref().map(|(s, _)| *s);
        let result = match self
            .exec_varinfo_query(query, set_target.as_ref(), &slot_vals, &mut evaluated)
        {
            Ok(r) => r,
            Err(e) => {
                if let Some(reason) =
                    self.halt_or_record(self.pc, g.command_type, e.to_string())
                {
                    return Ok(Action::Suspend(VmSuspend::Error(reason)));
                }
                self.pc += 1;
                return Ok(Action::Keep);
            }
        };
        // 写 LET 目标
        if let (Some(result), Some(target)) = (result, &let_target) {
            self.write_var_target(target, result)?;
        }
        if let Some(t) = &let_target {
            evaluated.push((255u8, format!("→ {}", t.summary())));
        }
        self.push_event(VmEvent::VarQuery {
            pc: self.pc,
            command: g.command_type,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `VarAct`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_varact(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00453178(反编译逐分支,2026-09-03 勘误后):
        // 处理器头部把槽0(SET)引用记为对象、槽1(LET)引用记为目标。
        // 分支结构(全 if 独立判断,非互斥 else):
        // - a3(CUT) 分支:POS(4)/LENGTH(5) 联合计算字符区间 [POS,
        //   POS+LENGTH) 的字节偏移,拷贝子串 → FUN_0045b9f8(写 LET 目标);
        //   对象为空串时步进循环不执行 → 结果 = 空串(不报错)。
        // - 顶层 `if (a6==0)` 为真(无 TYPE 槽)才进上述各分支;a6≠0 是
        //   INT/FLT/STR 类型转换分支(收尾直接接收器,无 LET 也可)。
        // - DIMSIZE(13)/PUSH(14)/POP(15) 分支无接收器写回(直接改数组
        //   描述符);全语料直方图:[0,13]=131、[0,15]=113、[0,14]=93
        //   (全部无 LET 槽,与「直接改数组」自洽)。
        // - 语料 [0,1,3,4,5]=15 组 = CUT+POS+LENGTH 联合(截取子串)。
        // 未实现分支:诚实挂起(不猜)。
        let mut set_target: Option<VarTarget> = None;
        let mut let_target: Option<VarTarget> = None;
        // (slot, B3 算符, 槽值);B3 = tag 高字节(0=赋值/1=加/…,
        // DIMSIZE 分支经 FUN_00425224 消费;成果 59)
        let mut ops: Vec<(u8, u8, Value)> = Vec::new();
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut pending: Vec<(u8, yuris_format::ystb::CommandSlot)> = Vec::new();
        let cmd_idx = g.command_type;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            match slot {
                0 | 1 => pending.push((slot, w.clone())),
                _ => {
                    let Some(v) = self.eval_window_condition(Some(w))? else {
                        continue;
                    };
                    if slot >= 2 {
                        let b3 = ((w.tag >> 24) & 0xff) as u8;
                        ops.push((slot, b3, v.clone()));
                    }
                    evaluated.push((slot, value_summary(&v)));
                }
            }
        }
        for (slot, w) in pending {
            let bytes = self
                .ctx
                .script
                .window_bytes_pooled_copy(&w)
                .ok_or_else(|| Error::format("VARACT 引用窗越过池尾"))?;
            let t = extract_var_target(&bytes)?;
            if slot == 0 {
                set_target = Some(t);
            } else {
                let_target = Some(t);
            }
        }
        // **INIT/G_* 恢复组**在 SET 检查之前(引擎判定链同序;
        // 旁线仓库 Round20 Confirmed:恢复分支不消费 SET 槽,
        // `VARACT INIT=1 G_INT3=1` 无 SET 合法,否则引擎判死指令)。
        if set_target.is_none()
            && ops.iter().any(|(s, _, _)| {
                *s == self.slot_of_param(cmd_idx, "INIT").unwrap_or(255)
            })
        {
            const G_SLOTS: &[(&str, u8)] = &[
                ("G_INT", 1u8),
                ("G_INT2", 1u8),
                ("G_INT3", 1u8),
                ("G_INT4", 1u8),
                ("G_FLT", 2u8),
                ("G_FLT2", 2u8),
                ("G_FLT3", 2u8),
                ("G_FLT4", 2u8),
                ("G_STR", 3u8),
                ("G_STR2", 3u8),
                ("G_STR3", 3u8),
                ("G_STR4", 3u8),
            ];
            let g_slots: Vec<(&str, u8, Option<u8>)> = G_SLOTS
                .iter()
                .map(|(name, ty)| (*name, *ty, self.slot_of_param(cmd_idx, name)))
                .collect();
            let by_slot = |want: u8| {
                ops.iter()
                    .find(|(s, _, _)| *s == want)
                    .map(|(_, _, v)| v.clone())
            };
            let restored = self.varact_restore_globals(&by_slot, &g_slots)?;
            self.push_event(VmEvent::Subsystem {
                pc: self.pc,
                command: g.command_type,
                evaluated: vec![(
                    254u8,
                    format!("INIT 全局恢复 {} 条(掩码=G_* 槽)", restored),
                )],
            });
            self.push_event(VmEvent::GroupExecuted {
                pc: self.pc,
                command: g.command_type,
                condition: None,
            });
            self.pc += 1;
            return Ok(Action::Keep);
        }
        // 缺 SET 槽:严格模式照旧报错;非严格模式按项目既有契约
        // (`halt_or_record`)记事件并跳过本组 —— 让 `--lenient` 能继续
        // 跑到「画面」那一步做对拍,而不是整台 VM 停摆。
        let Some(target) = set_target else {
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                format!(
                    "VARACT 无 SET 引用槽(引擎必填) s{} pc={} 槽清单={:?}",
                    self.ctx.script_id,
                    self.pc,
                    windows.iter().map(|w| w.tag & 0xff).collect::<Vec<_>>()
                ),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
            self.pc += 1;
            return Ok(Action::Keep);
        };
        // 对象当前值(STR 操作分支的输入;引擎经 FUN_0044d4e8 读取)
        let obj = self.read_var_target(&target)?;
        // 引擎 if-else 链按槽号独立 if(可多槽同时启用);逐个处理
        let mut result: Option<Value> = None;
        let mut unimplemented: Vec<u8> = Vec::new();
        let has_range_op = ops.iter().any(|(s, _, _)| matches!(s, 2 | 3));
        for (slot, b3, v) in &ops {
            match slot {
                13 => {
                    // DIMSIZE:new_len = 算符(B3)以**当前元素个数**
                    // 为基(引擎 00453178 DIMSIZE 分支,00425224 唯一
                    // 调用点:op 0=赋值/1=加/2=减/3=乘/4=除,基值 =
                    // desc+4;负值 → 报错 0x…b60;结果 == cur 不
                    // resize)。语料惯用法 DIMSIZE(+=1) = 压栈
                    // (s47 g42 等,成果 59)。
                    let r = match &target {
                        VarTarget::Scalar(r) => r,
                        VarTarget::Indexed(r, _) => r,
                    };
                    let cur = self
                        .store
                        .array_dims(r)
                        .and_then(|d| d.first().copied())
                        .ok_or_else(|| {
                            Error::format(format!(
                                "VARACT DIMSIZE 目标非数组(引擎 desc+2==1) s{} pc={}",
                                self.ctx.script_id, self.pc
                            ))
                        })?;
                    let newv = yuris_value::compound_assign(
                        *b3,
                        &Value::Int(cur as i64),
                        v,
                    )?;
                    let new_len = match newv {
                        Value::Int(n) if n >= 0 => n as usize,
                        other => {
                            return Err(Error::format(format!(
                                "VARACT DIMSIZE 需非负 int,得到 {other:?}"
                            )))
                        }
                    };
                    if new_len != cur as usize {
                        self.resize_array_target(&target, new_len)?;
                    }
                }
                4 | 5 if has_range_op => {
                    // POS/LENGTH 是 CUT/COPY 分支的字符区间参数
                    // (引擎 a2/a3 守卫内消费),不单独产生结果
                }
                slot @ (7 | 9) => {
                    // UPPER(7)/LOWER(9):引擎 FUN_004546cc/FUN_00464dac
                    // (各 57B,Ghidra 2026-09-04):SJIS 感知 ASCII 大小写
                    // 原地转换 —— 步进表 DAT_0059b0c0==1(双字节首区
                    // 0x81-0x9F/0xE0-0xEF)连跳 2 字节;单字节 'a'-'z'→
                    // -0x20(UPPER) / 'A'-'Z'→+0x20(LOWER),其余原样。
                    // 槽值(push 1)= 使能标志,转换本身不消费(语料
                    // s190 g267 LOWER=1;Likely)。接收 = LET 目标。
                    let bytes = match &obj {
                        Value::Str(b) => b.clone(),
                        other => {
                            return Err(Error::format(format!(
                                "VARACT 槽 {slot} 对象非 STR: {other:?}"
                            )))
                        }
                    };
                    result = Some(Value::Str(varact_ascii_case(
                        &bytes,
                        *slot == 7,
                    )));
                }
                slot @ (2 | 3) => {
                    // slot3 COPY:截取字符区间 [POS-1, POS-1+LENGTH)
                    // slot2 CUT :写出切除该区间后的剩余部分
                    // (引擎 CMDH_00453178 两分支:按 SJIS 步进表定位
                    // 字节偏移。守卫语义:POS∈{0,1}→首字符(引擎对值
                    // 0/1 有专门短路);LENGTH=0→空区间;对象为空串 →
                    // 步进循环不执行 → 结果=空串,**不报错**。
                    // 报错(0x1d4ca/0x1d4d4)仅在非空串上步进越过缓冲尾,
                    // 本实现以「钳制到字符数」近似(引擎此时也报错;
                    // 真实语料该路径未观测到,等级 Likely)。
                    let bytes = match &obj {
                        Value::Str(b) => b.clone(),
                        other => {
                            return Err(Error::format(format!(
                                "VARACT 槽 {slot} 对象非 STR: {other:?}"
                            )))
                        }
                    };
                    let pos_ch = self.varact_slot_usize(ops.as_slice(), 4)?;
                    let len_ch = self.varact_slot_usize(ops.as_slice(), 5)?;
                    if bytes.is_empty() {
                        // 空串:两循环都不步进 → COPY=""、CUT=""
                        result = Some(Value::Str(Vec::new()));
                    } else {
                        let start_ch = pos_ch.saturating_sub(1);
                        let start = varact_char_to_byte(&obj, start_ch)
                            .ok_or_else(|| {
                                let pc = self.pc;
                                let blen = bytes.len();
                                let sid = self.ctx.script_id;
                                let hex: String = bytes
                                    .iter()
                                    .take(48)
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                Error::format(format!(
                                    "VARACT 槽 {slot} POS={pos_ch} 越界(引擎 0x1d4ca 同族; s{sid} pc={pc} 目标={target:?} 串字节={blen} ops={ops:?} hex={hex})"
                                ))
                            })?;
                        let end = varact_char_to_byte(&obj, start_ch + len_ch)
                            .ok_or_else(|| {
                                let pc = self.pc;
                                let blen = bytes.len();
                                let sid = self.ctx.script_id;
                                Error::format(format!(
                                    "VARACT 槽 {slot} LENGTH={len_ch} 越界(引擎 0x1d4d4 同族; s{sid} pc={pc} 串字节={blen} POS={pos_ch} ops={ops:?})"
                                ))
                            })?;
                        result = Some(match slot {
                            3 => Value::Str(bytes[start..end].to_vec()),
                            _ => {
                                let mut out = bytes[..start].to_vec();
                                out.extend_from_slice(&bytes[end..]);
                                Value::Str(out)
                            }
                        });
                    }
                }
                14 | 15 => {
                    // PUSH(14)/POP(15):数组元素插入/删除(带移位;
                    // 引擎 00453178 PUSH/POP 分支汇编级 Confirmed):
                    // - 前提:目标 1 维数组(desc+2==1,否则 0x879b80/
                    //   0x879be0 报错);槽值 = 操作位置 pos(64 位,
                    //   钳制:pos >= count-1 时只写末位)。
                    // - PUSH:`for i in (pos+1..count).rev(): arr[i] =
                    //   arr[i-1]`(后移),`arr[pos] = 类型默认值`
                    //   (INT 0 / FLT 0.0[0x579ca8] / STR ""[0x87899c])。
                    // - POP:`for i in pos..count-1: arr[i] = arr[i+1]`
                    //   (前移),`arr[count-1] = 默认值`。
                    // 无 LET 写回、无 resize(desc+4 不变)。
                    let r = match &target {
                        VarTarget::Scalar(r) => r,
                        // 带下标 = 非裸数组引用(PUSH/POP 须裸引用)
                        VarTarget::Indexed(_, _) => {
                            return Err(Error::format(format!(
                                "VARACT PUSH/POP 目标须裸数组引用(引擎 desc+2==1)\
                                 s{} pc={}",
                                self.ctx.script_id, self.pc
                            )));
                        }
                    };
                    let Some(bounds) = self.store.array_dims(r) else {
                        return Err(Error::format(format!(
                            "VARACT PUSH/POP 目标非数组 s{} pc={}",
                            self.ctx.script_id, self.pc
                        )));
                    };
                    if bounds.len() != 1 {
                        return Err(Error::format(format!(
                            "VARACT PUSH/POP 要求 1 维数组(引擎 desc+2==1),\
                             得到 {} 维",
                            bounds.len()
                        )));
                    }
                    let count = bounds[0] as usize;
                    let pos = match v {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        other => {
                            return Err(Error::format(format!(
                                "VARACT PUSH/POP 位置须非负 int,得到 {other:?}"
                            )))
                        }
                    };
                    if count == 0 {
                        return Err(Error::format(
                            "VARACT PUSH/POP 空数组(引擎 desc+4==0)",
                        ));
                    }
                    let default = match self.store.array_elem_type(r).unwrap() {
                        yuris_value::ElemType::Int => Value::Int(0),
                        yuris_value::ElemType::Float => Value::Float(0.0),
                        yuris_value::ElemType::Str => Value::Str(Vec::new()),
                    };
                    if *slot == 14 {
                        // PUSH:pos 钳到 count-1;后移 + 插入默认值
                        let pos = pos.min(count - 1);
                        for i in ((pos + 1)..count).rev() {
                            let v = self.store.get_elem(r, &[(i - 1) as i64])?.clone();
                            self.store.set_elem(r, &[i as i64], v)?;
                        }
                        self.store.set_elem(r, &[pos as i64], default)?;
                    } else {
                        // POP:pos >= count-1 只尾置默认;否则前移 + 尾置默认
                        if pos < count.saturating_sub(1) {
                            for i in pos..(count - 1) {
                                let v = self
                                    .store
                                    .get_elem(r, &[(i + 1) as i64])?
                                    .clone();
                                self.store.set_elem(r, &[i as i64], v)?;
                            }
                        }
                        self.store
                            .set_elem(r, &[(count - 1) as i64], default)?;
                    }
                }
                other_slot => unimplemented.push(*other_slot),
            }
        }
        // 结果接收:引擎 a3 分支收尾写 LET 引用的变量(有 LET 槽才写;
        // DIMSIZE/PUSH/POP 无 LET 也不写)
        if let Some(v) = result {
            let lt = let_target.clone().ok_or_else(|| {
                Error::format("VARACT CUT 有结果但无 LET(1) 引用槽(引擎必填)")
            })?;
            self.write_var_target(&lt, v)?;
            evaluated.push((255u8, format!("→ {}", lt.summary())));
        }
        if !unimplemented.is_empty() {
            let slots: Vec<String> =
                unimplemented.iter().map(|s| format!("{s}")).collect();
            if let Some(reason) = self.halt_or_record(
                self.pc,
                g.command_type,
                format!(
                    "VARACT 写操作槽 [{slots:?}] 未实现(不猜) s{} pc={}",
                    self.ctx.script_id, self.pc
                ),
            ) {
                return Ok(Action::Suspend(VmSuspend::Error(reason)));
            }
        }
        self.push_event(VmEvent::VarQuery {
            pc: self.pc,
            command: g.command_type,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }

    /// 处理 `LabelInfo`(从 `run_main` 拆出;语义逐行照搬)。
    pub(crate) fn cmd_labelinfo(&mut self, g: CommandGroup, windows: &[CommandSlot]) -> Result<Action> {
        // 引擎 CMDH_00443674(Confirmed,402B 小函数):
        // '#'(槽0,k1)=标签名;LET(槽1)=写回目标(可带下标);
        // EXIST(槽2)非零 → FUN_0045124c(murmur2 查标签表):
        //   命中 → LET 写 1;未命中 → 写 0(INT 写 8B 零/FLT 写 0.0)。
        // 未启用 EXIST → 仅拷名(no-op)。
        let mut label: Option<String> = None;
        let mut let_target: Option<VarTarget> = None;
        let mut exist = false;
        let mut evaluated: Vec<(u8, String)> = Vec::new();
        let mut pending_let: Option<yuris_format::ystb::CommandSlot> = None;
        for w in windows {
            if w.len == 0 {
                continue;
            }
            let slot = (w.tag & 0xff) as u8;
            match slot {
                0 => {
                    if let Some(v) = self.eval_window_condition(Some(w))? {
                        if let Value::Str(_) = v {
                            label = Some(v.str_as_string());
                        }
                        evaluated.push((slot, value_summary(&v)));
                    }
                }
                1 => pending_let = Some(w.clone()),
                _ => {
                    let v = self.eval_window_condition(Some(w))?;
                    if slot == 2 {
                        exist = matches!(&v, Some(Value::Int(n)) if *n != 0);
                    }
                    if let Some(v) = &v {
                        evaluated.push((slot, value_summary(v)));
                    }
                }
            }
        }
        if let Some(w) = pending_let {
            let bytes = self
                .ctx
                .script
                .window_bytes_pooled_copy(&w)
                .ok_or_else(|| Error::format("LABELINFO LET 引用窗越过池尾"))?;
            let_target = Some(extract_var_target(&bytes)?);
        }
        if exist {
            let hit = label
                .as_deref()
                .and_then(|name| self.labels.get(name.as_bytes()))
                .is_some();
            // 诊断(`YURIS_TRACE_LABELS=1`):`es._label.info.exist` 是游戏的
            // **标签存在性门控** —— 分派链里常见的形态是
            // `IF(es._label.info.exist("X")) { GOSUB X } else { … }`。
            // 结果被静默写回变量时,「为什么走了这条分支」从事件流完全看不出;
            // 与 GO/GOSUB 的 MISS 日志同一开关,排障时一起打开。
            if std::env::var_os("YURIS_TRACE_LABELS").is_some() {
                yuris_log::trace_forced!(
                    "[label] s{} pc{} EXIST {:?} = {} ({})",
                    self.ctx.script_id,
                    self.pc,
                    label.as_deref().unwrap_or("<无名字窗>"),
                    hit as i64,
                    if hit { "命中" } else { "未命中" }
                );
            }
            if let Some(target) = &let_target {
                self.write_var_target(target, Value::Int(hit as i64))?;
                evaluated.push((255u8, format!("EXIST({label:?})={} → {}", hit as i64, target.summary())));
            }
        }
        self.push_event(VmEvent::VarQuery {
            pc: self.pc,
            command: g.command_type,
            evaluated,
        });
        self.pc += 1;
        return Ok(Action::Keep);
    }
}

/// `YURIS_TRACE_LET=<id逗号表>`(高等级门控,缺省 None = 静默)。
fn let_trace_filter() -> Option<Vec<i64>> {
    static GATE: std::sync::OnceLock<Option<Vec<i64>>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| {
        std::env::var("YURIS_TRACE_LET")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.split(',')
                    .filter_map(|x| x.trim().parse::<i64>().ok())
                    .collect()
            })
    })
    .clone()
}
