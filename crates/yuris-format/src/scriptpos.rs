//! `SCRIPTPOS` —— 任务执行位置的字符串编码（`TASKINFO SCRIPTPOS` /
//! `TASK SCRIPTPOS`，手册 `control/taskinfo.html` 例[5]）。
//!
//! ## 容器格式（v494 序列化器 `0x4528c8`，逐指令读出 —— **Confirmed**）
//!
//! ```text
//! [8 字节]  魔数 01 02 03 04 05 06 07 08         (= .rdata 0x5b4090)
//! [2 字符]  GOSUB 帧数            (hex2)
//! 每帧 18 字符:
//!   [2]  byte  +0    （脚本号低字节）
//!   [8]  u32   +4
//!   [8]  u32   +8
//! [2 字符]  「块帧」数            (hex2)          （引擎 task+0x140 栈深）
//! 每项 20 字符:
//!   [2]  byte  +0    [2] byte +1   [8] u32 +4   [8] u16 +8 的高 16 位域展开
//! [2 字符]  循环帧数              (hex2)          （引擎 task+0x244 栈深）
//! 每项 34 字符:
//!   [2]  byte  +0    [8] u32 +4    [8] u32 +8
//!   [8]  u32   +0x18  [8] u32 +0x1c
//! ```
//!
//! **字符编码**：每字节/每 16 位半字节都用 `'a' + nibble`（`0x61..0x70`）
//! —— **不是** `0-9A-F`。所以一个字节固定 2 字符、u32 固定 8 字符。
//! 序列化器在 `0x4528f8`/`0x45293a` 等处统一使用
//! `nib = (v >> shift) & 0xf; ch = 0x61 + nib`。
//!
//! ## 与引擎的字节兼容性：**未验证**
//!
//! 本节编码**由本内核的栈模型**产生（[`ScriptPos::encode`]），解码
//! （[`ScriptPos::decode`]）也只接受本内核产生的形态。它与引擎的
//! `TASK SCRIPTPOS` 恢复路径**未做逐字节对拍** —— 引擎帧记录
//! （`+0/+4/+8` 三字段）的语义尚未取证，故本模块**不声称**能读引擎写出的
//! `SCRIPTPOS` 串，也不声称引擎能读本内核的串。
//!
//! 它的用途是：脚本把 `SCRIPTPOS` 存进变量、之后通过 `TASK` 原样回传时，
//! 本内核内部能**无损往返**（存档/读档锚点自洽），而不写假值。

/// 魔数（引擎 `.rdata` `0x5b4090`）。
pub const MAGIC: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

/// 一个 GOSUB 帧的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GosubPos {
    /// 脚本号（低字节参与编码）。
    pub script_id: u16,
    /// 返回组下标。
    pub return_pc: u32,
    /// 该帧的 IF 嵌套深度。
    pub if_depth: u32,
}

/// 一个循环帧的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoopPos {
    /// 循环体顶部组下标。
    pub body_start: u32,
    /// 退出点组下标。
    pub exit_target: u32,
    /// 已完成次数。
    pub counter: u32,
    /// 计数上限（`u32::MAX` = 无限）。
    pub limit: u32,
}

/// 一个「块帧」的位置（引擎 `task+0x144` 栈；本内核对应 IF 块栈）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockPos {
    /// 类型字节 1。
    pub kind: u8,
    /// 类型字节 2。
    pub flag: u8,
    /// 组下标。
    pub pc: u32,
    /// 次级编号（u16）。
    pub sub: u16,
}

/// 一个任务执行位置的完整快照。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScriptPos {
    /// GOSUB 帧（由外到内）。
    pub gosub: Vec<GosubPos>,
    /// 块帧（IF 块栈）。
    pub blocks: Vec<BlockPos>,
    /// 循环帧。
    pub loops: Vec<LoopPos>,
}

/// 编码错误（容量/格式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptPosError {
    /// 魔数不符 —— 不是本内核产生的 `SCRIPTPOS` 串。
    BadMagic,
    /// 长度不足 / 段计数越界 / 字符不在 `'a'..='p'`。
    Malformed(String),
}

impl std::fmt::Display for ScriptPosError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "SCRIPTPOS 魔数不符(不是本内核编码)"),
            Self::Malformed(m) => write!(f, "SCRIPTPOS 格式错误:{m}"),
        }
    }
}

impl std::error::Error for ScriptPosError {}

/// 半字节 → 字符（`'a' + n`；引擎口径）。
fn hex_char(n: u8) -> char {
    (b'a' + (n & 0xf)) as char
}

/// 写一个字节（2 字符，高半字节在前）。
fn push_u8(out: &mut String, v: u8) {
    out.push(hex_char(v >> 4));
    out.push(hex_char(v));
}

/// 写一个 u32（8 字符，从高到低）。
fn push_u32(out: &mut String, v: u32) {
    for shift in [28, 24, 20, 16, 12, 8, 4, 0] {
        out.push(hex_char((v >> shift) as u8));
    }
}

/// 写一个 u16（4 字符，从高到低）。
fn push_u16(out: &mut String, v: u16) {
    for shift in [12, 8, 4, 0] {
        out.push(hex_char((v >> shift) as u8));
    }
}

impl ScriptPos {
    /// 编码为脚本可存的字符串（CP932 兼容 ASCII）。
    pub fn encode(&self) -> String {
        let mut out = String::new();
        // 魔数：按字节逐个写成 2 字符（引擎 serializer 直接 memcpy 这 8 字节，
        // 但本内核用字符域承载，故统一走 nibble 编码，解码端对称还原）。
        for b in MAGIC {
            push_u8(&mut out, b);
        }
        push_u8(&mut out, self.gosub.len() as u8);
        for f in &self.gosub {
            push_u8(&mut out, f.script_id as u8);
            push_u32(&mut out, f.return_pc);
            push_u32(&mut out, f.if_depth);
        }
        push_u8(&mut out, self.blocks.len() as u8);
        for b in &self.blocks {
            push_u8(&mut out, b.kind);
            push_u8(&mut out, b.flag);
            push_u32(&mut out, b.pc);
            push_u16(&mut out, b.sub);
            // 引擎该项 20 字符:2+2+8+4=16,余 4 字符为对齐域(实测写 0)。
            push_u16(&mut out, 0);
        }
        push_u8(&mut out, self.loops.len() as u8);
        for l in &self.loops {
            push_u32(&mut out, l.body_start);
            push_u32(&mut out, l.exit_target);
            push_u32(&mut out, l.counter);
            push_u32(&mut out, l.limit);
            // 引擎该项 34 字符:8*4=32,余 2 字符为对齐域(实测写 0)。
            push_u8(&mut out, 0);
        }
        out
    }

    /// 从字符串解码（只接受本内核 [`Self::encode`] 的产物）。
    pub fn decode(s: &str) -> Result<Self, ScriptPosError> {
        let b = s.as_bytes();
        let mut pos = 0usize;
        let take_nib = |pos: &mut usize| -> Result<u8, ScriptPosError> {
            let c = *b
                .get(*pos)
                .ok_or_else(|| ScriptPosError::Malformed("意外结束".into()))?;
            *pos += 1;
            if !(b'a'..=b'p').contains(&c) {
                return Err(ScriptPosError::Malformed(format!(
                    "字符 {c:?} 不在 'a'..'p' 域"
                )));
            }
            Ok(c - b'a')
        };
        let take_u8 = |pos: &mut usize| -> Result<u8, ScriptPosError> {
            Ok((take_nib(pos)? << 4) | take_nib(pos)?)
        };
        let mut magic = [0u8; 8];
        for m in magic.iter_mut() {
            *m = take_u8(&mut pos)?;
        }
        if magic != MAGIC {
            return Err(ScriptPosError::BadMagic);
        }
        let mut out = ScriptPos::default();
        let n_gosub = take_u8(&mut pos)? as usize;
        for _ in 0..n_gosub {
            let raw = take_u8(&mut pos)? as u16;
            let mut u32s = [0u32; 2];
            for v in u32s.iter_mut() {
                let mut val = 0u32;
                for _ in 0..8 {
                    val = (val << 4) | take_nib(&mut pos)? as u32;
                }
                *v = val;
            }
            out.gosub.push(GosubPos {
                script_id: raw,
                return_pc: u32s[0],
                if_depth: u32s[1],
            });
        }
        let n_blocks = take_u8(&mut pos)? as usize;
        for _ in 0..n_blocks {
            let kind = take_u8(&mut pos)?;
            let flag = take_u8(&mut pos)?;
            let mut pc = 0u32;
            for _ in 0..8 {
                pc = (pc << 4) | take_nib(&mut pos)? as u32;
            }
            let mut sub = 0u16;
            for _ in 0..4 {
                sub = (sub << 4) | take_nib(&mut pos)? as u16;
            }
            for _ in 0..4 {
                let _ = take_nib(&mut pos)?;
            }
            out.blocks.push(BlockPos {
                kind,
                flag,
                pc,
                sub,
            });
        }
        let n_loops = take_u8(&mut pos)? as usize;
        for _ in 0..n_loops {
            let mut u32s = [0u32; 4];
            for v in u32s.iter_mut() {
                let mut val = 0u32;
                for _ in 0..8 {
                    val = (val << 4) | take_nib(&mut pos)? as u32;
                }
                *v = val;
            }
            let _ = take_u8(&mut pos)?; // 对齐域
            out.loops.push(LoopPos {
                body_start: u32s[0],
                exit_target: u32s[1],
                counter: u32s[2],
                limit: u32s[3],
            });
        }
        if pos != b.len() {
            return Err(ScriptPosError::Malformed(format!(
                "尾部残余 {} 字符",
                b.len() - pos
            )));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ScriptPos {
        ScriptPos {
            gosub: vec![
                GosubPos {
                    script_id: 20,
                    return_pc: 329,
                    if_depth: 1,
                },
                GosubPos {
                    script_id: 70,
                    return_pc: 1234,
                    if_depth: 2,
                },
            ],
            blocks: vec![BlockPos {
                kind: 3,
                flag: 0,
                pc: 65538,
                sub: 7,
            }],
            loops: vec![LoopPos {
                body_start: 100,
                exit_target: 200,
                counter: 3,
                limit: u32::MAX,
            }],
        }
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let p = sample();
        let enc = p.encode();
        let dec = ScriptPos::decode(&enc).expect("decode");
        assert_eq!(dec, p);
    }

    #[test]
    fn empty_is_just_magic_and_three_counts() {
        let enc = ScriptPos::default().encode();
        // 8 字节魔数 = 16 字符 + 三个计数 = 6 字符
        assert_eq!(enc.len(), 22);
        assert_eq!(ScriptPos::decode(&enc).unwrap(), ScriptPos::default());
    }

    #[test]
    fn uses_a_to_p_not_0to9() {
        let enc = ScriptPos::default().encode();
        assert!(
            enc.chars().all(|c| ('a'..='p').contains(&c)),
            "引擎用 'a'+nibble 编码:{enc}"
        );
    }

    #[test]
    fn rejects_foreign_strings() {
        assert_eq!(ScriptPos::decode(""), Err(ScriptPosError::Malformed("意外结束".into())));
        // 魔数正确但编码域错误（'0'-'9'）
        assert!(ScriptPos::decode("0102030405060708aa").is_err());
        // 魔数不符
        let bad = "aa".repeat(11);
        assert_eq!(ScriptPos::decode(&bad), Err(ScriptPosError::BadMagic));
    }

    #[test]
    fn rejects_trailing_garbage() {
        let mut enc = sample().encode();
        enc.push('a');
        assert!(ScriptPos::decode(&enc).is_err(), "尾部残余须报错");
    }
}
