//! 表达式求值器:对单个指令窗口做栈式执行。
//!
//! 只执行**已证实**语义(`docs/opcode/opcode-table.md` 第二版);
//! 求值/存储语义未证实处显式报错:
//! - `PushVarRef` / `PushVarIndexed` / `ArrayLoad`:赋值与数组存储模型 Unknown
//! - `Unknown` 指令:走 [`yuris_core::Error::UnresolvedOpcode`]
//! - 未定义变量读取:报错(引擎默认值行为 Unknown,不猜)

use std::collections::HashMap;
use std::sync::Arc;

use yuris_core::{Error, Result};
use yuris_value::{Value, VarRef, VarSpace, VariableStore};

use crate::insn::{BinOp, Cast, Insn, Instruction};

/// 引擎输入系统变量的求值快照（由 VM 每帧注入）。
///
/// ## 为什么放在 `yuris-script`
///
/// `yuris-vm` 依赖本 crate（VM → 表达式求值），输入状态机 `yuris-input`
/// 是 L6（平台层）。求值器需要在 `load_auto`/`ArrayLoad` 里读 `@_MOUSE_X`、
/// `@_INP(code)` 等，而这条路径不经过 VM 的 `read_var_value`。把**纯数据**
/// 定义在这里，VM 负责从输入层填快照，依赖方向保持单向。
///
/// 语义：id 空间与 YSCM 系统变量表一致（SDK 0.495：133/138 = 光标，
/// 144 = `_INP`，145/146 = `_KEYLAST`/`_MOUSELAST`，147..=166 鼠标键，
/// 167..=275 键盘键，276..=280 手柄，281.. 计数与 `_CODE_*` 常量）。
#[derive(Debug, Clone, Default)]
pub struct InputVars {
    /// 本帧声明为引擎输入系统变量的 id 集合（未声明的 id 走普通变量存储）。
    pub claimed: std::collections::HashSet<u16>,
    /// 标量系统变量。
    pub scalars: HashMap<u16, i64>,
    /// 数组型输入变量（`_PAD_UP/DOWN/LEFT/RIGHT`、`_PAD_BUTTON`）。
    pub pad: HashMap<u16, Vec<i64>>,
    /// `_INP` 数组（下标 = 按钮码）。
    pub inp: Vec<i64>,
}

impl InputVars {
    /// `_INP(code)`；越界/负值 → 0（引擎边界外读 = 类型默认值）。
    pub fn inp_at(&self, code: i64) -> i64 {
        if code < 0 {
            return 0;
        }
        self.inp.get(code as usize).copied().unwrap_or(0)
    }

    /// 手柄数组扁平下标（与 `yuris-vm::InputSnapshot::pad_index` 同式）。
    pub fn pad_index(id: u16, indices: &[i64]) -> Option<usize> {
        match id {
            // `_PAD_UP(n)`：无下标按手柄 0（引擎参数必填；不因缺省报错）。
            276..=279 => Some(indices.first().map(|p| (*p).max(0) as usize).unwrap_or(0)),
            280 => match indices {
                [p, b, ..] => Some(((*p).max(0) as usize) * 32 + (*b).clamp(0, 31) as usize),
                [p] => Some(((*p).max(0) as usize) * 32),
                _ => Some(0),
            },
            _ => None,
        }
    }

    /// 带下标的读；`None` = 该 id 不由输入层提供。
    pub fn read(&self, id: u16, indices: &[i64]) -> Option<i64> {
        if !self.claimed.contains(&id) {
            return None;
        }
        if id == 144 {
            return Some(self.inp_at(indices.first().copied().unwrap_or(0)));
        }
        if let Some(v) = self.pad.get(&id) {
            let i = Self::pad_index(id, indices)?;
            return Some(v.get(i).copied().unwrap_or(0));
        }
        // 已声明但未展开的输入 id：引擎里始终有值，缺省 0。
        Some(self.scalars.get(&id).copied().unwrap_or(0))
    }

    /// 无下标读（引擎 0x48）：标量直读；数组取 **0 号元素**（引擎
    /// `load_auto` 语义）。未声明的 id → `None`。
    pub fn read_auto(&self, id: u16) -> Option<i64> {
        if !self.claimed.contains(&id) {
            return None;
        }
        if let Some(v) = self.scalars.get(&id) {
            return Some(*v);
        }
        if id == 144 {
            return Some(self.inp_at(0));
        }
        if let Some(v) = self.pad.get(&id) {
            return Some(v.first().copied().unwrap_or(0));
        }
        Some(0)
    }

    /// 该 id 是否由输入层提供。
    pub fn contains(&self, id: u16) -> bool {
        self.claimed.contains(&id)
    }
}

/// 栈式求值器。
pub struct Evaluator<'s> {
    store: &'s mut VariableStore,
    /// **求值栈复用缓冲**(成果 183)。
    ///
    /// `eval_slots` 每个窗口都 `Vec::new()`;LET 实测 345k 次/帧 × 2 窗
    /// = 69 万次堆分配。该缓冲由调用方(VM)持有并复用,稳态零分配。
    /// 空 Vec 时行为与旧实现完全相同。
    pub slot_buf: Vec<Slot>,
    /// 当前 GOSUB 帧局部(可选):PushVar 优先查帧局部,再查全局。
    /// 帧局部 id 空间与全局分离(引擎:声明命令 id + 帧内槽)。
    locals: Option<&'s VariableStore>,
    /// @48 系统变量当前值 = 内层 LOOP 迭代计数(引擎 sysvar case 0x30,
    /// 00447ebc:读 obj+0x244 嵌套栈顶记录 +0x10;LOOP 置 1、LOOPEND +1、
    /// 无活动循环 = 0)。由 VM 在每次求值前注入。
    loop_counter: i64,
    /// 光标逻辑坐标(P9.2;引擎 sysvar case 0x85/0x8a = 窗口对象
    /// +0x2ac/+0x2b0,每帧 GetCursorPos→ScreenToClient→逻辑缩放,
    /// recon_00403f2c_input_poll.c)。由 VM 注入;(0,0) = 无输入面。
    cursor: (i64, i64),
    /// `@1[1]` 系统变量当前值 = **引擎毫秒计时**(引擎 sysvar case 0x01,
    /// `docs/engine/command-layer.md` §9c:`00447ebc`/`00447d4c` 的 id<1000
    /// 二次派发,timeGetTime 派生计时)。
    ///
    /// 语料用法恒为**时间差**:`@1[1] - 起始` 再除时长 = 进度;全语料 1504 处,
    /// **只用下标 1**。缺它会怎样:本游戏 `es.CGFADEOUT`(s8 pc470 的
    /// `LOOP -1`)以「alpha == 256」为出口,alpha 由 `(@1[1]-起始)*256/时长`
    /// 得出 —— 计时恒 0 ⇒ alpha 恒 0 ⇒ `LOOPBREAK` 永不命中 ⇒ 启动链卡死
    /// 在淡入淡出循环(实机:VM 停在 s8 pc=511,画面永远出不来)。由 VM 在
    /// 每次求值前注入(读即计算、无存储;引擎同)。
    timer_ms: i64,
    /// `@115/@116` 系统变量 = **屏幕宽/高**(引擎 sysvar case 0x73/0x74,
    /// `00447ebc`;手册「システム変数」= `_SCREEN_SIZE_X/Y`)。
    ///
    /// 真值 = 游戏自身 `yscfg.ybn` 的 `screen_width/height`,由 VM 每次求值前
    /// 注入 —— 与 `WINDOWINFO NO=0 SX/SY` 应答**同源**
    /// (成果 123;T20 修复前本模块写死 oracle 环境的 1920×1080,
    /// 与 WINDOWINFO 路径(本作 1100×825)矛盾)。
    ///
    /// `None` = 未注入(VM 未提供分辨率)→ 读取**报错**,不猜。
    screen: Option<(i64, i64)>,
    /// 输入系统变量快照（VM 每帧注入；未注入 = 无输入面）。
    input: Option<Arc<InputVars>>,
    /// 调试:打印变量装载(默认 false)。
    pub verbose: bool,
}

impl<'s> Evaluator<'s> {
    /// 绑定变量存储(无帧局部)。
    pub fn new(store: &'s mut VariableStore) -> Self {
        Self {
            store,
            locals: None,
            slot_buf: Vec::new(),
            loop_counter: 0,
            cursor: (0, 0),
            timer_ms: 0,
            screen: None,
            input: None,
            verbose: false,
        }
    }

    /// 绑定变量存储 + 帧局部。
    pub fn with_locals(
        store: &'s mut VariableStore,
        locals: Option<&'s VariableStore>,
    ) -> Self {
        Self {
            store,
            locals,
            slot_buf: Vec::new(),
            loop_counter: 0,
            cursor: (0, 0),
            timer_ms: 0,
            screen: None,
            input: None,
            verbose: false,
        }
    }

    /// 注入 `@1[1]`(引擎毫秒计时)。
    pub fn set_timer_ms(&mut self, ms: i64) {
        self.timer_ms = ms;
    }

    /// 注入 @48 当前值(内层 LOOP 迭代计数;无循环 = 0)。
    pub fn set_loop_counter(&mut self, v: i64) {
        self.loop_counter = v;
    }

    /// 注入光标逻辑坐标(@133/@138;引擎 obj+0x2ac/+0x2b0)。
    pub fn set_cursor(&mut self, x: i64, y: i64) {
        self.cursor = (x, y);
    }

    /// 注入 `@115/@116`(屏幕宽/高;真值 = 游戏自身 yscfg 分辨率,
    /// 与 `WINDOWINFO SX/SY` 应答同源)。
    pub fn set_screen_size(&mut self, w: i64, h: i64) {
        self.screen = Some((w, h));
    }

    /// 注入引擎输入系统变量快照（`@_MOUSE_X` / `@_INP` / `@_KEY_*` …）。
    pub fn set_input(&mut self, input: Arc<InputVars>) {
        self.input = Some(input);
    }

    /// `@115/@116` 当前值;未注入 → 报错(不猜)。
    fn screen_dims(&self, id: u16) -> Result<Value> {
        let (w, h) = self.screen.ok_or_else(|| {
            Error::format(format!(
                "@{id} 屏幕尺寸未注入(VM 未提供 yscfg 分辨率;不猜)"
            ))
        })?;
        Ok(Value::Int(if id == 115 { w } else { h }))
    }

    /// 解码并执行整个窗口,返回**剩余栈**(自底向上)。
    ///
    /// 正常的单值窗口余栈恰为 1 个元素;含组分隔的参数列表窗口可能为空。
    pub fn eval_window(&mut self, window: &[u8]) -> Result<Vec<Value>> {
        let instrs = crate::decode_window(window)?;
        self.eval_instructions(&instrs)
    }

    /// 窗口的**左值引用形态**是否可判:栈上含未解引用槽。
    ///
    /// **用途(成果 173)**:诊断/通用参数记录循环会对**所有**窗口调
    /// `eval_window_condition`;而 `MATH3D`/`VARINFO` 的引用槽是
    /// 裸左值(`0x76`),按值求值必然失败 —— 那是**用法不同**,不是错误。
    /// 调用方据此跳过,避免把正常引用窗记成「求值失败」。
    pub fn window_is_lvalue(&mut self, window: &[u8]) -> bool {
        let Ok(instrs) = crate::decode_window(window) else {
            return false;
        };
        matches!(self.eval_slots(&instrs), Ok(slots) if slots.iter().any(|s| matches!(s, Slot::LValue(_))))
    }

    /// 执行已解码的指令序列,返回剩余栈。
    pub fn eval_instructions(&mut self, instrs: &[Instruction]) -> Result<Vec<Value>> {
        let slots = self.eval_slots(instrs)?;
        slots
            .into_iter()
            .map(|s| {
                s.into_value().ok_or_else(|| {
                    Error::Unimplemented(
                        "窗口以左值引用结尾(LET 语境需 eval_lvalue_window)",
                    )
                })
            })
            .collect()
    }

    /// 执行指令序列,返回完整槽位栈(LET / ArrayLoad 语境)。
    ///
    /// 槽位模型(引擎运行期变量处理器 0x48/0x56/0x76/0x29,`command-layer.md` §5b):
    /// - 0x56 → 平行**左值引用栈**(不装值);0x76 → 延迟占位(细节 Likely)
    /// - 0x29 → 从最近的左值引用点收集其上全部纯值项作下标表,行主序装载元素
    pub fn eval_slots(&mut self, instrs: &[Instruction]) -> Result<Vec<Slot>> {
        // 返回值必须**属于调用方**,无法复用共享缓冲 —— 这里把缓冲搬出、
        // 求值后把它交给调用方(不搬回)。真正的零分配入口是
        // [`Self::eval_last_value`](热路径:只取末值)。
        let mut stack = std::mem::take(&mut self.slot_buf);
        self.eval_slots_fill(instrs, &mut stack)?;
        Ok(stack)
    }

    /// **热路径专用**:执行指令序列,只返回**末值**,缓冲原地复用(零分配)。
    ///
    /// 为什么需要:条件窗口求值每帧约 70 万次,调用方几乎都只要「最后一个
    /// 值」(`eval_window_condition` 的 `stack.next_back()`)。旧实现每次
    /// `eval_instructions` 都 `collect::<Vec<Value>>()` 再丢前 n-1 个元素。
    ///
    /// 语义与 `eval_instructions(...)?.pop()` 完全一致:
    /// - 空栈 → `Ok(None)`
    /// - 末槽是左值引用 → `Err(Unimplemented(…))`(与旧路径同文本)
    pub fn eval_last_value(&mut self, instrs: &[Instruction]) -> Result<Option<Value>> {
        let mut stack = std::mem::take(&mut self.slot_buf);
        let out = self.eval_last_value_in(instrs, &mut stack);
        self.slot_buf = std::mem::take(&mut stack);
        out
    }

    /// **热路径**:用**调用方持有**的缓冲求值,只返回末值。
    ///
    /// 为什么需要调用方持有:`Evaluator` 是每次求值临时构造的（借用
    /// `&mut VariableStore`），它的内部缓冲**活不过一次调用** ⇒
    /// 自动复用无效，每次仍是一次 `Vec` 分配。把缓冲提到长寿命对象
    /// （[`crate::GroupVm`]）上，才能真正做到稳态零分配。
    pub fn eval_last_value_in(
        &mut self,
        instrs: &[Instruction],
        stack: &mut Vec<Slot>,
    ) -> Result<Option<Value>> {
        self.eval_slots_fill(instrs, stack)?;
        match stack.pop() {
            Some(Slot::Val(v)) => Ok(Some(v)),
            Some(Slot::LValue(_)) => Err(Error::Unimplemented(
                "窗口以左值引用结尾(LET 语境需 eval_lvalue_window)",
            )),
            None => Ok(None),
        }
    }

    /// `eval_slots` 的内核:把槽位**原地**写进调用方缓冲(不 take)。
    fn eval_slots_fill(&mut self, instrs: &[Instruction], stack: &mut Vec<Slot>) -> Result<()> {
        let stack = &mut *stack;
        stack.clear();
        for ins in instrs {
            match &ins.kind {
                Insn::PushInt(v) => stack.push(Slot::Val(Value::Int(*v))),
                Insn::PushFloat(v) => stack.push(Slot::Val(Value::Float(*v))),
                Insn::PushStr(p) => {
                    // 引擎 0x4d(成果 53):首字节为界定符,结果为界定符间
                    // 字节 + 转义解码;`22 22` → 空串(引擎空串字面量形态)
                    stack.push(Slot::Val(Value::Str(decode_pushstr(p))));
                }
                Insn::PushVar(r) => {
                    // 引擎 0x48:索引域清零后装载 → 标量,或数组的 0 号元素
                    let v = self.load_auto(r)?;
                    stack.push(Slot::Val(v));
                }
                Insn::PushVarRef(r) | Insn::PushVarIndexed(r) => {
                    stack.push(Slot::LValue(*r));
                }
                Insn::ArrayLoad { .. } => {
                    // 引擎 0x29:从引用点收集其上全部纯值项为下标表;
                    // 值栈 sp 重置到引用点、装载值压入(引用在平行引用栈单独弹)
                    let marker = stack
                        .iter()
                        .rposition(|s| matches!(s, Slot::LValue(_)))
                        .ok_or_else(|| {
                            Error::format("arrayload 无左值引用(引擎 0x1a5ea 同族)")
                        })?;
                    let indices = gather_indices(&stack[marker + 1..])?;
                    let r = match &stack[marker] {
                        Slot::LValue(r) => *r,
                        _ => unreachable!(),
                    };
                    // 帧局部数组优先(引擎 0042158e:id<1000 系统变量族值的
                    // 读写走帧/系统存储,desc 仅作形状校验;P5 定性成果 50)
                    let input_v = if r.space == VarSpace::At {
                        self.input
                            .as_ref()
                            .and_then(|iv| iv.read(r.id, &indices))
                            .map(Value::Int)
                    } else {
                        None
                    };
                    let v = if let Some(v) = input_v {
                        v
                    } else if r.space == VarSpace::At && r.id == 1 {
                        // @1(引擎 sysvar case 0x01):timeGetTime 派生毫秒计时。
                        // 语料**只用下标 1**(1504 处时间差),故只有子槽 1 是
                        // 已证实语义;其余子槽语义未证实 → 保持 0,不猜。
                        if indices.first() == Some(&1) {
                            Value::Int(self.timer_ms)
                        } else {
                            Value::Int(0)
                        }
                    } else if r.space == VarSpace::At && r.id == 48 {
                        // @48(引擎 sysvar case 0x30):LOOP 迭代计数,读即
                        // 计算、无存储;引擎对下标不敏感(语料无索引用法)
                        Value::Int(self.loop_counter)
                    } else if r.space == VarSpace::At && r.id == 114 {
                        // @114(sysvar case 0x72):显示色深 bpp(成果 59c)
                        Value::Int(32)
                    } else if r.space == VarSpace::At && r.id == 115 {
                        // @115(sysvar case 0x73)= `_SCREEN_SIZE_X`:屏幕宽。
                        // T20 修复:曾写死 oracle 环境 1920;现由 VM 注入真值
                        // (与 WINDOWINFO SX 同源),未注入报错。
                        self.screen_dims(115)?
                    } else if r.space == VarSpace::At && r.id == 116 {
                        // @116(sysvar case 0x74)= `_SCREEN_SIZE_Y`:屏幕高。
                        self.screen_dims(116)?
                    } else if let Some(locals) = self.locals {
                        if locals.has_array(&r) {
                            match locals.get_elem(&r, &indices) {
                                Ok(v) => v.clone(),
                                Err(e) => {
                                    // 帧局部系统族越界读 = 类型默认值(引擎
                                    // sysvar case 0x35 边界检查 `idx >= count`
                                    // → 值 0;成果 52)。全局数组仍严格报错。
                                    if is_frame_sysvar(&r) {
                                        frame_sysvar_default(
                                            locals.array_elem_type(&r),
                                        )
                                    } else {
                                        return Err(e);
                                    }
                                }
                            }
                        } else if is_frame_sysvar(&r) {
                            // 引擎:id<1000 族读**永远走帧**,绝无全局回退
                            // (gparam 计数 = 0 → 槽不存在 → 边界外 = 默认值;
                            // 成果 52:s22 g260 无参调用 es.CGTSS,$55[1] 须
                            // 为 "",读全局会吃到 YSVR 全局 $55 → 分歧)。
                            frame_sysvar_default(frame_sysvar_elem(&r))
                        } else {
                            // 引擎全局数组读 FUN_00459490/00459418(成果 57):
                            // 越界 → 类型默认值,不报错;未声明变量仍报错。
                            match self.store.get_elem(&r, &indices) {
                                Ok(v) => v.clone(),
                                Err(e) => match self.store.array_elem_type(&r) {
                                    Some(et) => type_default(Some(et)),
                                    None => return Err(e),
                                },
                            }
                        }
                    } else {
                        // 同上:全局读越界容忍(成果 57)。
                        match self.store.get_elem(&r, &indices) {
                            Ok(v) => v.clone(),
                            Err(e) => match self.store.array_elem_type(&r) {
                                Some(et) => type_default(Some(et)),
                                None => return Err(e),
                            },
                        }
                    };
                    stack.truncate(marker);
                    stack.push(Slot::Val(v));
                }
                Insn::Binary(op) => {
                    // 先弹右操作数,再弹左操作数(lhs op rhs)
                    let rhs = pop_val(stack, ins)?;
                    let lhs = pop_val(stack, ins)?;
                    stack.push(Slot::Val(apply_binary(*op, &lhs, &rhs)?));
                }
                Insn::Unary(_) => {
                    let v = pop_val(stack, ins)?;
                    stack.push(Slot::Val(v.neg()?));
                }
                Insn::Cast(Cast::ToStr) => {
                    let v = pop_val(stack, ins)?;
                    stack.push(Slot::Val(v.cast_to_str()?));
                }
                Insn::Cast(Cast::ToInt) => {
                    let v = pop_val(stack, ins)?;
                    stack.push(Slot::Val(v.cast_to_int()?));
                }
                Insn::GroupSep => {}
                Insn::Unknown { raw_op, .. } => {
                    return Err(Error::UnresolvedOpcode {
                        code: *raw_op as u32,
                        offset: ins.offset,
                    });
                }
            }
        }
        Ok(())
    }

    /// LET 左值解析:窗口执行后,栈底为 LValue、其上全部为 Int 下标
    /// (引擎:引用点与栈顶之间的纯值项 = 下标表,推入序 = 维度序)。
    pub fn eval_lvalue_window(&mut self, window: &[u8]) -> Result<LValueRef> {
        let instrs = crate::decode_window(window)?;
        self.eval_lvalue_window_instrs(&instrs)
    }

    /// 已解码指令序列的左值解析。
    ///
    /// 引擎模型(VARH_00420ec4/004218b0/00421a4c 反编译,2026-09-03 勘误):
    /// - **0x48 运行期推值**(非引用)——旧实现把左值窗内全部 0x48 变换为引用
    ///   是过度概括,导致 `56 $a | 48 @i | 29` 的下标变量 @i 被误当数组基点;
    /// - 0x56 才推引用(记录标记位);0x29 从引用点上方收集纯值项作下标;
    /// - 0x29 命中**底层引用**且延迟模式时不装载、保留 {基点,下标} 交 LET
    ///   写回(VARH_00421a4c `iVar7==0 && db970!=0 → return 0`)。
    /// 语料佐证(全语料扫描):LET w0 含 aload 的窗口 **全部 0x56 起头**
    /// (6836 个;首指令 0x48 的多指令窗 0 个);单条 0x48/0x76 即标量左值。
    pub fn eval_lvalue_window_instrs(&mut self, instrs: &[Instruction]) -> Result<LValueRef> {
        // 单条 PushVar 族指令 → 直接作为赋值目标(不求值装载值)。
        if let [ins] = instrs {
            if let Insn::PushVar(r) | Insn::PushVarRef(r) | Insn::PushVarIndexed(r) = &ins.kind {
                return Ok(LValueRef { var: *r, indices: Vec::new() });
            }
        }
        // 多指令形态:基点 = **首条** var 类指令(语料全部 0x56);
        // 其后 0x48 保持取值语义(下标表达式);末条 0x29 在引擎延迟模式下
        // 不装载 → 从指令序列剥除,让基点引用与下标值留在栈上。
        let mut instrs: Vec<Instruction> = instrs.to_vec();
        if let Some(first) = instrs.first_mut() {
            if let Insn::PushVar(r) = first.kind {
                first.kind = Insn::PushVarRef(r);
            }
        }
        if matches!(instrs.last().map(|i| &i.kind), Some(Insn::ArrayLoad { .. })) {
            instrs.pop();
        } else if matches!(instrs.first().map(|i| &i.kind), Some(Insn::PushVarRef(_))) {
            // 无尾部 0x29 的多指令形态:除首条外若还有引用类指令,属未知形态
            if instrs.iter().skip(1).any(|i| {
                matches!(i.kind, Insn::PushVarRef(_) | Insn::PushVarIndexed(_))
            }) {
                return Err(Error::Unimplemented(
                    "LET 左值窗无 0x29 但含多条引用(语料外形态;不猜)",
                ));
            }
        }
        let slots = self.eval_slots(&instrs)?;
        let marker = slots
            .iter()
            .position(|s| matches!(s, Slot::LValue(_)))
            .ok_or_else(|| Error::Unimplemented("LET 左值窗口不含变量引用(不猜)"))?;
        let r = match &slots[marker] {
            Slot::LValue(r) => *r,
            _ => unreachable!(),
        };
        // 槽位形态(引擎 LET 左值窗):[LValue(0x56), 下标 Int×n]
        let indices = gather_indices(&slots[marker + 1..])?;
        if marker != 0 {
            return Err(Error::Unimplemented(
                "LET 左值引用前有多余栈项(引擎形态外;不猜)",
            ));
        }
        Ok(LValueRef { var: r, indices })
    }

    /// 引擎 0x48 装载:数组 → 0 号元素;标量 → 值;都无 → 报错(0x1a5ea 同族)。
    fn load_auto(&mut self, r: &VarRef) -> Result<Value> {
        // 输入系统变量（`@_MOUSE_X` / `@_KEY_*` / `@_KEYLAST` …）：由输入层
        // 每帧快照提供，**优先于变量存储**（引擎这些 id 无存储，读即计算）。
        if r.space == VarSpace::At {
            if let Some(input) = &self.input {
                if let Some(v) = input.read_auto(r.id) {
                    return Ok(Value::Int(v));
                }
            }
        }
        // @48 系统变量(引擎 sysvar case 0x30):内层 LOOP 迭代计数,
        // 独立于变量存储(引擎无 desc,读即计算);下标被忽略(引擎同)。
        if r.space == VarSpace::At && r.id == 48 {
            return Ok(Value::Int(self.loop_counter));
        }
        // @133/@138(引擎 sysvar case 0x85/0x8a):光标逻辑坐标
        // (窗口对象 +0x2ac/+0x2b0,输入收集层每帧写入)。VM 无窗口栈,
        // 由播放器注入;(0,0) 缺省。
        if r.space == VarSpace::At && r.id == 133 {
            return Ok(Value::Int(self.cursor.0));
        }
        if r.space == VarSpace::At && r.id == 138 {
            return Ok(Value::Int(self.cursor.1));
        }
        // @114(引擎 sysvar case 0x72,00447ebc):显示色深 bpp(原生全局
        // DAT_00872374,显示初始化写入;s40 色深检查 `@114<=8` → 报
        // 「需 65536 色以上」)。VM 无显示栈,按 oracle 环境 = 32bpp。
        if r.space == VarSpace::At && r.id == 114 {
            return Ok(Value::Int(32));
        }
        // @115/@116(sysvar case 0x73/0x74)= `_SCREEN_SIZE_X/Y`:屏幕宽/高。
        // T20 修复:曾写死 oracle 环境的 1920×1080,与 WINDOWINFO 应答路径
        // (游戏自身 yscfg,本作 1100×825)矛盾;现两路径同源,由 VM 注入。
        if r.space == VarSpace::At && (r.id == 115 || r.id == 116) {
            return self.screen_dims(r.id);
        }
        // 帧局部优先(引擎:帧局部变量 id 属独立空间)
        if self.verbose {
            yuris_log::trace_forced!("load_auto: prefix={:#04x} id={} ({:#x})", r.space.prefix(), r.id, r.id);
        }
        if let Some(locals) = self.locals {
            if let Some(v) = locals.get_opt(r) {
                return Ok(v.clone());
            }
            if locals.has_array(r) {
                let dims = locals.array_dims(r);
                let zeros = vec![0i64; dims.unwrap_or_default().len()];
                return Ok(locals.get_elem(r, &zeros)?.clone());
            }
            // 有帧但无该键:系统族 = 边界外默认(引擎绝无全局回退;成果 52)
            if is_frame_sysvar(r) {
                return Ok(frame_sysvar_default(frame_sysvar_elem(r)));
            }
        }
        if self.store.has_array(r) {
            let dims = self.store.array_dims(r);
            let zeros = vec![0i64; dims.unwrap_or_default().len()];
            // 越界容忍(成果 57):0 号下标在 0 长数组上越界 → 类型默认
            return match self.store.get_elem(r, &zeros) {
                Ok(v) => Ok(v.clone()),
                Err(e) => match self.store.array_elem_type(r) {
                    Some(et) => Ok(type_default(Some(et))),
                    None => Err(e),
                },
            };
        }
        Ok(self.store.get(r)?.clone())
    }
}

/// 求值栈槽位:值或左值引用(引擎平行引用栈的忠实建模)。
#[derive(Debug, Clone, PartialEq)]
pub enum Slot {
    /// 普通值。
    Val(Value),
    /// 左值引用(0x56/0x76;LET 赋值目标 / ArrayLoad 的数组基点)。
    LValue(VarRef),
}

impl Slot {
    /// 转值;LValue → `None`。
    pub fn into_value(self) -> Option<Value> {
        match self {
            Slot::Val(v) => Some(v),
            Slot::LValue(_) => None,
        }
    }
}

/// LET 左值解析结果:变量 + 下标表(空 = 标量)。
#[derive(Debug, Clone, PartialEq)]
pub struct LValueRef {
    /// 变量引用。
    pub var: VarRef,
    /// 下标表(推入顺序 = 维度顺序;空 = 标量)。
    pub indices: Vec<i64>,
}

/// 帧局部系统变量族(@53/@60 INT、@54/@61 FLT、$55/$62 STR;成果 50/52)。
pub fn is_frame_sysvar(r: &VarRef) -> bool {
    match r.space {
        VarSpace::At => matches!(r.id, 53 | 54 | 60 | 61),
        VarSpace::Dollar => matches!(r.id, 55 | 62),
        _ => false,
    }
}

/// 帧局部系统族的元素类型(按 id 推断;gparam 未声明该型时读边界外默认)。
pub fn frame_sysvar_elem(r: &VarRef) -> Option<yuris_value::ElemType> {
    match (r.space, r.id) {
        (VarSpace::At, 53) | (VarSpace::At, 60) => Some(yuris_value::ElemType::Int),
        (VarSpace::At, 54) | (VarSpace::At, 61) => Some(yuris_value::ElemType::Float),
        (VarSpace::Dollar, 55) | (VarSpace::Dollar, 62) => Some(yuris_value::ElemType::Str),
        _ => None,
    }
}

/// 帧局部系统族越界读的默认值(引擎 sysvar case 0x35 边界外 → 0;
/// STR 族同理按类型默认 "" / 0.0)。
pub fn frame_sysvar_default(et: Option<yuris_value::ElemType>) -> Value {
    type_default(et)
}

/// 按元素类型的读取默认值(引擎全局数组读 FUN_00459490/00459418:
/// 越界 → 0 / 0.0 / "",不报错;成果 57)。
pub fn type_default(et: Option<yuris_value::ElemType>) -> Value {
    match et {
        Some(yuris_value::ElemType::Float) => Value::Float(0.0),
        Some(yuris_value::ElemType::Str) => Value::Str(Vec::new()),
        _ => Value::Int(0),
    }
}

/// 从槽位切片收集下标表:全部为 `Val(Int)`,否则报错
/// (引擎下标为 i64;Float/Str 下标语义 Unknown)。
///
/// **报错带实际取值与下标序号** —— 只写「非整数下标」定位不到是哪一个、值是什么,
/// 排查时只能回去猜(本仓已为此返工过一次)。
fn gather_indices(slots: &[Slot]) -> Result<Vec<i64>> {
    slots
        .iter()
        .enumerate()
        .map(|(i, s)| match s {
            Slot::Val(Value::Int(v)) => Ok(*v),
            Slot::Val(Value::Float(f)) => Err(Error::format(format!(
                "unimplemented: 非整数下标语义 Unknown(引擎下标域为 i64);\
                 下标[{i}] = 浮点 {f}"
            ))),
            Slot::Val(Value::Str(s)) => Err(Error::format(format!(
                "unimplemented: 非整数下标语义 Unknown(引擎下标域为 i64);\
                 下标[{i}] = 字符串 {:?}",
                String::from_utf8_lossy(s)
            ))),
            Slot::LValue(_) => Err(Error::format(format!(
                "下标表中混入左值引用(引擎 0x2710a 同族);下标[{i}]"
            ))),
        })
        .collect()
}

fn pop_val(stack: &mut Vec<Slot>, ins: &Instruction) -> Result<Value> {
    match stack.pop() {
        Some(Slot::Val(v)) => Ok(v),
        Some(Slot::LValue(_)) => Err(Error::format(format!(
            "lvalue 不能作运算数 at offset {}",
            ins.offset
        ))),
        None => Err(Error::format(format!(
            "stack underflow at offset {} (op {:#04x})",
            ins.offset, ins.raw_op
        ))),
    }
}

fn apply_binary(op: BinOp, lhs: &Value, rhs: &Value) -> Result<Value> {
    match op {
        BinOp::Add => lhs.add(rhs),
        BinOp::Sub => lhs.sub(rhs),
        BinOp::Mul => lhs.mul(rhs),
        BinOp::Div => lhs.div(rhs),
        BinOp::Mod => lhs.modulo(rhs),
        BinOp::Eq => Ok(Value::Int(lhs.eq(rhs)? as i64)),
        BinOp::Ne => Ok(Value::Int(lhs.ne(rhs)? as i64)),
        BinOp::Ge => Ok(Value::Int(lhs.ge(rhs)? as i64)),
        BinOp::Le => Ok(Value::Int(lhs.le(rhs)? as i64)),
        BinOp::Gt => Ok(Value::Int(lhs.gt(rhs)? as i64)),
        BinOp::Lt => Ok(Value::Int(lhs.lt(rhs)? as i64)),
        BinOp::BitAnd => lhs.bitand(rhs),
        BinOp::BitOr => lhs.bitor(rhs),
        BinOp::LogAnd => lhs.logand(rhs),
        BinOp::LogOr => lhs.logor(rhs),
        BinOp::Xor => lhs.xor(rhs),
    }
}

/// 引擎 0x4d 字符串字面量解码(成果 53,`00420cb8_expr_0x4d_pushstr.c` 反编译):
/// 载荷首字节 = 界定符(通常 `"`,也可以是 `'` 等);结果 = 界定符**之间**的字节,
/// 界定符本身不进入结果串;转义:`\\`→`\`、`\n`→LF(0x0a)、`\t`→TAB(0x09);
/// SJIS 双字节首字节(0x81-0x9F/0xE0-0xEF,引擎引导表 0x0059b0c0 语义)
/// 连同后续字节整体拷贝(避免把 SJIS 尾字节误当界定符/转义)。
/// 载荷为空/无界定符闭合 → 引擎循环自然终止于同界定符;畸形载荷(首字节
/// 即定界、无闭合)按引擎「读到闭合或截断」建模 —— 这里保守按无闭合处理,
/// 返回整个载荷(与旧行为兼容;语料无此形态)。
pub fn decode_pushstr(payload: &[u8]) -> Vec<u8> {
    let Some(&delim) = payload.first() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(payload.len());
    let mut i = 1usize;
    while i < payload.len() {
        let b = payload[i];
        if b == delim {
            // 引擎:遇到与首字节相同的界定符 → 结束(其后字节仍在窗口内,
            // 但 0x4d 是独立指令,操作数恰为字面量;这里到界定符即截断)
            return out;
        }
        if b == b'\\' {
            // 引擎转义分支:\\ → \ ;\n → 0x0a ;\t → 0x09;其余 → 丢弃反斜杠
            match payload.get(i + 1) {
                Some(0x5c) => {
                    out.push(0x5c);
                    i += 2;
                }
                Some(0x6e) => {
                    out.push(0x0a);
                    i += 2;
                }
                Some(0x74) => {
                    out.push(0x09);
                    i += 2;
                }
                // 反斜杠在末尾:引擎会继续读越界;保守按字面处理
                _ => {
                    out.push(b'\\');
                    i += 1;
                }
            }
            continue;
        }
        // SJIS 双字节:首字节 + 后续整体拷贝(引擎步进表 0x0059b0c0/v494 的
        // 0x5b40a0,首个/末个前导区为 **0x81-0x9F 与 0xE0-0xFC**)。
        out.push(b);
        i += 1;
        if (0x81..=0x9F).contains(&b) || (0xE0..=0xFC).contains(&b) {
            if let Some(&b2) = payload.get(i) {
                out.push(b2);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insn::{VarRef, VarSpace};

    fn run(window: &[u8]) -> Result<Vec<Value>> {
        let mut store = VariableStore::new();
        Evaluator::new(&mut store).eval_window(window)
    }

    #[test]
    fn constant_arithmetic() {
        // 2 + 3 = 5
        assert_eq!(
            run(&[0x42, 1, 0, 2, 0x42, 1, 0, 3, 0x2b, 0, 0]).unwrap(),
            vec![Value::Int(5)]
        );
        // 字符串拼接(手册 Confirmed;0x4d 载荷 = 界定符包裹,成果 53)
        assert_eq!(
            run(&[
                0x4d, 4, 0, b'"', b'1', b'2', b'"',
                0x4d, 4, 0, b'"', b'3', b'4', b'"',
                0x2b, 0, 0,
            ])
            .unwrap(),
            vec![Value::Str(b"1234".to_vec())]
        );
        // 混合精度: 5 * 0.5 = 2.5
        let mut w = vec![0x42, 1, 0, 5, 0x46, 8, 0];
        w.extend_from_slice(&0.5f64.to_le_bytes());
        w.extend_from_slice(&[0x2a, 0, 0]);
        assert_eq!(run(&w).unwrap(), vec![Value::Float(2.5)]);
        // 操作数顺序: 8 - 3 = 5(先弹 rhs)
        assert_eq!(
            run(&[0x42, 1, 0, 8, 0x42, 1, 0, 3, 0x2d, 0, 0]).unwrap(),
            vec![Value::Int(5)]
        );
    }

    #[test]
    fn comparisons_and_casts() {
        assert_eq!(
            run(&[0x42, 1, 0, 5, 0x42, 1, 0, 5, 0x3d, 0, 0]).unwrap(),
            vec![Value::Int(1)]
        );
        assert_eq!(
            run(&[0x42, 1, 0, 3, 0x42, 1, 0, 5, 0x3c, 0, 0]).unwrap(),
            vec![Value::Int(1)]
        );
        // $() 转字符串
        assert_eq!(
            run(&[0x42, 1, 0, 9, 0x73, 0, 0]).unwrap(),
            vec![Value::Str(b"9".to_vec())]
        );
        // @() 转整数(手册: @("123"+"45") == 12345;0x4d 带界定符,成果 53)
        let w = [
            0x4d, 5, 0, b'"', b'1', b'2', b'3', b'"',
            0x4d, 4, 0, b'"', b'4', b'5', b'"',
            0x2b, 0, 0,
            0x69, 0, 0,
        ];
        assert_eq!(run(&w).unwrap(), vec![Value::Int(12345)]);
    }

    #[test]
    fn pushstr_decodes_delimiters_and_escapes() {
        // 成果 53(引擎 00420cb8):首字节 = 界定符;结果 = 界定符间字节;
        // 空串字面量 22 22 → 空;转义 \\ \n \t;SJIS 双字节整体拷贝
        assert_eq!(decode_pushstr(b"\"\""), Vec::<u8>::new());
        assert_eq!(decode_pushstr(b"\"AB\""), b"AB".to_vec());
        assert_eq!(decode_pushstr(b"'ES'"), b"ES".to_vec());
        assert_eq!(decode_pushstr(b"\"a\\\\b\""), b"a\\b".to_vec());
        assert_eq!(decode_pushstr(b"\"a\\nb\""), b"a\nb".to_vec());
        assert_eq!(decode_pushstr(b"\"a\\tb\""), b"a\tb".to_vec());
        // SJIS 双字节(0x83 0x5c)不因尾字节 0x5c 误入转义
        assert_eq!(decode_pushstr(b"\"\x83\x5c\""), b"\x83\x5c".to_vec());
        // 空载荷 → 空串
        assert_eq!(decode_pushstr(b""), Vec::<u8>::new());
    }

    #[test]
    fn sysvar_at1_is_engine_millisecond_timer() {
        // @1[1] = 引擎毫秒计时(引擎 sysvar case 0x01,`command-layer.md` §9c)。
        // 窗口: pushvarref(@,1) pushint(1) aload
        let w1 = [0x56, 3, 0, 0x40, 0x01, 0x00, 0x42, 1, 0, 1, 0x29, 1, 0, 0];
        let mut store = VariableStore::new();
        let mut ev = Evaluator::new(&mut store);
        ev.set_timer_ms(1234);
        assert_eq!(ev.eval_window(&w1).unwrap(), vec![Value::Int(1234)]);
        // 未证实的子槽(下标 != 1)保持 0 —— 不猜
        let w2 = [0x56, 3, 0, 0x40, 0x01, 0x00, 0x42, 1, 0, 2, 0x29, 1, 0, 0];
        let mut store2 = VariableStore::new();
        let mut ev2 = Evaluator::new(&mut store2);
        ev2.set_timer_ms(1234);
        assert_eq!(ev2.eval_window(&w2).unwrap(), vec![Value::Int(0)]);
        // 未注入(无 VM)= 0:恒定 0 正是「计时缺失」的旧行为,由 VM 侧注入修正
        let mut store3 = VariableStore::new();
        assert_eq!(
            Evaluator::new(&mut store3).eval_window(&w1).unwrap(),
            vec![Value::Int(0)]
        );
    }

    #[test]
    fn screen_dims_injected_or_error() {
        // @115/@116 = 屏幕宽/高(T20):旧实现写死 oracle 环境的 1920×1080,
        // 与 WINDOWINFO 应答路径矛盾。现:注入 → 真值;未注入 → 报错(不猜)。
        // 窗口: pushvar(@,115)  → 0x48 装载路径(load_auto)
        let w115 = [0x48, 3, 0, 0x40, 0x73, 0x00];
        let w116 = [0x48, 3, 0, 0x40, 0x74, 0x00];
        let mut store = VariableStore::new();
        let mut ev = Evaluator::new(&mut store);
        ev.set_screen_size(1100, 825);
        assert_eq!(ev.eval_window(&w115).unwrap(), vec![Value::Int(1100)]);
        assert_eq!(ev.eval_window(&w116).unwrap(), vec![Value::Int(825)]);
        // 未注入 → **报错**,绝不回落 1920/1080
        let mut store2 = VariableStore::new();
        let e = Evaluator::new(&mut store2).eval_window(&w115).unwrap_err();
        assert!(
            e.to_string().contains("未注入"),
            "未注入须显式报错,得到: {e}"
        );
    }

    #[test]
    fn input_sysvars_are_read_from_snapshot() {
        // @_MOUSE_X(133) / @_KEY_ENTER(244) / @_INP(144)[code]。
        use std::sync::Arc;
        let mut vars = InputVars::default();
        vars.claimed.extend([133u16, 244, 144]);
        vars.scalars.insert(133, 640);
        vars.scalars.insert(244, 7);
        vars.inp = vec![0; 256];
        vars.inp[0x0D] = -3;
        // pushvar(@,133) → 0x48
        let w_mouse_x = [0x48, 3, 0, 0x40, 133, 0];
        // pushvar(@,244)
        let w_enter = [0x48, 3, 0, 0x40, 244, 0];
        // _INP: pushvarref(@,144) pushint(0x0D) aload
        let w_inp = [0x56, 3, 0, 0x40, 144, 0, 0x42, 1, 0, 0x0D, 0x29, 1, 0, 0];
        let mut store = VariableStore::new();
        let mut ev = Evaluator::new(&mut store);
        ev.set_input(Arc::new(vars));
        assert_eq!(ev.eval_window(&w_mouse_x).unwrap(), vec![Value::Int(640)]);
        assert_eq!(ev.eval_window(&w_enter).unwrap(), vec![Value::Int(7)]);
        assert_eq!(ev.eval_window(&w_inp).unwrap(), vec![Value::Int(-3)]);
        // 未注入 = 无输入面:`@133` 回落到旧的光标注入路径（缺省 0）。
        let mut store2 = VariableStore::new();
        assert_eq!(
            Evaluator::new(&mut store2).eval_window(&w_mouse_x).unwrap(),
            vec![Value::Int(0)]
        );
    }

    #[test]
    fn variables_and_errors() {
        let mut store = VariableStore::new();
        store.set(
            &VarRef { space: VarSpace::At, id: 100 },
            Value::Int(7),
        );
        let w = [0x48, 3, 0, 0x40, 100, 0, 0x42, 1, 0, 1, 0x2b, 0, 0];
        assert_eq!(
            Evaluator::new(&mut store).eval_window(&w).unwrap(),
            vec![Value::Int(8)]
        );
        // 未定义变量 → 报错
        let w_undef = [0x48, 3, 0, 0x40, 0xff, 0xff];
        let mut store2 = VariableStore::new();
        assert!(Evaluator::new(&mut store2).eval_window(&w_undef).is_err());
        // pushvarref → Unimplemented(语义未解,不猜)
        let w_ref = [0x56, 3, 0, 0x24, 0x04, 0x00];
        assert!(matches!(
            run(&w_ref),
            Err(Error::Unimplemented(_))
        ));
        // 未证实 opcode → UnresolvedOpcode
        assert!(matches!(
            run(&[0x00, 0, 0]),
            Err(Error::UnresolvedOpcode { code: 0x00, offset: 0 })
        ));
        // 栈下溢 → 报错
        assert!(run(&[0x2b, 0, 0]).is_err());
    }
}
