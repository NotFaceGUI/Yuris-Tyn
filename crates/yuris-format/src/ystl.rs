//! YSTL 脚本名表（`ysbin/yst_list.ybn`）—— 脚本槽位 ↔ 源文件名 ↔ 文本数。
//!
//! 这是 `SYSTEMINFO` 的 `SCRIPTFILE` / `SCRIPTNUM` / `TEXTNUM` 三条查询的
//! **真值来源**（引擎 v494 `0x450478` 处理体：把槽号索引进 `0x802b28`
//! （名字表）与 `0x802b2c`（文本数表），两张表由 `0x45d650` 从本文件建立）。
//!
//! ## 布局（release 实机逐字节核对；160 条精确消费到文件末尾）
//!
//! ```text
//! [Header 12B] magic b"YSTL" + version u32 + 条目数 u32
//! [Entry × 条目数]
//!     u32 index        槽号（实测恒 0,1,2,…,N-1；引擎按它填名字表）
//!     u32 name_len
//!     u8[name_len]     name（CP932；`data\script\...\*.yst` / `*.txt`）
//!     u32 hash_a       逐条不同（56 种取值 / 160 条；语义 Unknown）
//!     u32 hash_b       逐条不同（64 种取值 / 160 条；语义 Unknown）
//!     u32 stat_a       纯 `.txt` 剧本为 0，系统 `.yst` 非 0（语义 Unknown）
//!     u32 stat_b       候选「标签数」（`選択肢画面設計.txt`=1、`確認画面設計.txt`=12 …；未取证）
//!     u32 text_count   **文本（台词）条数** —— `SYSTEMINFO TEXTNUM` 的返回值
//! ```
//!
//! ## `text_count` 语义的两条独立证据
//!
//! 1. 引擎读它的位置是 `0x45d7e5..0x45d7ef`（`MOV EBX,[ESI+EBX+0x14]`，
//!    即 `name_end+16`）→ 填入 `0x802b2c` 表；`SYSTEMINFO TEXTNUM`
//!    (`0x4504e6`) 读的正是这张表；
//! 2. 全表合计 **28299**，与本仓库 `PROGRESS.md` 记录的「全语料 28299 组
//!    `RETURNCODE`」逐数吻合 —— 每个文本行恰好发一条 `RETURNCODE`，
//!    故该字段 = 文本行数。
//!
//! `hash_a` / `hash_b` / `stat_a` / `stat_b` 的语义**未取证**，本模块原样
//! 保留、不做解释（**不得**用它们做任何判定）。

use yuris_core::{Error, Reader, Result};

/// YSTL magic：`YSTL`。
pub const YSTL_MAGIC: [u8; 4] = *b"YSTL";

/// header 长度（magic 4 + version 4 + 条目数 4）。
pub const HEADER_LEN: usize = 12;

/// 条目名长度上限（实测最长 121 字节；超过即视为布局变化）。
const MAX_NAME_LEN: usize = 1024;

/// 每条目尾部的定长字段数（`hash_a / hash_b / stat_a / stat_b / text_count`）。
const TAIL_U32: usize = 5;

/// 脚本表的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YstlEntry {
    /// 槽号（= 引擎内部脚本下标；实测与条目次序一致）。
    pub index: u32,
    /// 源文件名（CP932 原始字节，如 `data\script\eris\es_cash.yst`）。
    pub name: Vec<u8>,
    /// 尾部第一哈希（语义 Unknown，原样保留）。
    pub hash_a: u32,
    /// 尾部第二哈希（语义 Unknown，原样保留）。
    pub hash_b: u32,
    /// 尾部统计量 1（语义 Unknown，原样保留）。
    pub stat_a: u32,
    /// 尾部统计量 2（语义 Unknown，原样保留）。
    pub stat_b: u32,
    /// 文本（台词）条数 —— `SYSTEMINFO TEXTNUM` 的返回值（见模块文档）。
    pub text_count: u32,
}

impl YstlEntry {
    /// 源文件名（lossy UTF-8；CP932 名会带替换字符，仅用于诊断/后缀匹配）。
    pub fn name_lossy(&self) -> String {
        String::from_utf8_lossy(&self.name).to_string()
    }
}

/// 已解析的 YSTL 表。
#[derive(Debug, Clone)]
pub struct YstlTable {
    version: u32,
    entries: Vec<YstlEntry>,
    trailing: usize,
}

impl YstlTable {
    /// 从**已解压/已解密**的字节解析。
    ///
    /// 严格按上表布局读取：截断、长度越界、槽号不自洽、尾部残余 → `Err`
    /// （布局变化不猜）。
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let magic = r.u32_bytes()?;
        if magic != YSTL_MAGIC {
            return Err(Error::BadMagic {
                expected: YSTL_MAGIC,
                actual: magic,
            });
        }
        let version = r.u32_le()?;
        let count = r.u32_le()? as usize;
        let mut entries = Vec::with_capacity(count);
        for slot in 0..count {
            let index = r.u32_le()?;
            if index != slot as u32 {
                return Err(Error::format(format!(
                    "YSTL 条目 {slot} 槽号不自洽(index={index})——布局变化?"
                )));
            }
            let name_len = r.u32_le()? as usize;
            if name_len > MAX_NAME_LEN {
                return Err(Error::format(format!(
                    "YSTL 条目 {slot} 名字长度 {name_len} 超上限 {MAX_NAME_LEN}——布局变化?"
                )));
            }
            let name = r.bytes(name_len)?.to_vec();
            let mut tail = [0u32; TAIL_U32];
            for t in tail.iter_mut() {
                *t = r.u32_le()?;
            }
            entries.push(YstlEntry {
                index,
                name,
                hash_a: tail[0],
                hash_b: tail[1],
                stat_a: tail[2],
                stat_b: tail[3],
                text_count: tail[4],
            });
        }
        let trailing = r.remaining();
        if trailing != 0 {
            return Err(Error::format(format!(
                "YSTL 尾部残余 {trailing} 字节(实测应为 0)——布局变化,不猜"
            )));
        }
        Ok(Self {
            version,
            entries,
            trailing,
        })
    }

    /// 引擎版本号（header `version` 字段）。
    pub fn version(&self) -> u32 {
        self.version
    }

    /// 全部条目（按槽号次序）。
    pub fn entries(&self) -> &[YstlEntry] {
        &self.entries
    }

    /// 条目数（= `SYSTEMINFO SCRIPTNUM` 的返回值）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 按槽号取条目。
    pub fn get(&self, index: u32) -> Option<&YstlEntry> {
        self.entries.get(index as usize)
    }

    /// 按源文件名（后缀、大小写不敏感、分隔符等价）查条目。
    pub fn find_by_name(&self, name: &str) -> Option<&YstlEntry> {
        let want = normalize(name);
        self.entries.iter().find(|e| {
            let have = normalize(&e.name_lossy());
            have == want || have.ends_with(&want)
        })
    }

    /// 文本数合计（全语料口径）。
    pub fn total_texts(&self) -> u64 {
        self.entries.iter().map(|e| e.text_count as u64).sum()
    }

    /// 尾部残余字节数（恒 0；诊断用）。
    pub fn trailing(&self) -> usize {
        self.trailing
    }
}

/// 名字归一化：分隔符统一为 `\`、小写。
fn normalize(s: &str) -> String {
    s.replace('/', "\\").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按实机布局构造一条最小 YSTL。
    fn build(entries: &[(&[u8], u32, u32)], version: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&YSTL_MAGIC);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (i, (name, stat_a, texts)) in entries.iter().enumerate() {
            out.extend_from_slice(&(i as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&0xDEADBEEFu32.to_le_bytes()); // hash_a
            out.extend_from_slice(&0u32.to_le_bytes()); // hash_b
            out.extend_from_slice(&stat_a.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // stat_b
            out.extend_from_slice(&texts.to_le_bytes());
        }
        out
    }

    #[test]
    fn parses_entries_and_counts() {
        let blob = build(
            &[(br"data\script\a.yst", 100, 7), (br"data\script\b.yst", 20, 3)],
            494,
        );
        let t = YstlTable::from_bytes(&blob).expect("parse");
        assert_eq!(t.version(), 494);
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(0).unwrap().stat_a, 100);
        assert_eq!(t.get(1).unwrap().text_count, 3);
        assert_eq!(t.total_texts(), 10);
        assert_eq!(t.trailing(), 0);
    }

    #[test]
    fn slot_number_must_be_consistent() {
        let a = br"data\script\a.yst";
        let mut blob = build(&[(a, 1, 1), (br"data\script\b.yst", 2, 2)], 494);
        // 第二条的槽号位置 = header + (8 + len(a) + 20)
        let second = HEADER_LEN + 8 + a.len() + 20;
        blob[second..second + 4].copy_from_slice(&9u32.to_le_bytes());
        assert!(YstlTable::from_bytes(&blob).is_err(), "槽号不自洽须报错");
    }

    #[test]
    fn rejects_non_ystl() {
        let err = YstlTable::from_bytes(b"YSTB\0\0\0\0").unwrap_err();
        assert!(matches!(err, Error::BadMagic { .. }));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut blob = build(&[(br"a.yst", 1, 1)], 494);
        assert!(YstlTable::from_bytes(&blob).is_ok());
        blob.extend_from_slice(&[0u8; 4]);
        assert!(YstlTable::from_bytes(&blob).is_err(), "残余字节须报错");
    }

    #[test]
    fn suffix_and_separator_lookup() {
        let blob = build(&[(br"data\script\eris\es_cash.yst", 35, 0)], 494);
        let t = YstlTable::from_bytes(&blob).unwrap();
        assert!(t.find_by_name("data/script/eris/es_cash.yst").is_some());
        assert!(t.find_by_name(r"DATA\SCRIPT\ERIS\ES_CASH.YST").is_some());
        assert!(t.find_by_name("es_cash.yst").is_some());
        assert!(t.find_by_name("nope.yst").is_none());
    }
}
