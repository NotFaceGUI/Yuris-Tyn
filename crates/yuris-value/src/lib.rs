//! yuris-value
//!
//! Value 类型与变量存储。YU-RIS 的类型转换语义在此实现,不使用 `serde_json::Value`。
//!
//! ## 证据等级(铁律:每个语义行为都标注等级)
//!
//! - **Confirmed**(官方手册 `yu-ris_sdk_0495/マニュアル/YU-RIS/html`):
//!   变量三类型 INT(64 位)/FLT(64 位)/STR;`@`数值/`$`字符串前缀;
//!   字符串 `==` 比较;`+` 字符串拼接;`$()`/`@()` 显式转换;
//!   `@("123"+"45") == 12345`;`INT[@A = 0xFFFFFFFFFFFFFFFF]` 得 -1(i64 环绕字面量)
//! - **Likely**(C 实现惯例,待 Golden Test 对照):
//!   混合 int/float 运算提升为 float;有符号溢出环绕;比较结果为 1/0;
//!   float→int 截断
//! - **Unknown → 显式报错**:字符串排序比较、字符串真值、float→str 格式化、
//!   非数字字符串转 int 的精确规则(手册称 β 版"不確定")

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;

use yuris_core::{Error, Result};

/// crate 版本(与 workspace 同步)
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 变量空间。对应源码前缀字符(编译器证实,见 `docs/reverse/yscom-compiler-notes.md` §4.3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VarSpace {
    /// `@` (0x40) —— 数值变量(INT/FLT)。
    At,
    /// `$` (0x24) —— 字符串变量。
    Dollar,
    /// `#` (0x23) —— 标签/宏引用(`GO[#=es.ERIS]`)。
    Hash,
    /// `` ` `` (0x60) —— 罕见,语义 Unknown。
    Backtick,
    /// 其他前缀(本语料未观测;原样保留,不猜)。
    Unknown(u8),
}

impl VarSpace {
    /// 由操作数首字节解析。
    pub fn from_prefix(b: u8) -> Self {
        match b {
            0x40 => VarSpace::At,
            0x24 => VarSpace::Dollar,
            0x23 => VarSpace::Hash,
            0x60 => VarSpace::Backtick,
            other => VarSpace::Unknown(other),
        }
    }

    /// 原始前缀字节(存储键的一部分)。
    pub fn prefix(self) -> u8 {
        match self {
            VarSpace::At => 0x40,
            VarSpace::Dollar => 0x24,
            VarSpace::Hash => 0x23,
            VarSpace::Backtick => 0x60,
            VarSpace::Unknown(b) => b,
        }
    }
}

/// 变量引用:`[前缀字符][id:u16]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarRef {
    /// 变量空间(源码前缀)。
    pub space: VarSpace,
    /// 变量 id(u16 LE;id ↔ 名字的解析关系 Unknown —— 编译期已消解)。
    pub id: u16,
}

impl VarRef {
    /// 显示形式(`@1041` / `$F` / `#label` 字符化)。
    pub fn display(&self) -> String {
        let ch = match self.space {
            VarSpace::At => '@',
            VarSpace::Dollar => '$',
            VarSpace::Hash => '#',
            VarSpace::Backtick => '`',
            VarSpace::Unknown(b) => b as char,
        };
        format!("{ch}{}", self.id)
    }
}

/// YU-RIS 值。变量三类型:INT(64 位)/FLT(64 位)/STR(手册 §変数,**Confirmed**)。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 整数(INT,64 位)。
    Int(i64),
    /// 实数(FLT,IEEE f64)。
    Float(f64),
    /// 字符串(STR;**SJIS 字节原样保存**,编码转换在显示层)。
    Str(Vec<u8>),
}

impl Value {
    /// `+`。Int 环绕加(Likely);Str+Str = 字节拼接(手册 **Confirmed**);
    /// 混合数值提升 float(Likely)。
    pub fn add(&self, other: &Value) -> Result<Value> {
        if let (Value::Str(a), Value::Str(b)) = (self, other) {
            let mut v = a.clone();
            v.extend_from_slice(b);
            return Ok(Value::Str(v));
        }
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            _ => Err(Error::Unimplemented(
                "字符串与数值的混合 + 语义 Unknown(手册要求显式 $()/@() 转换)",
            )),
        }
    }

    /// `-`。
    pub fn sub(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a - *b as f64)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 - b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            _ => Err(Error::Unimplemented("字符串 - 语义 Unknown")),
        }
    }

    /// `*`。
    pub fn mul(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a * *b as f64)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 * b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            _ => Err(Error::Unimplemented("字符串 * 语义 Unknown")),
        }
    }

    /// `/`。
    ///
    /// **除零语义(2026-10-15 成果 133)**:Int 除零 → **返回 0,不炸 VM**。
    ///
    /// 勘误留痕:旧实现显式报错 → 实测会把整个 VM 打进 Error 态,boot 链从此
    /// 死锁(实测:<game-b> s71 stdlib `PER.GET` 的 LOOP 计数来自
    /// `SYSTEMINFO` 写回;未实装时=0 → 循环体跳过 → 累加器保持 0 →
    /// `($df*100)/$de` 除 0 → 全程不推进)。引擎这里同位置是 **int div
    /// 硬异常**(进程级崩溃),内核的"超集"口径 = 记 0 并继续,让链跑出
    /// 下一个真实缺口。`// UNVERIFIED`:引擎 0→0 假设由调用面实证替代 ——
    /// 触发处全部带 YURIS_DEBUG_FRAME 事件(见 VM 层日志),非静默。
    pub fn div(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Ok(Value::Int(0))
                } else {
                    Ok(Value::Int(a.wrapping_div(*b)))
                }
            }
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a / *b as f64)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 / b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            _ => Err(Error::Unimplemented("字符串 / 语义 Unknown")),
        }
    }

    /// `%`。Int 除零 → 你的语义同 `/`(成果 133;见 [`Value::div`])。
    ///
    /// **混合型(2026-10-16,成果 141)**:`Int % Float` / `Float % Int` 原先
    /// 落到 `_ => Unimplemented` ⇒ VM 进 Error ⇒ 启动链死在片头 LOGO 段。
    /// 触发实例:s66 `pc5396` 的 `INT[@4095 = @4095 % 100]`,而 `@4095` 在此
    /// 路径上是**浮点**(`es.EF.COM.*` 的百分比参数族)—— 声明型 `INT[…]`
    /// 是**转换意图**,不保证运行值已是 int。
    ///
    /// 混合时按 C 的"公共实数类型"提升为 float 取余(`f64::rem`)。
    /// `// UNVERIFIED`:引擎为 x87 实现,混合 `%` 的舍入方向未逐位取证;
    /// 该路径此前是硬错误(全链死),现在至少语义连续。若取证到不同舍入,
    /// 须在本处留勘误。
    pub fn modulo(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Ok(Value::Int(0))
                } else {
                    Ok(Value::Int(a.wrapping_rem(*b)))
                }
            }
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
            (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a % *b as f64)),
            (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 % b)),
            _ => Err(Error::Unimplemented("字符串 % 语义 Unknown")),
        }
    }

    /// `==`(数值或字符串字节相等;字符串 **Confirmed**)。
    pub fn eq(&self, other: &Value) -> Result<bool> {
        Ok(match (self, other) {
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Float(a), Value::Int(b)) | (Value::Int(b), Value::Float(a)) => {
                *a == *b as f64
            }
            (Value::Str(_), _) | (_, Value::Str(_)) => false,
        })
    }

    /// 排序比较前的数值检查。
    fn require_numeric(&self, other: &Value) -> Result<()> {
        if matches!(self, Value::Str(_)) || matches!(other, Value::Str(_)) {
            return Err(Error::Unimplemented(
                "字符串排序比较语义 Unknown(仅 ==/!= 有手册佐证)",
            ));
        }
        Ok(())
    }

    /// `!=`。
    pub fn ne(&self, other: &Value) -> Result<bool> {
        Ok(!self.eq(other)?)
    }

    /// `>=`。字符串排序 → Unknown 报错。
    pub fn ge(&self, other: &Value) -> Result<bool> {
        self.require_numeric(other)?;
        Ok(self.cmp_num(other) >= 0)
    }

    /// `<=`。
    pub fn le(&self, other: &Value) -> Result<bool> {
        self.require_numeric(other)?;
        Ok(self.cmp_num(other) <= 0)
    }

    /// `>`。
    pub fn gt(&self, other: &Value) -> Result<bool> {
        self.require_numeric(other)?;
        Ok(self.cmp_num(other) > 0)
    }

    /// `<`。
    pub fn lt(&self, other: &Value) -> Result<bool> {
        self.require_numeric(other)?;
        Ok(self.cmp_num(other) < 0)
    }

    fn cmp_num(&self, other: &Value) -> i64 {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => match a.cmp(b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            },
            _ => {
                let a = self.as_f64();
                let b = other.as_f64();
                match a.partial_cmp(&b) {
                    Some(std::cmp::Ordering::Less) => -1,
                    Some(std::cmp::Ordering::Equal) => 0,
                    _ => 1,
                }
            }
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            Value::Int(v) => *v as f64,
            Value::Float(v) => *v,
            Value::Str(_) => 0.0,
        }
    }

    /// 按位与(仅 Int;**Likely**,单字符 `&` 的精确语义待引擎对照)。
    pub fn bitand(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
            _ => Err(Error::Unimplemented("bitand 仅对 Int 实现(Float/Str Unknown)")),
        }
    }

    /// 按位或。
    pub fn bitor(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
            _ => Err(Error::Unimplemented("bitor 仅对 Int 实现")),
        }
    }

    /// 按位异或。
    pub fn xor(&self, other: &Value) -> Result<Value> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
            _ => Err(Error::Unimplemented("xor 仅对 Int 实现")),
        }
    }

    /// 逻辑与(真值合并;比较结果在栈上无短路 —— 编译器把两侧先行求值)。
    pub fn logand(&self, other: &Value) -> Result<Value> {
        Ok(Value::Int((self.is_truthy()? && other.is_truthy()?) as i64))
    }

    /// 逻辑或。
    pub fn logor(&self, other: &Value) -> Result<Value> {
        Ok(Value::Int((self.is_truthy()? || other.is_truthy()?) as i64))
    }

    /// 真值。Str 真值 → Unknown 报错。
    pub fn is_truthy(&self) -> Result<bool> {
        match self {
            Value::Int(v) => Ok(*v != 0),
            Value::Float(v) => Ok(*v != 0.0),
            Value::Str(_) => Err(Error::Unimplemented("字符串真值语义 Unknown")),
        }
    }

    /// 一元负号。
    pub fn neg(&self) -> Result<Value> {
        match self {
            Value::Int(v) => Ok(Value::Int(v.wrapping_neg())),
            Value::Float(v) => Ok(Value::Float(-v)),
            Value::Str(_) => Err(Error::Unimplemented("字符串取负 语义 Unknown")),
        }
    }

    /// `$()` 转字符串。Int → 十进制(手册 **Confirmed**);
    /// Float → 格式化规则 Unknown → 报错;Str → 原样。
    pub fn cast_to_str(&self) -> Result<Value> {
        match self {
            Value::Str(_) => Ok(self.clone()),
            Value::Int(v) => Ok(Value::Str(v.to_string().into_bytes())),
            Value::Float(_) => Err(Error::Unimplemented(
                "float→string 格式化规则 Unknown(引擎可能是 %f 系)",
            )),
        }
    }

    /// 取 Int(仅 i64;其他类型 → None,不猜)。
    pub fn as_int_opt(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// 字符串按 SJIS 解码的 String(lossy;用于事件字段展示)。
    pub fn str_as_string(&self) -> String {
        match self {
            Value::Str(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            _ => String::new(),
        }
    }

    /// `@()` 转整数。字符串全为十进制数字(可带负号)→ 数值
    /// (手册 `@("123"+"45") == 12345` **Confirmed**);
    /// 含非数字字符 → 手册称 β 版"不確定" → 显式报错;
    /// Float → 截断(**Likely**,C 惯例)。
    pub fn cast_to_int(&self) -> Result<Value> {
        match self {
            Value::Int(_) => Ok(self.clone()),
            Value::Float(v) => Ok(Value::Int(*v as i64)),
            Value::Str(bytes) => {
                let s = String::from_utf8_lossy(bytes);
                let t = s.trim();
                match t.parse::<i64>() {
                    Ok(v) => Ok(Value::Int(v)),
                    Err(_) => Err(Error::Unimplemented(
                        "非数字字符串转 int 的引擎规则 Unknown(手册:β 版不確定)",
                    )),
                }
            }
        }
    }
}

/// 数组元素类型(变量描述符 byte+1:1=INT 2=FLT 3=STR;引擎 Confirmed)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemType {
    /// 1 = INT(i64,8 字节/元素)。
    Int,
    /// 2 = FLT(f64,8 字节/元素)。
    Float,
    /// 3 = STR(变长字节串)。
    Str,
}

/// 数组存储:类型 + 各维边界 + 行主序元素(引擎 0x29 处理器 Confirmed:
/// 线性化 = Σ idx[k]·Π bounds[j<k],**最后一维最快**;越界 → 引擎报错 0x1a202)。
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayStorage {
    /// 元素类型。
    pub elem: ElemType,
    /// 各维边界(元素个数;来自 YSVR 维数/边界或 GOSUB gparam)。
    pub bounds: Vec<u32>,
    /// 行主序元素。长度 = bounds.product()(STR 数组每元素为字符串)。
    pub data: Vec<Value>,
}

impl ArrayStorage {
    /// 按边界构造并清零/置空。
    pub fn new(elem: ElemType, bounds: &[u32]) -> Self {
        let n: usize = bounds.iter().map(|&b| b as usize).product();
        let data = match elem {
            ElemType::Int => vec![Value::Int(0); n],
            ElemType::Float => vec![Value::Float(0.0); n],
            ElemType::Str => vec![Value::Str(Vec::new()); n],
        };
        Self { elem, bounds: bounds.to_vec(), data }
    }

    /// 行主序线性偏移(**Confirmed**:引擎 0x29,最后一维最快)。
    /// `indices.len() != bounds.len()` → 维数不匹配(引擎 0x1a20c);
    /// `idx[k] >= bounds[k]` → 越界(引擎 0x1a202)。
    pub fn linear_offset(bounds: &[u32], indices: &[i64]) -> Result<usize> {
        if indices.len() != bounds.len() {
            return Err(Error::format(format!(
                "array dimension mismatch: got {} indices, declared {}",
                indices.len(),
                bounds.len()
            )));
        }
        let mut offset: i64 = 0;
        let mut stride: i64 = 1;
        for k in (0..bounds.len()).rev() {
            let idx = indices[k];
            let b = bounds[k] as i64;
            if !(0..b).contains(&idx) {
                return Err(Error::format(format!(
                    "array index out of bounds: idx[{k}]={idx}, bound={b}"
                )));
            }
            offset += stride * idx;
            stride *= b;
        }
        Ok(offset as usize)
    }

    /// 读元素。
    pub fn get(&self, indices: &[i64]) -> Result<&Value> {
        let off = Self::linear_offset(&self.bounds, indices)?;
        self.data.get(off).ok_or_else(|| Error::format("linear offset out of range (内部错误)"))
    }

    /// 写元素。
    pub fn set(&mut self, indices: &[i64], v: Value) -> Result<()> {
        if !self.elem_matches(&v) {
            return Err(Error::format(format!(
                "element type mismatch: storing {:?} into {:?} array",
                v, self.elem
            )));
        }
        let off = Self::linear_offset(&self.bounds, indices)?;
        self.data[off] = v;
        Ok(())
    }

    fn elem_matches(&self, v: &Value) -> bool {
        matches!(
            (self.elem, v),
            (ElemType::Int, Value::Int(_))
                | (ElemType::Float, Value::Float(_))
                | (ElemType::Str, Value::Str(_))
        )
    }

    /// 把**全部元素**设为同一初值(YSVR 数组初值语义)。
    ///
    /// 引擎 `ysv.ybn` 的数组条目只记一个初值(`bounds=[20] init=-1`),脚本按任意
    /// 下标读取时都期望拿到该初值 —— 实测 s60 `es.SND.PLAY` 的每声道参数表
    /// (`@2927..@2936`,全部 `bounds=[20] init=-1/-99999`)按 `(@2971-1)/10`
    /// 取下标 0..14;若只有元素 0 带初值,其余元素为 0,脚本会把「未指定」
    /// 当成显式 0 传给 `SOUND`(`LOOP=0` = 无限循环)。
    pub fn fill(&mut self, v: Value) -> Result<()> {
        if !self.elem_matches(&v) {
            return Err(Error::format(format!(
                "element type mismatch: filling {:?} array with {:?}",
                self.elem, v
            )));
        }
        for slot in self.data.iter_mut() {
            *slot = v.clone();
        }
        Ok(())
    }
}

/// LET 复合赋值码(w0.B3,引擎 FUN_00443808 **Confirmed**):
/// 0=`=` 1=`+=` 2=`-=` 3=`*=` 4=`/=` 5=`%=` 6=`&=` 7=`|=` 8=`^=`。
pub fn compound_assign(code: u8, lhs: &Value, rhs: &Value) -> Result<Value> {
    match code {
        0 => Ok(rhs.clone()),
        1 => lhs.add(rhs),
        2 => lhs.sub(rhs),
        3 => lhs.mul(rhs),
        4 => lhs.div(rhs),
        5 => lhs.modulo(rhs),
        6 => lhs.bitand(rhs),
        7 => lhs.bitor(rhs),
        8 => lhs.xor(rhs),
        _ => Err(Error::format(format!("unknown compound assignment code {code}"))),
    }
}


/// 调试:`YURIS_WATCH_VAR=<十六进制 id>` 时打印该变量的每一次标量写入。
///
/// 用途:定位「谁把这个变量改成了这个值」—— 变量身份 = id 之后,同一个 id
/// 可能被不同前缀/不同路径写到,光看最终值分不出是哪一处下的手。
/// `YURIS_WATCH_VAR` 的**进程级快照**(成果 183)。
///
/// **这是实测出的头号热点**:旧实现每次调用都 `std::env::var(...)` ——
/// 而 `set_watch_site` 在**每组**执行前调用一次(boot 单帧 85 万组、
/// 稳态 1.7 万组/帧)。Windows 的 `GetEnvironmentVariableW` 要拿进程
/// 环境锁 + UTF-16 转换,单次约 200–400ns ⇒ 稳态每帧 3–7ms、boot 更深。
///
/// 环境在进程生命周期内不变 ⇒ 一次快照,之后只读 `OnceLock`。
fn watch_var() -> Option<u16> {
    static GATE: std::sync::OnceLock<Option<u16>> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| {
        let s = std::env::var("YURIS_WATCH_VAR").ok()?;
        u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()
    })
}

/// 当前执行组 `(script_id, pc)`,由 VM 在每组派发前登记,供写入日志标注来源。
///
/// 只在 `YURIS_WATCH_VAR` 开启时才写 —— 关闭时 [`set_watch_site`] 直接返回,
/// 不碰热路径(单帧可达数十万组)。
static WATCH_SITE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

/// 登记当前执行组(诊断用;`YURIS_WATCH_VAR` 未开启时为空操作)。
pub fn set_watch_site(script_id: u16, pc: usize) {
    if watch_var().is_none() {
        return;
    }
    let packed = ((script_id as u64) << 32) | (pc as u64 & 0xffff_ffff);
    WATCH_SITE.store(packed, std::sync::atomic::Ordering::Relaxed);
}

fn watch_site() -> String {
    let packed = WATCH_SITE.load(std::sync::atomic::Ordering::Relaxed);
    if packed == u64::MAX {
        return "?".to_string();
    }
    format!("s{} pc{}", packed >> 32, packed & 0xffff_ffff)
}

#[derive(Debug, Clone)]
/// 变量存储:标量按 id 直接索引,字符串/数组走辅助表(见各字段)。
pub struct VariableStore {
    /// **标量:按 id 直接索引**(`vars[id]`)。
    ///
    /// **语义(Confirmed)**:变量身份只有 id —— 引擎描述符表是
    /// `DAT_0087240c[id]`,**没有前缀维**(`docs/engine/command-layer.md` §9);
    /// 前缀字节是编译期名字解析的产物(见 [`fallback_key`])。
    ///
    /// **为什么从 `HashMap<(u8,u16), Value>` 改过来(2026-10-17,成果 184)**:
    /// 旧实现的 `scalar_key()` 每次访问做 1 次 `contains_key` + **未命中时**
    /// 一次全表 `fallback_key` 扫描;`LET` 实测占全部组的 **40.4%**
    /// (345,450/854,124),读写各一次 ⇒ 每帧约 70 万次哈希查找 +
    /// 潜在的 O(n) 扫描。改数组后是纯索引,无哈希、无分配。
    ///
    /// 语义等价性:旧实现里 `set` 会把不同前缀的写**写穿到同一 id 的
    /// 唯一登记**(测试 `scalar_prefix_fallback_and_write_through`),
    /// 因此标量本就只有「一个 id 一份存储」—— 数组是对该事实的直接表达。
    vars: Vec<Option<Value>>,
    /// **数组仍按 `(前缀,id)` 存**:声明窗可能给同一 id 建两份不同前缀的
    /// 数组(不同元素类型/边界),此时按「有歧义 → 不解析」处理
    /// (测试 `ambiguous_array_id_is_not_resolved`)。数组访问只占 2.7%
    /// (`VARACT`),不在本轮热路径上。
    arrays: HashMap<(u8, u16), ArrayStorage>,
}

/// 变量身份 = **id**;前缀字节是编译期名字解析的产物,不参与寻址。
///
/// **引擎侧依据（Confirmed）**：`docs/engine/command-layer.md` §9 —— 变量描述符表是
/// `DAT_0087240c[id]`，**只有 id 一维**；`ysv.ybn`（YSVR）条目只有 `var_id`、没有前缀字段。
/// **编译器侧依据**：`docs/reverse/yscom-compiler-notes.md` §4.3 —— 操作数 3 字节 =
/// `[前缀字符][id:u16]`，前缀是 fixup 回填写入的**名字解析结果**。
///
/// **实测触发点（本机，第三方发行版 v494）**：前缀 `` ` ``(0x60) 全语料只出现 5 次
/// （全在 s91 `ES.DEBUG.DEFINE`），且**没有任何声明窗**；而 `id 0x0489` 在 YSVR 里是
/// `INT[8192]`（`ysvr_dump` 实测 2568/2568 个 id 零重复、零类型冲突）——
/// 引擎只有按 id 寻址才可能读到该值。
///
/// **回退规则**：精确 `(前缀,id)` 未登记时，若该 id 在其**他前缀**下恰好只有一处登记，
/// 则解析到它；**多于一处 = 有歧义 → 不解析（不猜）**。
fn fallback_key<'a, I>(exact: (u8, u16), keys: I) -> Option<(u8, u16)>
where
    I: Iterator<Item = &'a (u8, u16)>,
{
    let mut hit: Option<(u8, u16)> = None;
    for k in keys {
        if k.1 != exact.1 || *k == exact {
            continue;
        }
        match hit {
            None => hit = Some(*k),
            Some(prev) if prev == *k => {}
            Some(_) => return None,
        }
    }
    hit
}

impl Default for VariableStore {
    fn default() -> Self {
        Self {
            vars: Vec::new(),
            arrays: HashMap::new(),
        }
    }
}

impl VariableStore {
    /// 空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 数组存储键：精确 `(前缀,id)` → 该 id 的唯一条目（见 [`fallback_key`]）。
    fn array_key(&self, r: &VarRef) -> (u8, u16) {
        let exact = (r.space.prefix(), r.id);
        if self.arrays.contains_key(&exact) {
            return exact;
        }
        fallback_key(exact, self.arrays.keys()).unwrap_or(exact)
    }

    /// 读取；未定义 → 显式报错（引擎默认值行为 Unknown，不猜）。
    pub fn get(&self, r: &VarRef) -> Result<&Value> {
        self.vars
            .get(r.id as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| Error::format(format!("undefined variable {}", r.display())))
    }

    /// 读取（可选）。
    pub fn get_opt(&self, r: &VarRef) -> Option<&Value> {
        self.vars.get(r.id as usize).and_then(Option::as_ref)
    }

    /// 写入（LET / 命令参数求值的结果）。
    ///
    /// 按 **id** 写入（前缀不参与寻址;见 [`Self`] 的字段文档）。
    pub fn set(&mut self, r: &VarRef, v: Value) {
        let i = r.id as usize;
        if watch_var() == Some(r.id) {
            yuris_log::trace_forced!(
                "[watch] {} 写 {}{} = {:?}(请求前缀 {:#04x})",
                watch_site(),
                r.space.prefix() as char,
                r.id,
                v,
                r.space.prefix()
            );
        }
        if i >= self.vars.len() {
            // 稀疏 id:按需扩到 id+1(引擎的描述符表同样是 id 索引;
            // 语料最大 id ≈ 0x0bb9,单槽 Value ≈ 32B ⇒ 全表 < 200KB)。
            self.vars.resize_with(i + 1, || None);
        }
        self.vars[i] = Some(v);
    }

    /// 声明/重置数组（YSVR 消费或 GOSUB 帧局部）。
    /// 幂等：重复声明同型数组不重置（引擎：声明组只在加载期消费一次）。
    pub fn declare_array(&mut self, r: &VarRef, elem: ElemType, bounds: &[u32]) {
        let key = (r.space.prefix(), r.id);
        if let Some(a) = self.arrays.get(&key) {
            if a.elem == elem && a.bounds == bounds {
                return;
            }
        }
        self.arrays.insert(key, ArrayStorage::new(elem, bounds));
    }

    /// 读数组元素（标量数组 = 单下标；未声明 → 报错，同引擎 0x1a5ea）。
    pub fn get_elem(&self, r: &VarRef, indices: &[i64]) -> Result<&Value> {
        let a = self.arrays.get(&self.array_key(r)).ok_or_else(|| {
            Error::format(format!(
                "undefined array variable {} (引擎 0x1a5ea 同族)",
                r.display()
            ))
        })?;
        a.get(indices).map_err(|e| {
            Error::format(format!(
                "{} (array {}, dims {:?})",
                e,
                r.display(),
                a.bounds
            ))
        })
    }

    /// 写数组元素。
    pub fn set_elem(&mut self, r: &VarRef, indices: &[i64], v: Value) -> Result<()> {
        let key = self.array_key(r);
        if watch_var() == Some(r.id) {
            yuris_log::trace_forced!(
                "[watch] {} 写元素 {}{}{:?} = {:?}",
                watch_site(),
                r.space.prefix() as char,
                r.id,
                indices,
                v
            );
        }
        match self.arrays.get_mut(&key) {
            Some(a) => {
                // 引擎写入按**描述符类型**收敛(变量无独立类型,desc 定型):
                // FLT 数组收 Int → 转 f64;INT 数组收 Float → 四舍五入成 i64
                // (引擎 00443808 FLT/INT 描述符分支各自转存;成果 59g)。
                let cv = match (&a.elem, v) {
                    (ElemType::Float, Value::Int(i)) => Value::Float(i as f64),
                    (ElemType::Int, Value::Float(f)) => Value::Int(f.round() as i64),
                    (_, other) => other,
                };
                let bounds = a.bounds.clone();
                a.set(indices, cv).map_err(|e| {
                    Error::format(format!(
                        "{} (array {}, dims {:?})",
                        e,
                        r.display(),
                        bounds
                    ))
                })
            }
            None => Err(Error::format(format!(
                "undefined array variable {}",
                r.display()
            ))),
        }
    }

    /// 把整个数组填成同一初值(YSVR 数组初值;见 [`ArrayStorage::fill`])。
    pub fn fill_array(&mut self, r: &VarRef, v: Value) -> Result<()> {
        let key = self.array_key(r);
        if watch_var() == Some(r.id) {
            yuris_log::trace_forced!(
                "[watch] {} 填充数组 {}{} = {:?}",
                watch_site(),
                r.space.prefix() as char,
                r.id,
                v
            );
        }
        match self.arrays.get_mut(&key) {
            Some(a) => {
                let cv = match (&a.elem, v) {
                    (ElemType::Float, Value::Int(i)) => Value::Float(i as f64),
                    (ElemType::Int, Value::Float(f)) => Value::Int(f.round() as i64),
                    (_, other) => other,
                };
                a.fill(cv)
            }
            None => Err(Error::format(format!(
                "undefined array variable {}",
                r.display()
            ))),
        }
    }

    /// 移除该 id 的**全部**数组登记,返回移除条数。
    ///
    /// **引擎侧依据**:一个变量的描述符只有一份(`DAT_0087240c[id]`,
    /// 字段 `{+0 类别, +1 类型, +2 维数, +4.. 边界}`),**标量与数组不可能并存** ——
    /// `维数==0` 是标量,`>0` 才是数组。
    ///
    /// 实测触发点(2026-10-13):`id 0x0cf6` 被声明窗建成 **FLT 数组**,而游戏自己的
    /// YSVR 表写的是 **INT 标量**(`ty=1, bounds=[]`)—— 两者在 Rust 侧各占一张表,
    /// 于是"同一个 id 既标量又数组";`load_auto` 优先数组分支 → 读第 0 元素失败 →
    /// 退回类型默认 `Float(0.0)` → 该变量作数组下标时炸「非整数下标」。
    /// YSVR 后于声明应用(boot 顺序),故 YSVR 的形态即终态。
    pub fn clear_arrays_for_id(&mut self, id: u16) -> usize {
        let before = self.arrays.len();
        self.arrays.retain(|(_, i), _| *i != id);
        before - self.arrays.len()
    }

    /// 移除该 id 的标量值,返回移除条数(0 或 1;与
    /// [`Self::clear_arrays_for_id`] 对称)。
    ///
    /// 数组按 id 直接索引后「条数」恒 ≤1(一个 id 一份标量存储)。
    pub fn clear_scalars_for_id(&mut self, id: u16) -> usize {
        let i = id as usize;
        match self.vars.get_mut(i) {
            Some(slot @ Some(_)) => {
                *slot = None;
                1
            }
            _ => 0,
        }
    }

    /// 数组是否存在(描述符检查;引擎:id>999 需描述符非零,否则 0x1a5ea)。
    pub fn has_array(&self, r: &VarRef) -> bool {
        self.arrays.contains_key(&self.array_key(r))
    }

    /// 数组各维边界(未声明 → None)。
    pub fn array_dims(&self, r: &VarRef) -> Option<Vec<u32>> {
        self.arrays.get(&self.array_key(r)).map(|a| a.bounds.clone())
    }

    /// 数组元素类型(未声明 → None)。
    pub fn array_elem_type(&self, r: &VarRef) -> Option<ElemType> {
        self.arrays.get(&self.array_key(r)).map(|a| a.elem)
    }

    /// 数组扁平数据只读视图(SAVE 序列化用;行主序,与
    /// [`Self::load_array_data`] 的填充顺序一致)。
    pub fn array_data(&self, r: &VarRef) -> Option<&[Value]> {
        self.arrays.get(&self.array_key(r)).map(|a| a.data.as_slice())
    }

    /// 调整数组为 `new_bounds`(保留原有元素,新增元素清零;
    /// 引擎 FUN_00454710 realloc 的保留行为 **Likely**)。
    /// 维数必须与原数组一致(引擎 VARACT DIMSIZE 要求维数==1,0x18cb4 族)。
    pub fn resize_array(&mut self, r: &VarRef, new_bounds: &[u32]) -> Result<()> {
        let key = self.array_key(r);
        let Some(old) = self.arrays.get(&key) else {
            return Err(Error::format(format!(
                "resize_array: undefined array {}{}",
                r.space.prefix(),
                r.id
            )));
        };
        if old.bounds.len() != new_bounds.len() {
            return Err(Error::format(format!(
                "resize_array: dim mismatch (array has {}, got {})",
                old.bounds.len(),
                new_bounds.len()
            )));
        }
        let mut next = ArrayStorage::new(old.elem, new_bounds);
        // 行主序前缀保留(两数组前缀布局一致)
        let n = old.data.len().min(next.data.len());
        next.data[..n].clone_from_slice(&old.data[..n]);
        self.arrays.insert(key, next);
        Ok(())
    }

    /// YSSD/LOAD 整块写回(引擎 FUN_0044564d 等价:memcpy 到描述符数据区)。
    ///
    /// `values` 元素数 ≤ 声明容量(引擎 memcpy 无界,VM 显式防护);少于
    /// 容量 = 前缀覆盖、其余保留(引擎部分拷贝语义)。类型收敛同
    /// [`Self::set_elem`]。
    pub fn load_array_data(&mut self, r: &VarRef, values: Vec<Value>) -> Result<()> {
        let key = self.array_key(r);
        let Some(a) = self.arrays.get_mut(&key) else {
            return Err(Error::format(format!(
                "load_array_data: undefined array {}{}",
                r.space.prefix(),
                r.id
            )));
        };
        if values.len() > a.data.len() {
            return Err(Error::format(format!(
                "load_array_data: {} values exceed capacity {} ({}{})",
                values.len(),
                a.data.len(),
                r.space.prefix(),
                r.id
            )));
        }
        for (i, v) in values.into_iter().enumerate() {
            let cv = match (&a.elem, v) {
                (ElemType::Float, Value::Int(i)) => Value::Float(i as f64),
                (ElemType::Int, Value::Float(f)) => Value::Int(f.round() as i64),
                (_, other) => other,
            };
            a.data[i] = cv;
        }
        Ok(())
    }

    /// 已定义变量数(**非空槽**计数,与旧的 HashMap 语义一致)。
    pub fn len(&self) -> usize {
        self.vars.iter().filter(|v| v.is_some()).count()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.vars.iter().all(|v| v.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: &[u8]) -> Value {
        Value::Str(b.to_vec())
    }

    // ---- 变量身份 = id(前缀字节不参与寻址;勘误见 `fallback_key`)----

    #[test]
    fn var_prefix_is_not_part_of_identity() {
        // 引擎侧:描述符表 DAT_0087240c[id] 无前缀维;YSVR 条目只有 var_id。
        // 实测触发点:`` `[0x0489] `` 只在 s91 出现、**无声明窗**,而该 id 在
        // YSVR 里是 INT[8192] —— 前缀 0x60 必须能落到 0x40 的登记上。
        let mut st = VariableStore::new();
        let at = VarRef { space: VarSpace::At, id: 0x0489 };
        st.declare_array(&at, ElemType::Int, &[8192]);
        st.set_elem(&at, &[1], Value::Int(7)).unwrap();

        let bt = VarRef { space: VarSpace::Backtick, id: 0x0489 };
        assert_eq!(st.get_elem(&bt, &[1]).unwrap(), &Value::Int(7));
        assert!(st.has_array(&bt));
        assert_eq!(st.array_dims(&bt), Some(vec![8192]));
    }

    #[test]
    fn scalar_prefix_fallback_and_write_through() {
        let mut st = VariableStore::new();
        let at = VarRef { space: VarSpace::At, id: 42 };
        st.set(&at, Value::Int(5));
        let dollar = VarRef { space: VarSpace::Dollar, id: 42 };
        assert_eq!(st.get(&dollar).unwrap(), &Value::Int(5));
        // 写也落到同一份存储(引擎:一个 id 只有一份描述符/存储)
        st.set(&dollar, Value::Int(9));
        assert_eq!(st.get(&at).unwrap(), &Value::Int(9));
    }

    #[test]
    fn ambiguous_array_id_is_not_resolved() {
        // 同一 id 在两个前缀下**各有一份数组登记** = 有歧义 → 不猜(保持未定义)。
        // (标量侧到不了这个状态:`set` 的写穿保证一个 id 只有一份标量存储。)
        let mut st = VariableStore::new();
        st.declare_array(&VarRef { space: VarSpace::At, id: 7 }, ElemType::Int, &[4]);
        st.declare_array(&VarRef { space: VarSpace::Dollar, id: 7 }, ElemType::Str, &[4]);
        let hash = VarRef { space: VarSpace::Hash, id: 7 };
        assert!(!st.has_array(&hash));
        assert!(st.get_elem(&hash, &[0]).is_err());
    }

    #[test]
    fn unknown_id_still_errors() {
        let st = VariableStore::new();
        assert!(st.get(&VarRef { space: VarSpace::Backtick, id: 1234 }).is_err());
    }

    #[test]
    fn manual_confirmed_semantics() {
        // 手册: $S = "ABC" + ".BMP"
        assert_eq!(
            s(b"ABC").add(&s(b".BMP")).unwrap(),
            s(b"ABC.BMP")
        );
        // 手册: @("123"+"45") == 12345
        let n = s(b"123").add(&s(b"45")).unwrap();
        assert_eq!(n.cast_to_int().unwrap(), Value::Int(12345));
        // 手册: $() 数值转字符串
        assert_eq!(Value::Int(123).cast_to_str().unwrap(), s(b"123"));
        // 手册: 字符串 == 比较(りんご 的 SJIS 字节)
        let ringo: &[u8] = &[0x82, 0xE8, 0x82, 0xF1, 0x82, 0xB2];
        assert!(s(ringo).eq(&s(ringo)).unwrap());
        // 手册: 64 位字面量 0xFFFFFFFFFFFFFFFF → -1(字面量已在编译期环绕)
        assert_eq!(Value::Int(-1).cast_to_str().unwrap(), s(b"-1"));
    }

    #[test]
    fn likely_semantics_are_marked_and_working() {
        // 混合精度提升(Likely)
        assert_eq!(
            Value::Int(30).mul(&Value::Float(0.5)).unwrap(),
            Value::Float(15.0)
        );
        // 比较结果 1/0(Likely)
        assert_eq!(Value::Int(5).ge(&Value::Int(5)).unwrap(), true);
        assert_eq!(Value::Int(4).lt(&Value::Int(5)).unwrap(), true);
        // 截断(Likely)
        assert_eq!(Value::Float(1.9).cast_to_int().unwrap(), Value::Int(1));
    }

    #[test]
    fn unknown_semantics_error_out() {
        assert!(Value::Str(vec![0x82, 0xA0]).is_truthy().is_err()); // 字符串真值(あ SJIS)
        assert!(Value::Str(b"abc".to_vec()).cast_to_int().is_err()); // 非数字
        assert!(Value::Float(1.0).cast_to_str().is_err()); // float 格式化
        // 除零 = 0(成果 133;旧口径显式报错,实测炸 VM 死链,已勘误)
        assert_eq!(Value::Int(1).div(&Value::Int(0)).unwrap(), Value::Int(0));
        assert!(Value::Str(b"a".to_vec()).lt(&Value::Str(b"b".to_vec())).is_err()); // 字符串排序
    }

    #[test]
    fn store_roundtrip() {
        let mut st = VariableStore::new();
        let r = VarRef { space: VarSpace::At, id: 0x0919 };
        assert!(st.get(&r).is_err()); // 未定义 → 报错
        st.set(&r, Value::Int(250));
        assert_eq!(st.get(&r).unwrap(), &Value::Int(250));
        let r2 = VarRef { space: VarSpace::Dollar, id: 4 };
        st.set(&r2, s(b"GAME"));
        assert_eq!(st.get(&r2).unwrap(), &s(b"GAME"));
        assert_eq!(st.len(), 2);
    }
}

#[cfg(test)]
mod array_tests {
    use super::*;

    /// 行主序线性化(引擎 0x29 Confirmed:最后一维最快)。
    #[test]
    fn row_major_linearization() {
        // 2×3 数组:idx=(r,c) → r*3+c
        let bounds = [2u32, 3];
        assert_eq!(ArrayStorage::linear_offset(&bounds, &[0, 0]).unwrap(), 0);
        assert_eq!(ArrayStorage::linear_offset(&bounds, &[0, 2]).unwrap(), 2);
        assert_eq!(ArrayStorage::linear_offset(&bounds, &[1, 0]).unwrap(), 3);
        assert_eq!(ArrayStorage::linear_offset(&bounds, &[1, 2]).unwrap(), 5);
        // 三维:2×3×4,idx=(1,1,1) = 1*(3*4)+1*4+1 = 17
        assert_eq!(
            ArrayStorage::linear_offset(&[2, 3, 4], &[1, 1, 1]).unwrap(),
            17
        );
    }

    #[test]
    fn bounds_and_dimension_errors() {
        // 越界(引擎 0x1a202)
        assert!(ArrayStorage::linear_offset(&[2, 3], &[2, 0]).is_err());
        assert!(ArrayStorage::linear_offset(&[2, 3], &[0, 3]).is_err());
        assert!(ArrayStorage::linear_offset(&[2, 3], &[-1, 0]).is_err());
        // 维数不匹配(引擎 0x1a20c)
        assert!(ArrayStorage::linear_offset(&[2, 3], &[1]).is_err());
        assert!(ArrayStorage::linear_offset(&[2, 3], &[1, 1, 1]).is_err());
    }

    #[test]
    fn array_get_set_roundtrip() {
        let mut a = ArrayStorage::new(ElemType::Int, &[2, 3]);
        a.set(&[1, 2], Value::Int(42)).unwrap();
        assert_eq!(a.get(&[1, 2]).unwrap(), &Value::Int(42));
        assert_eq!(a.get(&[0, 0]).unwrap(), &Value::Int(0)); // 清零初始化
        // 类型不匹配拒绝
        assert!(a.set(&[0, 0], Value::Float(1.0)).is_err());
        // FLT 数组清零
        let f = ArrayStorage::new(ElemType::Float, &[2]);
        assert_eq!(f.get(&[1]).unwrap(), &Value::Float(0.0));
        // STR 数组置空串
        let s = ArrayStorage::new(ElemType::Str, &[2]);
        assert_eq!(s.get(&[1]).unwrap(), &Value::Str(Vec::new()));
    }

    /// LET 复合赋值码(引擎 FUN_00443808 Confirmed:B3 0..8)。
    #[test]
    fn compound_assign_codes() {
        let l = Value::Int(10);
        assert_eq!(compound_assign(0, &l, &Value::Int(3)).unwrap(), Value::Int(3));
        assert_eq!(compound_assign(1, &l, &Value::Int(3)).unwrap(), Value::Int(13));
        assert_eq!(compound_assign(2, &l, &Value::Int(3)).unwrap(), Value::Int(7));
        assert_eq!(compound_assign(3, &l, &Value::Int(3)).unwrap(), Value::Int(30));
        assert_eq!(compound_assign(4, &l, &Value::Int(3)).unwrap(), Value::Int(3));
        assert_eq!(compound_assign(5, &l, &Value::Int(3)).unwrap(), Value::Int(1));
        assert_eq!(compound_assign(6, &l, &Value::Int(3)).unwrap(), Value::Int(2));
        assert_eq!(compound_assign(7, &l, &Value::Int(3)).unwrap(), Value::Int(11));
        assert_eq!(compound_assign(8, &l, &Value::Int(3)).unwrap(), Value::Int(9));
        // 字符串 +=(手册 Confirmed 的拼接语义)
        let s = Value::Str(b"ABC".to_vec());
        assert_eq!(
            compound_assign(1, &s, &Value::Str(b".BMP".to_vec())).unwrap(),
            Value::Str(b"ABC.BMP".to_vec())
        );
        assert!(compound_assign(9, &l, &Value::Int(1)).is_err());
    }

    /// VariableStore 的数组 API(declare 幂等 / get_elem / set_elem / 未声明报错)。
    #[test]
    fn store_array_api() {
        let mut st = VariableStore::new();
        let r = VarRef { space: VarSpace::At, id: 5000 };
        assert!(!st.has_array(&r));
        st.declare_array(&r, ElemType::Int, &[4]);
        assert!(st.has_array(&r));
        st.set_elem(&r, &[2], Value::Int(9)).unwrap();
        assert_eq!(st.get_elem(&r, &[2]).unwrap(), &Value::Int(9));
        // 幂等重声明:数据保留
        st.declare_array(&r, ElemType::Int, &[4]);
        assert_eq!(st.get_elem(&r, &[2]).unwrap(), &Value::Int(9));
        // 未声明 → 报错(引擎 0x1a5ea 同族)
        let r2 = VarRef { space: VarSpace::At, id: 5001 };
        assert!(st.get_elem(&r2, &[0]).is_err());
        // 未定义标量仍报错
        assert!(st.get(&r2).is_err());
    }
}
