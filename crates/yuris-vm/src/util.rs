//! VM 内部工具:字节窗口解码 / 变量目标模型 / SJIS 与 VARACT 辅助 / 值工具。
//!
//! 本模块只放**无状态**纯函数与轻量数据模型 —— 有状态的一切在
//! [`crate::GroupVm`]。从 `lib.rs` 拆出(2026-10-15 重构,纯搬运不改行为)。

use yuris_core::{Error, Result};
use yuris_value::Value;

use crate::commands::{CmdKind, CommandDispatch, DeclElem};
use crate::host::ScriptCtx;

/// 解码单条 M-串窗口(`4d len "name"` 或类型 5 的无引号形式)。
pub(crate) fn mstring(window: &[u8]) -> Option<&[u8]> {
    if window.len() < 4 || window[0] != 0x4d {
        return None;
    }
    let payload = &window[3..];
    if payload.len() >= 2 && payload[0] == b'"' && *payload.last().unwrap() == b'"' {
        return Some(&payload[1..payload.len() - 1]);
    }
    Some(payload)
}

/// 变量引用目标(VARACT/VARINFO/CGINFO/SAVE/LOAD 的 SET/LET 槽;kind 2 延迟求值)。
#[derive(Debug, Clone, PartialEq)]

pub(crate) enum VarTarget {
    /// 标量(`@1041`)。
    Scalar(yuris_value::VarRef),
    /// 数组元素(`$0x37[1]`)。
    ///
    /// 2026-09-05 勘误(P1 对拍[144128]定位):基点 = **首条** var 类指令,
    /// 其后至末条 0x29 之前的全部指令 = **下标表达式**(引擎 kind 2 延迟
    /// 求值,写/读时求值)。旧实现用「最后一条 var 指令」当基点、下标只认
    /// 整数字面量 → `56 @1265 | 48 @1704 | 29` 形态把下标变量 @1704 误当
    /// 基点、写入错位(与 LET 左值窗成果 44 同族语义,此处漏同步)。
    /// 常量下标 = 指令序列恰为 PushInt 序列的特例。
    Indexed(yuris_value::VarRef, Vec<yuris_script::Instruction>),
}

impl VarTarget {
    pub(crate) fn summary(&self) -> String {
        match self {
            VarTarget::Scalar(r) => r.display(),
            VarTarget::Indexed(r, idx) => {
                if idx.iter().all(|i| matches!(i.kind, yuris_script::Insn::PushInt(_))) {
                    let vals: Vec<i64> = idx
                        .iter()
                        .filter_map(|i| match i.kind {
                            yuris_script::Insn::PushInt(n) => Some(n),
                            _ => None,
                        })
                        .collect();
                    format!("{}[{:?}]", r.display(), vals)
                } else {
                    format!("{}[expr×{}]", r.display(), idx.len())
                }
            }
        }
    }
}

/// 从引用槽窗口字节提取变量目标(引擎 kind 2 延迟求值形态)。
///
/// 识别形态(语料 Confirmed):
/// - 单条 `48/56/76 + [prefix][id:u16]` → 标量(0x76 裸引用 = 整数组,s37 g50);
/// - `56 + [prefix][id]` + 下标指令×N + `29`(ArrayLoad) → 数组元素,
///   基点 = 首条 var 指令,下标 = 其后指令序列(写/读时求值,支持
///   变量下标与表达式下标;见 [`VarTarget::Indexed`] 勘误)。
pub(crate) fn extract_var_target(bytes: &[u8]) -> Result<VarTarget> {
    let instrs = yuris_script::decode_window(bytes)?;
    // 单条 var 类指令 → 标量
    if let [ins] = &instrs[..] {
        if let yuris_script::Insn::PushVar(r)
        | yuris_script::Insn::PushVarRef(r)
        | yuris_script::Insn::PushVarIndexed(r) = &ins.kind
        {
            return Ok(VarTarget::Scalar(*r));
        }
    }
    // 多指令形态:基点 = 首条 var 类指令(语料:左值窗全部 0x56 起头)
    let base = match instrs.first().map(|i| &i.kind) {
        Some(yuris_script::Insn::PushVar(r))
        | Some(yuris_script::Insn::PushVarRef(r))
        | Some(yuris_script::Insn::PushVarIndexed(r)) => *r,
        _ => return Err(Error::format("引用窗首条不是变量指令")),
    };
    // 末条 0x29 = 引擎延迟模式不装载的加载标记,剥除;其余为下标表达式
    let idx_len = instrs
        .last()
        .map(|i| matches!(i.kind, yuris_script::Insn::ArrayLoad { .. }))
        .unwrap_or(false);
    if !idx_len {
        return Err(Error::format(
            "引用窗多指令形态缺末条 0x29(语料外形态;不猜)",
        ));
    }
    if instrs.len() < 3 {
        return Err(Error::format("引用窗多指令形态过短"));
    }
    Ok(VarTarget::Indexed(
        base,
        instrs[1..instrs.len() - 1].to_vec(),
    ))
}


/// **SJIS 双字节前导字节判定** —— 引擎步进表的**唯一**等价物。
///
/// 表来源(T5 取证,2026-10-16):v494 引擎 `.data` `0x5b40a0`,256 项 0/1 掩码
/// (与 v555 `DAT_0059b0c0` 同一张表,地址随版本漂移)。dump 结果:
///
/// ```text
/// 0x00-0x7F: 全 0     0x81-0x9F: 全 1
/// 0xA0:      0        0xA1-0xDF: 全 0(半角片假名,单字节)
/// 0xE0-0xFC: 全 1     0xFD-0xFF: 全 0
/// ```
///
/// 三处使用点交叉证实 `1` = 双字节前导:
/// - `FUN_0045543c`(UPPER)/`FUN_0045f0dc`(LOWER):`if table[c]==1 → 指针 +=2`;
/// - `FUN_00444f10`(文字宽度):`3 - (table[c] + 1)`(双字节 → 1,单字节 → 2)。
///
/// **勘误(2026-10-16)**:旧实现只认到 `0xEF`(漏 `0xF0-0xFC`)—— CP932 的
/// 用户区/IBM 扩展区正落在这里,凡含此类字节的串会算错字符数/切错偏移。
pub(crate) fn is_sjis_lead(b: u8) -> bool {
    (0x81..=0x9F).contains(&b) || (0xE0..=0xFC).contains(&b)
}

/// SJIS 字符数(引擎字符计数语义:LENGTH = 字符数,非字节数;
/// 步进表见 [`is_sjis_lead`])。
pub(crate) fn sjis_char_len(bytes: &[u8]) -> usize {
    let mut off = 0usize;
    let mut n = 0usize;
    while off < bytes.len() {
        off += if is_sjis_lead(bytes[off]) { 2 } else { 1 };
        n += 1;
    }
    n
}

/// VARACT UPPER/LOWER 的 SJIS 感知 ASCII 大小写转换
/// (引擎 FUN_004546cc=UPPER / FUN_00464dac=LOWER,Ghidra 逐行,57B 各):
/// 双字节首区(步进表 ==1)连跳 2 字节不转换;单字节
/// `to_upper`: 'a'-'z' → -0x20;`!to_upper`: 'A'-'Z' → +0x20;其余原样。
/// 引擎按 NUL 终止扫描,缓冲区语义由调用方保证 —— 这里按字节长度截住。
pub(crate) fn varact_ascii_case(bytes: &[u8], to_upper: bool) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut i = 0usize;
    while i < out.len() {
        let b = out[i];
        if is_sjis_lead(b) {
            i += 2; // 双字节整体跳过(引擎 in_EAX += 2)
            continue;
        }
        if to_upper && (b'a'..=b'z').contains(&b) {
            out[i] = b - 0x20;
        } else if !to_upper && (b'A'..=b'Z').contains(&b) {
            out[i] = b + 0x20;
        }
        i += 1;
    }
    out
}

/// 字符序数 → SJIS 字节偏移(引擎 FUN_00471cf5/00471d7f 语义,字符步进表
/// `DAT_0059b0c0`:双字节首区 → 步进 2,其余步进 1;表见 [`is_sjis_lead`])。
/// 越界(序数超过字符数)→ `None`(引擎报错 0x1d52e/0x1d538 同族)。
pub(crate) fn varact_char_to_byte(s: &Value, ordinal: usize) -> Option<usize> {
    let Value::Str(bytes) = s else {
        return None;
    };
    let mut off = 0usize;
    for _ in 0..ordinal {
        if off >= bytes.len() {
            return None;
        }
        off += if is_sjis_lead(bytes[off]) { 2 } else { 1 };
    }
    Some(off)
}

/// 字节偏移 → 字符序数(引擎 FUN_00471d7f 逆换算;非字符边界 → 界内序数)。
#[allow(dead_code)]
pub(crate) fn varact_byte_to_char(s: &Value, byte_off: usize) -> usize {
    let Value::Str(bytes) = s else {
        return 0;
    };
    let mut off = 0usize;
    let mut n = 0usize;
    while off < bytes.len() && off < byte_off {
        off += if is_sjis_lead(bytes[off]) { 2 } else { 1 };
        n += 1;
    }
    n
}

/// 消费 `ctx` 的**声明组**写入 `store`。
///
/// 声明命令族(YSCM 名表 Confirmed):
/// - FLT 族:F_FLT(0x10)/FLT(0x19)/G_FLT..4(0x1d-0x20)/S_FLT(0x52) → Float
/// - INT 族:F_INT(0x11)/INT(0x32)/G_INT..4(0x21-0x24)/S_INT(0x53) → Int
/// - STR 族:F_STR(0x12)/STR(0x5c)/G_STR..4(0x25-0x28)/S_STR(0x54) → Str
/// - VAR 族(0x13/0x29/0x55):类型不定 → 跳过(Unknown,不猜)
///
/// 窗口形态(样本探针):
/// - 标量:单条 `48/56 [prefix][id]` → 默认值
/// - 数组:`56/76 [prefix][id]` + 下标表达式 + `29`(ArrayLoad) →
///   declare_array(bounds=[表达式值+1])(**上界引用**;yst00000 F_STR 实测
///   `$1226[400+1]`;引擎 0x29 行主序装载 Confirmed,上界语义 Likely)
/// - M-串名字窗(tag B2=3):非引用,自然跳过
///
/// 幂等:已定义标量/同型数组不覆盖(YSVR 初值应用须在本消费之后)。

/// 值摘要(事件记录用;Str 截断到 24 字节防爆炸)。
pub(crate) fn value_summary(v: &Value) -> String {
    match v {
        Value::Int(n) => format!("int:{n}"),
        Value::Float(f) => format!("flt:{f}"),
        Value::Str(b) => {
            let shown: Vec<u8> = b.iter().copied().take(24).collect();
            format!("str:{}..({}B)", String::from_utf8_lossy(&shown), b.len())
        }
    }
}

/// 值真值。
///
/// - `Int`:`!= 0` —— **Likely**(条件表达式族的一致语义)。
/// - `Str`:非空 —— **Likely**。
/// - `Float`:`!= 0.0` —— **Likely,UNVERIFIED**。
///
/// 三者同族:引擎条件求值走同一套"为零/为空即假"的判定,只是元素类型不同。
/// Float 一路此前显式报错(`Unimplemented`),导致含浮点条件的脚本整链停摆
/// (实测某发行数据入口脚本首组即命中)。
///
/// ⚠️ UNVERIFIED:尚未在原引擎上对拍。需要时用同语义脚本在引擎与 VM 上各跑
/// 一遍比对分支走向;若引擎对 `-0.0` / NaN 有特殊处理,此处需修正。
pub(crate) fn value_truthy(v: &Value) -> Result<bool> {
    match v {
        Value::Int(i) => Ok(*i != 0),
        Value::Str(s) => Ok(!s.is_empty()),
        Value::Float(f) => Ok(*f != 0.0),
    }
}


/// 把窗口内属于**声明族**的命令应用到变量存储(数组维数/字符串初值),返回消费条数。
///
/// 当前按「命令名是否属声明族」判族(见 PROGRESS 成果 190 的残余说明)。
pub fn consume_declarations_into(
    store: &mut yuris_value::VariableStore,
    ctx: &ScriptCtx,
    dispatch: &CommandDispatch,
) -> Result<usize> {
    use yuris_script::Insn;
    use yuris_value::{ElemType, Value};
    let mut count = 0usize;
    for (gi, g) in ctx.groups.iter().enumerate() {
        // 声明族判定按**命令名**（来自游戏自身 YSCM），与引擎版本无关。
        let Some(name) = dispatch.name_of(g.command_type as u16) else {
            continue;
        };
        let elem: ElemType = match CmdKind::decl_elem_type(name) {
            Some(DeclElem::Float) => ElemType::Float,
            Some(DeclElem::Int) => ElemType::Int,
            Some(DeclElem::Str) => ElemType::Str,
            None => continue,
        };
        let default = match elem {
            ElemType::Float => Value::Float(0.0),
            ElemType::Int => Value::Int(0),
            ElemType::Str => Value::Str(Vec::new()),
        };
        let first = ctx.first_slots[gi];
        let wins: Vec<yuris_format::ystb::CommandSlot> = ctx.script.slots()
            [first..first + g.window_count as usize]
            .to_vec();
        for w in &wins {
            if w.tag & 0xFF != 0 {
                continue;
            }
            let Some(bytes) = ctx.script.window_bytes_pooled_copy(w) else {
                continue;
            };
            let Ok(instrs) = yuris_script::decode_window(&bytes) else {
                continue;
            };
            // 引用窗必须以单条变量指令开头
            let Some(base) = (match instrs.first().map(|i| &i.kind) {
                Some(Insn::PushVar(r)) | Some(Insn::PushVarRef(r))
                | Some(Insn::PushVarIndexed(r)) => Some(r.clone()),
                _ => None,
            }) else {
                continue;
            };
            if instrs.len() == 1 {
                // 标量声明
                if store.get_opt(&base).is_none() && !store.has_array(&base) {
                    store.set(&base, default.clone());
                    count += 1;
                }
                continue;
            }
            // 数组声明:存在 ArrayLoad;取其前的下标表达式值(变量指令压 0 占位)
            let has_load = instrs.iter().any(|i| matches!(i.kind, Insn::ArrayLoad { .. }));
            if !has_load {
                continue;
            }
            let mut stack: Vec<i64> = Vec::new();
            // 上界是否为**纯常量**表达式(勘误,2026-10-13):实测语料存在
            // 上界由**运行期变量**给出的声明窗,如 `G_STR2 @[0x0afb][@[0x0c61]]`
            // (s27 g3421)。旧实现把变量引用一律压 0 → 该数组被声明成
            // 「长度 1」并带上 G_STR2 的 STR 类型,凭空**遮蔽**同名 INT 标量
            // (s52 g1 的 `S_INT @[0x0afb] = 10`)—— 实测后果:复合赋值
            // `@[0x0afb] -= 1` 读到空串 → 挂起「字符串 - 语义 Unknown」。
            // 非纯常量上界 = 该维数须运行期求值(本内核未实现)→ **跳过声明**,
            // 交给已有的标量/其它脚本声明,不猜。
            let mut const_bound = true;
            let mut stack_seen_var = false;
            for ins in &instrs {
                match &ins.kind {
                    Insn::PushInt(n) => stack.push(*n),
                    Insn::PushVar(_) | Insn::PushVarRef(_) | Insn::PushVarIndexed(_) => {
                        // 首个引用 = 被声明对象;其余引用 = 非纯常量上界
                        if stack.is_empty() && !stack_seen_var {
                            stack_seen_var = true;
                            stack.push(0);
                        } else {
                            const_bound = false;
                            break;
                        }
                    }
                    Insn::Binary(op) => {
                        let (Some(rhs), Some(lhs)) = (stack.pop(), stack.pop()) else {
                            break;
                        };
                        let v = match op {
                            yuris_script::BinOp::Add => lhs.wrapping_add(rhs),
                            yuris_script::BinOp::Sub => lhs.wrapping_sub(rhs),
                            yuris_script::BinOp::Mul => lhs.wrapping_mul(rhs),
                            unmodeled => {
                                // **上界表达式里的未建模算子以前被当成 0**
                                // ⇒ 数组被声明成长度 1,静默遮蔽真实维数。
                                // 现在不猜:跳过该声明并大告警。
                                yuris_log::alert_once(
                                    module_path!(),
                                    format!("array-bound-op:{unmodeled:?}"),
                                    format!(
                                        "数组声明上界含未建模算子,该声明已跳过\n\
                                         算子: {unmodeled:?}\n\
                                         声明对象: {}\n\
                                         处置: 只支持 +/−/×(纯常量);该数组保持未声明,\
                                         由其它声明或运行期路径补齐",
                                        base.display()
                                    ),
                                );
                                const_bound = false;
                                break;
                            }
                        };
                        stack.push(v);
                    }
                    Insn::ArrayLoad { .. } => break,
                    _ => break,
                }
            }
            if !const_bound {
                continue;
            }
            if let Some(&declared) = stack.last() {
                if declared >= 0 {
                    // **勘误(2026-10-18,存档链实机闭环定位)**:旧实现把声明
                    // 括号值当「最大下标」做 `+1`(`need = max_idx + 1`)。
                    // 引擎侧铁证(v494 写回 `0x447989` 反汇编 + 真实存档):
                    // ① `0x1a63a` 要求 payload dims == desc dims **逐维相等**;
                    // ② 真实存档块 save0000x DNO=4 = STR **[16]**(真引擎所写);
                    // ③ 装它的脚本 s19 g470 声明字面量 = **16**(g522 把该块
                    //    装进 $2189,引擎 $2189 描述符必为 [16] 否则引擎自己也
                    //    过不了 0x1a63a)。⇒ 手册 `INT[@HA(34,5,6)]` 的括号值
                    //    = **各维元素个数**,不加 1。运行期声明路径
                    //    (`exec::misc` 成果 109)本来就是 `&[n]` 不加 1,
                    //    两条路径自此一致。
                    let need = declared as u32;
                    match store.array_dims(&base) {
                        None => {
                            store.declare_array(&base, elem, &[need]);
                            count += 1;
                        }
                        Some(dims) => {
                            // 同数组多条上界声明 → 取最大(扩容保留旧值)
                            if dims.len() == 1 && dims[0] < need {
                                store.resize_array(&base, &[need])?;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(count)

}
