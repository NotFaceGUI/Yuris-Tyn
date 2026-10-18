//! YSCF 工程配置（`yscfg.ybn`）。
//!
//! 规格来源：`arcusmaximus/VNTranslationTools` 的 Notes.txt 字段布局 + 本仓库样本实测。
//! 证据等级：**部分确认** —— 见下表。
//!
//! | 字段 | 偏移 | 等级 | 说明 |
//! |---|---|---|---|
//! | `magic` / `version` | 0x00 / 0x04 | Confirmed | 样本 `YSCF` / 555 |
//! | `screen_width/height` | 0x10 / 0x14 | Confirmed | 样本 1920 × 1080 |
//! | `image_type_slots` | 0x1C | Likely | 样本 `01 02 03 04 05 06 00 00` |
//! | `sound_type_slots` | 0x24 | Likely | 样本 `01 02 00 00` |
//! | `file_priority_*` ×3 | 0x3C/0x40/0x44 | **Likely** | 样本 `(1,1,0)`；三者取值与 Notes.txt 顺序吻合，但样本中 Dev/Debug 同为 1，无法独立区分前两者 |
//! | `caption_len` / `caption` | 0x4C / 0x4E | Confirmed | 样本 28 / "Kemonomichi Girlish Square 2" |

use yuris_core::{Error, Reader, Result};

/// YSCF magic：`YSCF`。
pub const YSCF_MAGIC: [u8; 4] = *b"YSCF";

/// YSCF 工程配置。
#[derive(Debug, Clone)]
pub struct YscfFile {
    /// 引擎版本。
    pub version: u32,
    /// 屏幕宽（像素）。
    pub screen_width: u32,
    /// 屏幕高（像素）。
    pub screen_height: u32,
    /// 图像格式槽（样本 `01 02 03 04 05 06 00 00`）。
    pub image_type_slots: [u8; 8],
    /// 音频格式槽（样本 `01 02 00 00`）。
    pub sound_type_slots: [u8; 4],
    /// 文件读取优先级：Dev 模式。
    pub file_priority_dev: u32,
    /// 文件读取优先级：Debug 模式。
    pub file_priority_debug: u32,
    /// 文件读取优先级：Release 模式。**免封包开关**。
    pub file_priority_release: u32,
    /// 游戏标题。
    pub caption: String,
    /// 未解析的原始字节（便于将来回查）。
    pub raw: Vec<u8>,
}

impl YscfFile {
    /// 从**已解压**的 YSCF 字节解析。
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let magic = r.u32_bytes()?;
        if magic != YSCF_MAGIC {
            return Err(Error::BadMagic {
                expected: YSCF_MAGIC,
                actual: magic,
            });
        }
        let version = r.u32_le()?;
        r.seek(0x10);
        let screen_width = r.u32_le()?;
        let screen_height = r.u32_le()?;
        r.seek(0x1C);
        let mut image_type_slots = [0u8; 8];
        image_type_slots.copy_from_slice(r.bytes(8)?);
        let mut sound_type_slots = [0u8; 4];
        sound_type_slots.copy_from_slice(r.bytes(4)?);
        r.seek(0x3C);
        let file_priority_dev = r.u32_le()?;
        let file_priority_debug = r.u32_le()?;
        let file_priority_release = r.u32_le()?;
        r.seek(0x4C);
        let caption_len = r.u16_le()? as usize;
        let caption_bytes = r.bytes(caption_len)?;
        let caption = decode_caption(caption_bytes);

        Ok(Self {
            version,
            screen_width,
            screen_height,
            image_type_slots,
            sound_type_slots,
            file_priority_dev,
            file_priority_debug,
            file_priority_release,
            caption,
            raw: data.to_vec(),
        })
    }

    /// 免封包是否已开启（`file_priority_release == 1`）。
    ///
    /// 样本实测 `(dev, debug, release) = (1, 1, 0)` —— **Release 默认为 0**，
    /// 与 `[YU-RIS] 免封包处理` 一文完全吻合：
    /// 「我们的目的正是在把 `filePriorityRelease` 变为 1」。
    pub fn unpacked_read_enabled(&self) -> bool {
        self.file_priority_release == 1
    }
}

/// caption 解码。
///
/// **勘误（2026-10-13）**：原实现把非 ASCII 字节一律换成 `'?'`，注释写「编码 Unknown
/// （样本为 ASCII）」—— 那只对 v555 样本（`Kemonomichi Girlish Square 2`）成立。
/// 实测第三方发行版（引擎 494）的 `yscfg.ybn` caption 是 **Shift-JIS**
/// （字节 `83 52 83 67 83 6c 83 43 83 8d` + `ver1.02`），原实现解出 `????ver1.02`。
/// 现按 SJIS 解码（纯 ASCII 输入结果与旧实现逐字符相同，样本行为不变）。
/// 证据：`crates/yuris-tools --example entry_dump -- <游戏目录> yscfg.ybn`。
fn decode_caption(b: &[u8]) -> String {
    if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        return b.iter().map(|&c| c as char).collect();
    }
    let (s, _, _) = encoding_rs::SHIFT_JIS.decode(b);
    s.into_owned()
}
