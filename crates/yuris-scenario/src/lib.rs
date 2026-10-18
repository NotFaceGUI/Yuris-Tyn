//! # yuris-scenario
//!
//! P7.2:sc.ypf 明文剧本(`scenario\*.txt`)的**数据侧**词法/行解析器。
//!
//! 输入为封包条目解压后的原始字节(zlib 解压产物),经指定编码解码为 UTF-8 后
//! 按行产出元素流。**不猜引擎语义**:只做语法结构解析,未知命令一律接受
//! (32 种已定性命令之外留扩展),语法表之外无法解析的结构报错并给出行号。
//!
//! ## 语法(成果 49,数据侧 Confirmed;全库 36 文件侦查复核)
//!
//! ```text
//! #标签                       段/跳转目标;独立成行;字符集 [A-Za-z0-9_]
//! \CMD(arg,arg,...)           逗号分隔参数;空槽 = Empty(跳位)
//! \CMD.MOD1.MOD2(...)         修饰符链(可多级,\GO.G.IF 实证两级)
//! \CMD                        无括号形式(\GO.TITLE、\END、\TITLE 等 7 种实证)
//! (ID:3481)\LE("…")\LT("…")   对话行:(ID:n) 行号标记,可位于行首或 \VO(...) 之后
//! \VO(...)(ID:n)\LE(...)\LT(...)  语音联动形态(语料主流形态)
//! // 行注释                   /* 块注释(可跨行) */
//! 裸文本行                    对话裸文本(可带 (ID:n) 前缀;全库 0 例,语法保留)
//! ```
//!
//! 参数形态:整数(`-600`)、浮点(`2.0`,全库参数位 0 例,语法保留)、
//! 双引号字符串(`"=="`,串内可含逗号/括号/CJK,不含引号与反斜杠)、
//! 空槽(`\BGM(,800)` 首槽)、无引号裸词(`white`/`SCENARIO_MAIN`/`pac/op.ypf`)。
//! 参数两侧空白剥离(`\S(logo, item/logo_wp ,1100,…)` → `item/logo_wp`)。
//!
//! ## 编码说明(重要,实证勘误)
//!
//! 成果 49 曾记「SJIS 明文直读」——本次实现全库复核**证伪**:本样本
//! (繁中版)36 文件中 34 个含高位字节的文件**SJIS 严格解码全部失败**,
//! 而 **Big5(CP950)全部成功**(如 maho2_22 的 `\LT` 文本解码为繁体中文)。
//! YU-RIS 引擎本身为日文引擎(日文原版应为 SJIS),剧本编码随发行版本地化
//! 而定。因此提供三个入口(2026-10-16 起生产路径用第三个):
//!
//! - [`parse_scenario_auto`]: **数据侧判定**(新,生产路径);
//! - [`parse_scenario`]: 默认 **SJIS**(任务规格/日文原版默认);
//! - [`parse_scenario_with`]: 显式指定编码(诊断/带外信息场景)。
//!
//! 解码失败(非法字节序列)**报错**并给出行号,不做替换字符静默吞。
//!
//! ## 行结构模型
//!
//! 每行(剥注释、剥首尾空白后)产出一个或多个元素:
//! - `#…` 行 → [`Element::Label`](独立成行);
//! - 其他行 → 令牌序列:命令 `\CMD…`、行号标记 `(ID:n)`;
//!   行首裸文本 → [`Element::Dialogue`](吞到行尾);
//!   行首 `(ID:n)` 后跟裸文本 → `Dialogue.id` 前缀;
//!   命令/标记之后出现裸文本 = 未证实语法 → 报错。
//!
//! 语料实证的对话主流形态 `\VO(x)(ID:n)\LE("…")\LT("…")` 产出
//! `Command(VO) + LineId(n) + Command(LE) + Command(LT)` 四个元素。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod parser;

use std::collections::HashMap;

use encoding_rs::Encoding;

/// crate 版本(与 workspace 同步)。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 剧本解析错误。所有变体都携带 1 基原始行号(与源文件行号一致,
/// 含注释/空行在内的物理行)。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ScenarioError {
    /// 指定编码解码失败(存在非法字节序列)。
    #[error("第 {line} 行:{encoding} 解码失败(非法字节序列)")]
    Decode {
        /// 1 基行号。
        line: usize,
        /// 编码名(encoding_rs 标准名,如 "Shift_JIS" / "Big5")。
        encoding: &'static str,
    },
    /// 语法错误(语法表之外无法解析的结构)。
    #[error("第 {line} 行:{message}")]
    Syntax {
        /// 1 基行号。
        line: usize,
        /// 错误描述。
        message: String,
    },
}

/// 解析结果别名。
pub type Result<T> = std::result::Result<T, ScenarioError>;

/// 命令参数(数据侧;不解释槽位语义)。
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// 整数(含负号,如 `-600`)。
    Int(i64),
    /// 浮点(如 `2.0`;全库参数位 0 例,语法保留)。
    Float(f64),
    /// 字符串:双引号串(已剥引号)或无引号裸词(`white`/`SCENARIO_MAIN`)。
    Str(String),
    /// 空槽(跳位,如 `\BGM(,800)` 首槽、`\BG(white,200,0,,,,1)` 中段)。
    Empty,
}

/// 行内元素(按出现顺序扁平产出;每元素携带 1 基行号)。
#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    /// `#标签` 段定义(独立成行)。
    Label {
        /// 1 基行号。
        line: usize,
        /// 标签名(不含 `#`)。
        name: String,
    },
    /// `\CMD.MOD1.MOD2(参数,…)` 命令调用(无括号形式参数为空)。
    Command {
        /// 1 基行号。
        line: usize,
        /// 命令名(首个名段,如 `GO`/`S`/`LOGO`)。
        name: String,
        /// 修饰符链(`\GO.G.IF` → `["G","IF"]`;可多级)。
        modifiers: Vec<String>,
        /// 参数列表(`()` = 零参数)。
        params: Vec<Param>,
    },
    /// `(ID:n)` 对话行号标记(可位于行首或 `\VO(...)` 之后)。
    LineId {
        /// 1 基行号。
        line: usize,
        /// 行号 id。
        id: u32,
    },
    /// 对话裸文本行(可选 `(ID:n)` 前缀 + 裸文本;全库 0 例,语法保留)。
    Dialogue {
        /// 1 基行号。
        line: usize,
        /// 行首 `(ID:n)` 前缀(存在时)。
        id: Option<u32>,
        /// 裸文本(已剥首尾空白,吞到行尾)。
        text: String,
    },
}

/// 解析结果:元素流 + 段标签表。
#[derive(Debug, Clone)]
pub struct Scenario {
    /// 按行序产出的元素流(注释/空行不产出元素)。
    pub elements: Vec<Element>,
    /// 段标签表:标签名 → `elements` 下标(指向对应 [`Element::Label`];
    /// 跳转目标查询用。同名单文件内重复时后出现者覆盖 —— 语料全库无重复)。
    pub labels: HashMap<String, usize>,
}

/// 解析剧本(默认 **SJIS** 编码,日文原版/任务规格默认)。
///
/// 本样本(繁中版)剧本实为 Big5 编码,请用 [`parse_scenario_with`] 传
/// `encoding_rs::BIG5`(见模块文档「编码说明」)。
pub fn parse_scenario(bytes: &[u8]) -> Result<Scenario> {
    parse_scenario_with(bytes, encoding_rs::SHIFT_JIS)
}

/// 解析剧本(显式指定文本编码)。
///
/// 编解码按行进行:某行存在指定编码的非法字节序列时报
/// [`ScenarioError::Decode`] 并携带该行行号(不做替换字符静默吞)。
pub fn parse_scenario_with(bytes: &[u8], encoding: &'static Encoding) -> Result<Scenario> {
    parser::parse(bytes, encoding)
}

// ---------------------------------------------------------------------------
// 跨层文本编码策略(T1 + T21 一次定稿;数据侧判定,全层唯一入口)
// ---------------------------------------------------------------------------
//
// 历史缺陷(2026-10-16 审查):编码在两个层各写死一个方向且**互相矛盾** ——
// 剧本层写死 Big5(`player-core/scenario.rs`,繁中样本),VM 文本层写死 SJIS
// (`player-core/lib.rs` `WORD` 显示,日文引擎默认)。两个方向各自只因台词
// 恰好走另一条路径而未爆:CP932 作品走剧本层即整篇乱码,Big5 作品走 VM 层
// 即整篇乱码。
//
// 定稿策略:**数据侧判定、全层生效**。判定规则(确定性):
//
// 1. strict 解码只有一方**无非法字节** → 取该方;
// 2. 两方都无非法字节(歧义)→ 取**半角片假名更少**的一方
//    (见下「歧义票」);数量相同 → SJIS(引擎默认);
// 3. 两方都有非法字节 → **报错**(不静默替换、不猜)。
//
// 为什么 SJIS 优先:YU-RIS 是日文引擎,日文原版 / 官方 SDK 语料(SJIS/CP932)
// 是规范形态;Big5 只在繁中本地化版本出现(样本实测:36 文件中 34 个含高位
// 字节的文件 SJIS 严格解码全部失败而 Big5 全部成功 —— 即对该语料两方向
// 在**文件级**互斥,优先序不引入歧义)。
//
// **歧义票(为何需要,证据等级 Likely)**:Big5 汉字(= 高位双字节)可能恰好
// 落在 SJIS 的合法编码空间里 —— 实测(2026-10-16 探针,`tmp_enc_probe`):
//
// | 文本 | Big5 字节 | SJIS 严格解码 | Big5 严格解码 |
// |---|---|---|---|
// | `測試` | `b4 fa b8 d5` | `ｴ弴ﾕ`(合法!)| `測試` |
// | `開始` | `b6 7d a9 6c` | `ｶ}ｩl`(合法!)| `開始` |
// | `中文` | `a4 a4 a4 e5` | 非法 | `中文` |
//
// 即 Big5 文本与 SJIS **唯一可能的偶然重合形态 = 半角片假名串**
// (SJIS 半角片假名 0xA1–0xDF 单字节区 + 0x40–0x7E ASCII 尾字节)。
// 反过来,日文剧本经 CP932 解码不会产生大片半角片假名(全角假名/汉字),
// 故「半角片假名字节占比」是有效判别票。纯 ASCII 两方解码逐字节相同
// (半角计数同为 0)→ 落回 SJIS 默认,结果无差异。
//
// 不做的事:不按**单个字符串**报错或猜测 —— 判定的调用场景是整份剧本 /
// 整条台词(字节量足够),短串歧义留给上层按本项目其它证据处理。

/// 数据侧编码判定错误:两种候选编码都存在非法字节序列。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("文本编码判定失败:SJIS 与 Big5 严格解码均有非法字节序列(数据既非日文亦非繁中语料;)")]
pub struct UndecodableText;

/// 半角片假名字节计数(U+FF61–U+FF9F;判定的歧义票,见模块文档)。
fn halfwidth_katakana_count(text: &str) -> usize {
    text.chars()
        .filter(|c| ('\u{FF61}'..='\u{FF9F}').contains(c))
        .count()
}

/// **数据侧编码判定**(跨层唯一策略,见模块文档)。
///
/// 返回 `Ok(encoding)` = 按「strict 有效性 → 歧义票」选出的编码;
/// 两种候选都失败 → [`UndecodableText`](不静默替换、不猜)。
pub fn detect_encoding(bytes: &[u8]) -> std::result::Result<&'static Encoding, UndecodableText> {
    let mut valid: Vec<(&'static Encoding, bool, usize)> = Vec::new();
    for enc in [encoding_rs::SHIFT_JIS, encoding_rs::BIG5] {
        let (text, had_errors) = enc.decode_without_bom_handling(bytes);
        if !had_errors {
            valid.push((enc, true, halfwidth_katakana_count(&text)));
        }
    }
    match valid.as_slice() {
        // 单方合法 → 直接取该方(语料主体形态)
        [(enc, _, _)] => Ok(*enc),
        // 双方合法(歧义)→ 半角片假名更少者;相同 → SJIS(引擎默认)
        [(s_enc, _, s_hw), (b_enc, _, b_hw)] => {
            Ok(if b_hw < s_hw { *b_enc } else { *s_enc })
        }
        _ => Err(UndecodableText),
    }
}

/// **跨层文本解码**(剧本层 / VM `WORD`·`TEXT` 台词 / 标题 caption 共用)。
///
/// 与 [`detect_encoding`] 同策略:判不出 → `Err`(调用方须显式处理,
/// 不得回落到某个写死编码)。
pub fn decode_text(bytes: &[u8]) -> std::result::Result<String, UndecodableText> {
    let enc = detect_encoding(bytes)?;
    let (text, _) = enc.decode_without_bom_handling(bytes);
    Ok(text.into_owned())
}

/// 解析剧本(**数据侧判定编码**;T1 定稿入口)。
///
/// 与 [`parse_scenario`](写死 SJIS)/[`parse_scenario_with`](显式编码)并列;
/// 生产路径(播放器装载 sc.ypf 条目)应走本入口 —— 定位不再依赖某一版
/// 本地化的写死编码。
pub fn parse_scenario_auto(bytes: &[u8]) -> Result<Scenario> {
    let enc = detect_encoding(bytes).map_err(|_| ScenarioError::Decode {
        line: 1,
        encoding: "SJIS/Big5(数据侧判定均失败)",
    })?;
    parser::parse(bytes, enc)
}

#[cfg(test)]
mod tests;
