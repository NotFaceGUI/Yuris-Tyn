//! `yscfg.dat` —— 引擎运行期配置文件(游戏根,128 字节)。
//!
//! **存在性 Confirmed / 字段语义 Unknown**:引擎二进制含 `\yscfg.dat`
//! 字符串(v494 `0x5d57d8`);两份语料(标定 SDK 作品与第二发行作品)
//! 的文件均恰为 128 字节 = 32 × u32。逐字段语义未逆向(读取器引用
//! 未见直接立即数,疑似基址+偏移或批量拷入),**不猜**;本模块只做
//! 结构解析,供启动链读取 + 日志留痕。
//!
//! 第二发行作品(引擎 v494)实测内容(2026-10-18):
//! `u32[0]=1, u32[4]=0, u32[8]=1, u32[0x0c]=1, u32[0x10]=1, u32[0x14]=0,
//! u32[0x18]=1, u32[0x1c]=0, u32[0x20]=0, …, u32[0x40]=2, u32[0x44]=2,
//! u32[0x48]=1, 其余多 0`(标量小整数,疑似开关/档位表)。

use yuris_core::{Error, Result};

/// 文件尺寸(语料恒 128)。
pub const YSCFG_DAT_LEN: usize = 128;

/// 槽位数(128 字节 / 4)。
pub const YSCFG_DAT_SLOTS: usize = 32;

/// 已解析的 `yscfg.dat`。
#[derive(Debug, Clone, PartialEq)]
pub struct YscfgDat {
    /// 32 个 u32 槽(原样;语义 Unknown)。
    pub slots: [u32; YSCFG_DAT_SLOTS],
}

impl YscfgDat {
    /// 从字节解析(长度必须恰为 128;多余/不足都报错 —— 引擎按定长读)。
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != YSCFG_DAT_LEN {
            return Err(Error::format(format!(
                "yscfg.dat 长度 {} != {YSCFG_DAT_LEN}(语料恒定长)",
                data.len()
            )));
        }
        let mut slots = [0u32; YSCFG_DAT_SLOTS];
        for (i, c) in data.chunks_exact(4).enumerate() {
            slots[i] = u32::from_le_bytes(c.try_into().unwrap());
        }
        Ok(Self { slots })
    }

    /// 从文件解析。
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        Self::from_bytes(&std::fs::read(path)?)
    }
}
