//! YPF 封包解析。
//!
//! 规格：`docs/formats/ypf.md`（**Confirmed**，样本 v500 / 309 条目）。
//!
//! 要点：
//! - header 0x20 字节 + 索引区前置 4 字节 `index_prefix`（用途 Unknown）
//! - entry 从 `0x24` 开始
//! - 文件名：C 字符串，非 0 字节 XOR key，0x00 终止符不加密
//! - 末条 entry 的 tail 可能不足 8 字节（被 `first_data_off` 截断），
//!   解析时按剩余空间钳制，因此解析后位置**精确等于** `first_data_off`

use std::collections::HashMap;
use std::io::Read;

use flate2::read::ZlibDecoder;
use yuris_core::{decode_xor_cstring, Error, Reader, Result};

/// YPF magic：`YPF\0`。
pub const YPF_MAGIC: [u8; 4] = *b"YPF\0";

/// 已知资源/脚本扩展名（条目名自动探测用）。
///
/// 覆盖 YU-RIS 实测包面：脚本 `.ybn`、图像 `.png/.jpg/.bmp/.webp/.psb`、
/// 音频 `.ogg/.wav`、影片 `.ymv/.mp4/.avi`、文本/配置 `.txt/.csv/.ini/.dat/.json`。
pub const KNOWN_ENTRY_EXTS: [&str; 15] = [
    ".ybn", ".png", ".jpg", ".ogg", ".wav", ".bmp", ".webp", ".psb", ".txt", ".csv", ".ini",
    ".dat", ".json", ".ymv", ".mp4",
];

/// `detect_name_key` 的最低扩展名命中率。低于此值视为探测失败（不猜）。
pub const MIN_KEY_HIT_RATE: f32 = 0.30;

/// se 型条目类型码：PNG（**存储原值**，与 `name_key` 无关）。
pub const SE_CODE_PNG: u8 = 0x02;
/// se 型条目类型码：WAV。
pub const SE_CODE_WAV: u8 = 0x05;
/// se 型条目类型码：OGG。
pub const SE_CODE_OGG: u8 = 0x06;

/// 是否为已知的 se 型类型码。
pub fn is_se_code(b: u8) -> bool {
    matches!(b, SE_CODE_PNG | SE_CODE_WAV | SE_CODE_OGG)
}

/// 条目名（已解码字节，通常为 CP932/ASCII）是否以已知扩展名结尾。
///
/// 注意：se 型条目的**类型码字节**紧贴名字尾（见 [`YpfIndex`] 的布局说明），
/// 故先剥掉尾部的非 ASCII 字节再判。
pub fn name_has_known_ext(name: &[u8]) -> bool {
    let end = name
        .iter()
        .rposition(|&b| b.is_ascii_graphic() || b == b' ')
        .map(|i| i + 1)
        .unwrap_or(0);
    let s = String::from_utf8_lossy(&name[..end]).to_ascii_lowercase();
    KNOWN_ENTRY_EXTS.iter().any(|x| s.ends_with(x))
}

/// 标志位：条目数据经过 zlib 压缩。
pub const FLAG_ZLIB: u8 = 1;

/// YPF 文件头（不含 `index_prefix`）。
#[derive(Debug, Clone, Copy)]
pub struct YpfHeader {
    /// YPF 格式版本（样本 500）。
    pub version: u32,
    /// 条目数。
    pub file_count: u32,
    /// 首个文件数据的绝对偏移；同时是索引区结束位置。
    pub first_data_off: u32,
}

/// 索引条目。
///
/// > `name_raw_prefix` / `tail` / `reserved` 语义均 **Unknown**，但**原样保留**，
/// > 便于将来回查与回写。
#[derive(Debug, Clone)]
pub struct YpfEntry {
    /// 已解码的内部路径，如 `$ysbin\yst00034.ybn`。
    pub name: String,
    /// 名字首字节的原始值（样本中 303×`0xED`、5×`0xEC`、1×`0xF0`，含义 Unknown）。
    pub name_raw_prefix: u8,
    /// 名字在文件中的绝对偏移（调试用）。
    pub name_offset: usize,
    /// 标志位：1 = zlib 压缩，0 = 原样存储。
    pub flags: u8,
    /// 解压后长度。
    pub uncompressed_len: u32,
    /// 压缩后长度（= 数据区占用字节数）。
    pub compressed_len: u32,
    /// 数据区绝对偏移。
    pub offset: u32,
    /// 保留字段，样本实测恒 0。
    pub reserved: u32,
    /// 条目尾部 8 字节，用途 Unknown，原样保留。
    pub tail: [u8; 8],
    /// 实际读到的 tail 字节数（末条可能 < 8，见模块文档）。
    pub tail_len: usize,
}

/// YPF 封包。
///
/// 当前实现为**内存全量**（样本 bn.ypf 仅 1.4 MB，可接受）。
/// 827 MB 的 cg.ypf 不应整体载入 —— 资源侧请走 `yuris-resource` 的懒加载路径，
/// 那里会改成 seek 式随机访问。
#[derive(Debug)]
pub struct YpfArchive {
    data: Vec<u8>,
    header: YpfHeader,
    index_prefix: u32,
    entries: Vec<YpfEntry>,
    index: HashMap<String, usize>,
}

impl YpfArchive {
    /// 从内存字节解析。
    ///
    /// `name_key` 为文件名 XOR key（样本 `0xC9`）。
    pub fn from_bytes(data: Vec<u8>, name_key: u8) -> Result<Self> {
        let mut r = Reader::new(&data);

        // ---- header（0x00..0x20）----
        let magic = r.u32_bytes()?;
        if magic != YPF_MAGIC {
            return Err(Error::BadMagic {
                expected: YPF_MAGIC,
                actual: magic,
            });
        }
        let version = r.u32_le()?;
        let file_count = r.u32_le()?;
        let first_data_off = r.u32_le()?;

        // 0x10..0x20：样本实测全 0，语义 Unknown，跳过
        r.seek(0x20);

        // ---- 索引区前置 4 字节（用途 Unknown，原样保留）----
        let index_prefix = r.u32_le()?;

        // ---- entries（从 0x24 开始）----
        let mut entries: Vec<YpfEntry> = Vec::with_capacity(file_count as usize);
        let mut index: HashMap<String, usize> = HashMap::with_capacity(file_count as usize);

        for i in 0..file_count {
            let name_offset = r.pos();

            // 名字：C 字符串，非 0 字节 XOR key
            let name_bytes = decode_xor_cstring(r.rest(), name_key);
            let name_len = name_bytes.len();
            let name = String::from_utf8(name_bytes)
                .map_err(|_| Error::format(format!("entry #{i}: name is not ASCII")))?;
            let name_raw_prefix = if name_len > 0 { r.rest()[0] } else { 0 };
            r.skip(name_len + 1)?; // 跳过名字 + NUL 终止符

            let flags = r.u8()?;
            let uncompressed_len = r.u32_le()?;
            let compressed_len = r.u32_le()?;
            let offset = r.u32_le()?;
            let reserved = r.u32_le()?;

            // tail：8 字节，但末条可能被 first_data_off 截断
            let tail_start = r.pos();
            let avail = (first_data_off as usize).saturating_sub(tail_start).min(8);
            let mut tail = [0u8; 8];
            if avail > 0 {
                tail[..avail].copy_from_slice(r.bytes(avail)?);
            }

            let entry = YpfEntry {
                name: name.clone(),
                name_raw_prefix,
                name_offset,
                flags,
                uncompressed_len,
                compressed_len,
                offset,
                reserved,
                tail,
                tail_len: avail,
            };
            index.insert(name, entries.len());
            entries.push(entry);
        }

        // ---- 闭合断言 ----
        if r.pos() != first_data_off as usize {
            return Err(Error::format(format!(
                "index does not close: parsed end = {:#x}, first_data_off = {:#x} (delta {} bytes)",
                r.pos(),
                first_data_off,
                first_data_off as i64 - r.pos() as i64
            )));
        }

        Ok(Self {
            data,
            header: YpfHeader {
                version,
                file_count,
                first_data_off,
            },
            index_prefix,
            entries,
            index,
        })
    }

    /// 文件头。
    pub fn header(&self) -> &YpfHeader {
        &self.header
    }

    /// 索引区前置 4 字节（用途 Unknown）。
    pub fn index_prefix(&self) -> u32 {
        self.index_prefix
    }

    /// 全部条目。
    pub fn entries(&self) -> &[YpfEntry] {
        &self.entries
    }

    /// 按内部路径查找条目。
    pub fn entry(&self, name: &str) -> Option<&YpfEntry> {
        self.index.get(name).map(|&i| &self.entries[i])
    }

    /// 读取一个条目并解压，返回原始字节。
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .entry(name)
            .ok_or_else(|| Error::format(format!("entry not found: {name}")))?;

        let start = e.offset as usize;
        let end = start
            .checked_add(e.compressed_len as usize)
            .ok_or_else(|| Error::format(format!("entry {name}: offset+len overflow")))?;
        let raw = self
            .data
            .get(start..end)
            .ok_or_else(|| Error::format(format!(
                "entry {name}: data range {start:#x}..{end:#x} out of bounds (file {} bytes)",
                self.data.len()
            )))?;

        match e.flags {
            FLAG_ZLIB if e.compressed_len > 0 => {
                let mut out = Vec::with_capacity(e.uncompressed_len as usize);
                ZlibDecoder::new(raw).read_to_end(&mut out)?;
                if out.len() != e.uncompressed_len as usize {
                    return Err(Error::format(format!(
                        "entry {name}: decompressed {} bytes, header says {}",
                        out.len(),
                        e.uncompressed_len
                    )));
                }
                Ok(out)
            }
            FLAG_ZLIB => Ok(Vec::new()),
            0 => Ok(raw.to_vec()),
            other => Err(Error::format(format!(
                "entry {name}: unknown flags value {other:#x} (语义 Unknown，不要猜)"
            ))),
        }
    }
}

/// `YpfIndex` 的单条目信息(P6.3,成果 64)。
///
/// `flag`:bn 型 = 压缩标记(0=stored / 1=zlib);se 型 = 内容类型码
/// (0x02=PNG / 0x06=OGG,数据 stored)。
#[derive(Debug, Clone)]
pub struct YpfEntryInfo {
    /// 规范化条目名(se 型已剥尾缀类型码)。
    pub name: Vec<u8>,
    /// flag(bn=压缩标记 0/1 / se=**原始**类型码 [`SE_CODE_PNG`] 等)。
    pub flag: u8,
    /// 解压后长度。
    pub uncompressed_len: u32,
    /// 存储长度(数据区占用字节数)。
    pub compressed_len: u32,
    /// 数据区绝对偏移。
    pub offset: u32,
}

/// **数据侧压缩判据**(与方言/版本无关)。
///
/// 理由(`docs` 既定结论):`flag` 字节在两族索引方言里含义不同(一族是压缩
/// 标记,另一族是内容类型码),而 zlib 头(`78 01/9c/da`)与长度差是**数据本身**
/// 的事实 —— 按它判无需知道方言,也不会因方言差异漏解压。
///
/// 实测(本机,引擎 494 发行版 `data.ypf`):`flag=0x03`(`.jpg`)与 `flag=0x07`
/// (`.psd`)的**数据是 zlib**,而 `flag=0x02`(`.png`)/`0x05`(`.wav`)/
/// `0x06`(`.ogg`)是明文 stored —— 同一份索引里两种存储态并存,故只能看数据。
pub fn looks_zlib(raw: &[u8], uncompressed_len: u32, compressed_len: u32) -> bool {
    raw.len() >= 2
        && raw[0] == 0x78
        && matches!(raw[1], 0x01 | 0x9c | 0xda)
        && uncompressed_len as usize != compressed_len as usize
}

/// 该字节串是否像一张图片(PNG / JPEG / WebP / BMP / GIF 魔数)。
///
/// 用途:按类型码放行的旧判据在换方言后全军覆没(`0xCB` 是「key=0xC9 时的
/// PNG 码」,见本模块顶部勘误);内容是不是图,看**数据**即可。
pub fn looks_like_image(raw: &[u8]) -> bool {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    raw.starts_with(&PNG)
        || raw.starts_with(&[0xff, 0xd8, 0xff])
        || (raw.len() >= 12 && raw.starts_with(b"RIFF") && &raw[8..12] == b"WEBP")
        || raw.starts_with(b"BM")
        || raw.starts_with(b"GIF8")
}

/// 仅索引的轻量 YPF 读取器(资源**存在性**检查用)。
///
/// cg.ypf 等数百 MB 封包不整载:只读 `0x00..first_data_off`(头部 + 索引区),
/// 建立条目名集合。数据区不触碰 —— 内容读取走 [`YpfReader`]
/// 或 [`YpfArchive`]。
/// 名字保留**原始解码字节**(部分封包为 SJIS 日文名,非 UTF-8)。
///
/// ## 条目布局(P6.2,成果 63):条目级自适应
///
/// 实证同一包内(update1.ypf)两种条目布局并存,总开销均为 len+26:
/// - **bn 型**(脚本/文本条目):`name + NUL + flag + uncomp + comp + off +
///   zero + tail8`;flag∈{0=stored, 1=zlib}。
/// - **se 型**(资源条目,PNG/OGG/WAV):`name + 类型码 + NUL + uncomp + comp +
///   off + zero + tail8`;类型码紧贴名字尾(rcs 会吞进名字尾),值 =**存储原值**
///   内容类型码([`SE_CODE_PNG`]/[`SE_CODE_OGG`]/[`SE_CODE_WAV`]),数据 stored 明文。
///
///   > **勘误（跨版本）**：旧实现把 se 型类型码记成**解码后**的
///   > `{0xCB,0xCF,0xCC}` —— 那恰好是 `key = 0xC9` 时 `{2,6,5} ^ key` 的结果，
///   > 等价于把 name_key 硬编码进了类型码。换 key（实测另一发行作品 `0xFF`）
///   > 后该表永不命中，索引游走退回「首个 NUL」→ 全表名字漂移。
///   > 类型码是数据侧值，与 key 无关；现按原始字节判别并原样记录。
///
/// 判别:se 型名字尾字节为控制码(<0x20,合法路径名不含);bn 型名字尾为
/// 可打印字符。名字规范化:se 型剥尾缀。
#[derive(Debug)]
pub struct YpfIndex {
    /// 条目名集合(规范化解码字节)。
    pub names: std::collections::HashSet<Vec<u8>>,
    /// 头部。
    pub header: YpfHeader,
    /// 各条目的 flag(bn 型 = 压缩标记 0/1 / se 型 = **原始**类型码 2/5/6;顺序同解析序)。
    pub flags: Vec<u8>,
    /// 全部条目信息(顺序同索引区;P6.3)。
    pub entries: Vec<YpfEntryInfo>,
    /// 条目名 → `entries` 下标(首个命中;同名条目取索引区先出现者)。
    pub map: std::collections::HashMap<Vec<u8>, usize>,
}

/// YPac 索引的**方言**(条目步长与名字编码的来源)。
///
/// 实测两族并存,必须**按包判别**,不可硬编码任何一族:
///
/// | 方言 | 名字长度 | 名字编码 | 条目步长 |
/// |---|---|---|---|
/// | [`IndexDialect::LenEncNot`] | **显式** `u8 lenEnc`,`nameLen = T[(0xFF-lenEnc)&0xFF]` | 逐字节**取反** `~b` | `nameLen + 27` |
/// | [`IndexDialect::NulXor`] | 以 `NUL` 结尾 | 逐字节 `XOR key`(逐游戏不同) | `nameLen + 26` |
///
/// `LenEncNot` 的结构: `lenEnc(1) + name(nameLen) + code(1) + NUL(1) + uncomp(4) + comp(4) + off(4) + zero(4) + tail8`。
/// 证据: 引擎侧索引建立函数按 `puVar22+1` 取长度码、按 `0xFF^code` 查宽度表、
/// 名字逐字节 `~b`,步长 `nameLen + 0x1B`(27);本仓库对该族封包实测
/// **12,209/12,211 条字段自洽**。
///
/// > 勘误: 本仓库此前只有 `NulXor` 一族, 且把它当唯一形态。`NulXor` 族下
/// > 被记作「虚拟根前缀 `$`/`%`/`9`」的那个首字节(语义 Unknown),
/// > 在 `LenEncNot` 族里**就是长度码本身** —— 谜团解除。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexDialect {
    /// 显式长度码 + 名字取反(实测该族含 v494 发行数据)。
    LenEncNot,
    /// NUL 结尾 + 逐字节 XOR key。
    NulXor(u8),
}

/// 宽度/长度解码表 `T`: 恒等表 + 12 对交换。
///
/// 引擎侧由初始化函数一次写入(唯一写入者),内容是固定的 12 对换位。
pub const NAME_LEN_SWAPS: [(u8, u8); 12] = [
    (3, 0x48),
    (6, 0x35),
    (9, 0x0B),
    (0x0C, 0x10),
    (0x0D, 0x13),
    (0x11, 0x19),
    (0x15, 0x1B),
    (0x1C, 0x1E),
    (0x20, 0x23),
    (0x26, 0x29),
    (0x2C, 0x2F),
    (0x2E, 0x32),
];

/// 由长度码求名字字节数: `T[(0xFF - lenEnc) & 0xFF]`。
pub fn name_len_from_code(len_enc: u8) -> u8 {
    let mut t = [0u8; 256];
    for (i, v) in t.iter_mut().enumerate() {
        *v = i as u8;
    }
    for (a, b) in NAME_LEN_SWAPS {
        t.swap(a as usize, b as usize);
    }
    t[((0xFFu16 - len_enc as u16) & 0xFF) as usize]
}

/// 名字逐字节取反(`~b`)。
pub fn not_bytes(raw: &[u8]) -> Vec<u8> {
    raw.iter().map(|b| !*b).collect()
}

impl YpfIndex {
    /// 自动探测封包的**条目名 XOR key**。
    ///
    /// `name_key` 逐游戏不同（实测两点：`0xC9` 与 `0xFF`），**不可硬编码**。
    /// 判别器：解码后的名字是否以**已知扩展名**结尾。
    ///
    /// 为什么不能用「可打印率」：错误 key 解出的乱码在字节层面同样有 98%+
    /// 可打印，完全无法判别（实测：某 v494 封包在 60+ 个 key 上都给出 98–99%）。
    /// 扩展名命中要求**整条路径结构恰好正确**，错误 key 的概率可忽略。
    ///
    /// 返回 `(key, 命中率)`；命中率 < [`MIN_KEY_HIT_RATE`] 视为失败（返回
    /// `Err`）—— 宁可不装载，也不猜。
    pub fn detect_name_key(path: &std::path::Path) -> Result<(u8, f32)> {
        let mut best: Option<(u8, usize, usize)> = None; // (key, 命中, 总数)
        let mut parsed_any = false;
        for k in 0u16..=255 {
            let k = k as u8;
            let Ok(idx) = Self::from_path(path, k) else {
                continue;
            };
            let n = idx.entries.len();
            if n == 0 || n != idx.header.file_count as usize {
                continue;
            }
            parsed_any = true;
            let hits = idx
                .entries
                .iter()
                .filter(|e| name_has_known_ext(&e.name))
                .count();
            if best.map(|(_, h, _)| hits > h).unwrap_or(true) {
                best = Some((k, hits, n));
            }
        }
        match best {
            Some((k, hits, n)) if n > 0 => {
                let rate = hits as f32 / n as f32;
                if rate >= MIN_KEY_HIT_RATE {
                    Ok((k, rate))
                } else {
                    Err(Error::format(format!(
                        "ypf name_key 探测失败: 最佳 key 0x{k:02X} 扩展名命中率仅 {:.1}%(阈值 {:.0}%)",
                        rate * 100.0,
                        MIN_KEY_HIT_RATE * 100.0
                    )))
                }
            }
            _ => Err(Error::format(format!(
                "ypf name_key 探测失败: 无任何 key 能一致解析索引(任一 key 可解析={parsed_any})"
            ))),
        }
    }

    /// 结构自洽率: 字段块 `zero == 0` 且 `off` 落在数据区内的比例。
    ///
    /// 这是判**索引游走是否漂移**的廉价而强的指标 —— 名字看着对不代表字段对。
    fn structural_score(entries: &[YpfEntryInfo], file_len: u32, first_data_off: u32) -> f32 {
        if entries.is_empty() {
            return 0.0;
        }
        let ok = entries
            .iter()
            .filter(|e| {
                !e.name.is_empty()
                    && e.flag != 0xFF
                    && e.compressed_len > 0
                    && e.offset >= first_data_off
                    && (e.offset as u64 + e.compressed_len as u64) <= file_len as u64
            })
            .count();
        ok as f32 / entries.len() as f32
    }

    /// 按 [`IndexDialect::LenEncNot`] 解析索引区。
    ///
    /// 结构: `lenEnc(1) + name(nameLen, 取反) + code(1) + NUL(1) + uncomp(4) +
    /// comp(4) + off(4) + zero(4) + tail8`, 步长 `nameLen + 27`。
    pub fn from_path_lenenc(path: &std::path::Path) -> Result<Self> {
        use std::io::{Read, Seek, SeekFrom};

        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; 0x24];
        f.read_exact(&mut head)?;
        let mut r = Reader::new(&head);
        let magic = r.u32_bytes()?;
        if magic != YPF_MAGIC {
            return Err(Error::BadMagic {
                expected: YPF_MAGIC,
                actual: magic,
            });
        }
        let version = r.u32_le()?;
        let file_count = r.u32_le()?;
        let first_data_off = r.u32_le()?;
        let index_len = (first_data_off as usize)
            .checked_sub(0x24)
            .ok_or_else(|| Error::format("ypf index: first_data_off < 0x24"))?;
        f.seek(SeekFrom::Start(0x24))?;
        let mut block = vec![0u8; index_len];
        f.read_exact(&mut block)?;
        let _file_len = std::fs::metadata(path)?.len() as u32;

        let mut names = std::collections::HashSet::with_capacity(file_count as usize);
        let mut flags = Vec::with_capacity(file_count as usize);
        let mut entries: Vec<YpfEntryInfo> = Vec::with_capacity(file_count as usize);
        let mut map: std::collections::HashMap<Vec<u8>, usize> =
            std::collections::HashMap::with_capacity(file_count as usize);

        let mut pos = 0usize;
        for _i in 0..file_count {
            let nl = name_len_from_code(block[pos]) as usize;
            let name_end = pos + 1 + nl;
            if name_end + 2 + 24 > block.len() {
                break;
            }
            let name = not_bytes(&block[pos + 1..name_end]);
            let code = block[name_end];
            // `name_end + 1` 期望是 NUL。**不硬报错**:单个条目的异常不应让整包
            // 无法装载(实测该族 12,211 条中 2 条例外 —— 步长仍是显式长度,
            // 故按固定步长继续走即可;异常条目由结构自洽率反映)。
            let fs = name_end + 2;
            let uncompressed_len =
                u32::from_le_bytes(block[fs..fs + 4].try_into().unwrap());
            let compressed_len =
                u32::from_le_bytes(block[fs + 4..fs + 8].try_into().unwrap());
            let offset = u32::from_le_bytes(block[fs + 8..fs + 12].try_into().unwrap());
            flags.push(code);
            map.entry(name.clone()).or_insert(entries.len());
            names.insert(name.clone());
            entries.push(YpfEntryInfo {
                name,
                flag: code,
                uncompressed_len,
                compressed_len,
                offset,
            });
            pos = fs + 24;
        }

        Ok(Self {
            names,
            header: YpfHeader {
                version,
                file_count,
                first_data_off,
            },
            flags,
            entries,
            map,
        })
    }

    /// 自动判别索引方言并给出结构自洽率。
    /// **快速预筛**：只解析前 `probe` 条，判断该 `name_key` 是否可能成立。
    ///
    /// 为什么需要（2026-10-18 启动性能修复）：`detect_dialect` 的 NUL 族
    /// 会对 **256 个候选 key** 逐个做**全量**解析。实测 3.3 GB / 12,211 条的
    /// 发行包上，这一步花约 5 秒（每次全量解析都要读整块索引 + 建 3 张表）。
    ///
    /// 预筛只读一次索引块、只走前 `probe` 条：键正确时前几条就应当结构成立
    /// （名字非空、flag 合法、偏移落在数据区）；键错误时第一条就会越界。
    /// 通过的候选再做全量解析并打分。**判定逻辑与全量解析同源**
    /// （同一 `name_boundary_valid` / 同一字段布局），不是新的启发式。
    fn probe_key(block: &[u8], file_count: u32, file_len: u32, first_data_off: u32, name_key: u8, probe: usize) -> bool {
        let mut r = Reader::new(block);
        let n = (file_count as usize).min(probe);
        for _ in 0..n {
            let rest = r.rest();
            let base = r.pos();
            // 与全量解析同款：对每个 0x00 候选做结构校验。
            let mut nul_rel: Option<usize> = None;
            let mut scan = 0usize;
            while let Some(rel) = rest[scan..].iter().position(|&b| b == 0) {
                let cand = scan + rel;
                let np = base + cand + 1;
                let name = decode_xor_cstring(&rest[..cand], name_key);
                if name_boundary_valid(block, np, name.as_slice(), file_len, first_data_off) {
                    nul_rel = Some(cand);
                    break;
                }
                scan = cand + 1;
            }
            let Some(cand) = nul_rel else { return false };
            let name_bytes = decode_xor_cstring(&rest[..cand], name_key);
            if name_bytes.is_empty() || name_bytes.len() > 1024 {
                return false;
            }
            // **关键判别**：包内条目名是 CP932 路径（ASCII 目录 + 可能的
            // SJIS 文件名）。key 错误时异或解出的是任意字节 —— 实测该包
            // 256 个 key 全都「结构成立」（offset 落在数据区），只有名字
            // 可打印性把它们区分开。这里要求：**含至少一个分隔符**
            // （反斜杠或斜杠）且**不含控制字符**。
            const SEP_BACK: u8 = 0x5c;
            if !name_bytes.iter().any(|b| *b == SEP_BACK || *b == b'/') {
                return false;
            }
            if name_bytes
                .iter()
                .any(|b| *b < 0x20 || (*b >= 0x7f && *b < 0xa0))
            {
                return false;
            }
            if r.skip(name_bytes.len() + 1).is_err() {
                return false;
            }
            // 名字后至少要能容纳剩余字段（bn 型 18B / se 型 19B 的保守下界）。
            if r.remaining() < 17 {
                return false;
            }
            r.skip(17).ok();
        }
        true
    }

    /// 按包内特征探测索引方言与置信度(0..1);跨版本语料用。
    pub fn detect_dialect(path: &std::path::Path) -> Result<(IndexDialect, f32)> {
        let file_len = std::fs::metadata(path)?.len() as u32;
        let mut head = [0u8; 0x24];
        {
            use std::io::Read;
            std::fs::File::open(path)?.read_exact(&mut head)?;
        }
        let first_data_off = u32::from_le_bytes(head[0x0C..0x10].try_into().unwrap());

        // 末条常被 `first_data_off` 钳制而不足一整条 → 允许 −2 的容差,
        // 否则正确方言会被「条数不等」误弃(实测该族为 12,211/12,212)。
        let tol = |n: usize, want: usize| n + 2 >= want && n <= want;

        let mut best: Option<(IndexDialect, f32)> = None;
        if let Ok(idx) = Self::from_path_lenenc(path) {
            if tol(idx.entries.len(), idx.header.file_count as usize) {
                let sc = Self::structural_score(&idx.entries, file_len, first_data_off);
                best = Some((IndexDialect::LenEncNot, sc));
            }
        }
        // NUL 族: **先预筛再全量**（见 `probe_key` 的文档）。
        // 旧实现对 256 个 key 各做一次全量解析 ⇒ 3.3 GB 包上约 5 秒。
        // 现在只读一次索引块，用前 8 条做结构预筛，通过的才算全量分。
        let mut best_nul: Option<(u8, f32)> = None;
        // header 里的条目数与首数据偏移（预筛需要）。
        let head_file_count = u32::from_le_bytes(head[0x08..0x0C].try_into().unwrap());
        let probe_block: Option<Vec<u8>> = (|| {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(path).ok()?;
            let mut head = [0u8; 0x24];
            f.read_exact(&mut head).ok()?;
            let file_count = u32::from_le_bytes(head[0x08..0x0C].try_into().ok()?);
            let first_data_off = u32::from_le_bytes(head[0x0C..0x10].try_into().ok()?);
            let index_len = (first_data_off as usize).checked_sub(0x24)?;
            f.seek(SeekFrom::Start(0x24)).ok()?;
            let mut block = vec![0u8; index_len];
            f.read_exact(&mut block).ok()?;
            let _ = file_count;
            Some(block)
        })();
        for k in 0u16..=255 {
            let k = k as u8;
            // 预筛(有块时):8 条结构不成立就直接判该 key 不可行。
            if let Some(block) = &probe_block {
                if !Self::probe_key(block, head_file_count, file_len, first_data_off, k, 8) {
                    continue;
                }
            }
            let Ok(idx) = Self::from_path(path, k) else {
                continue;
            };
            if idx.entries.is_empty()
                || !tol(idx.entries.len(), idx.header.file_count as usize)
            {
                continue;
            }
            let sc = Self::structural_score(&idx.entries, file_len, first_data_off);
            if best_nul.map(|(_, s)| sc > s).unwrap_or(true) {
                best_nul = Some((k, sc));
            }
        }
        if let Some((k, sc)) = best_nul {
            if best.map(|(_, s)| sc > s).unwrap_or(true) {
                best = Some((IndexDialect::NulXor(k), sc));
            }
        }
        best.ok_or_else(|| Error::format("ypf 索引方言判别失败: 两族都无法一致解析"))
    }

    /// 自动判别方言并按之解析(跨版本通用入口)。
    pub fn from_path_auto(path: &std::path::Path) -> Result<(Self, IndexDialect, f32)> {
        Self::detect_dialect_full(path).map(|(idx, d, sc)| (idx, d, sc))
    }

    /// [`Self::detect_dialect`] 的**完整版**：判别方言并**一并返回已解析的索引**。
    ///
    /// 为什么需要（2026-10-18 性能修复）：旧 `from_path_auto` 先
    /// `detect_dialect`（对候选 key 逐个全量解析）再按结果**又全量解析一次**
    /// —— 同一个 3.3 GB 包的索引块被读并解析 2 次以上。这里把判别阶段
    /// 解析出的最优结果直接返回，省掉重复解析。
    pub fn detect_dialect_full(
        path: &std::path::Path,
    ) -> Result<(Self, IndexDialect, f32)> {
        let mut best: Option<(IndexDialect, f32, Self)> = None;
        let file_len = std::fs::metadata(path)?.len() as u32;
        let mut head = [0u8; 0x24];
        {
            use std::io::Read;
            std::fs::File::open(path)?.read_exact(&mut head)?;
        }
        let first_data_off = u32::from_le_bytes(head[0x0C..0x10].try_into().unwrap());
        let head_file_count = u32::from_le_bytes(head[0x08..0x0C].try_into().unwrap());
        let tol = |n: usize, want: usize| n + 2 >= want && n <= want;

        // LenEnc 族
        if let Ok(idx) = Self::from_path_lenenc(path) {
            if tol(idx.entries.len(), idx.header.file_count as usize) {
                let sc = Self::structural_score(&idx.entries, file_len, first_data_off);
                best = Some((IndexDialect::LenEncNot, sc, idx));
            }
        }

        // NUL 族：先预筛，通过的才全量解析（详见 `probe_key`）。
        let probe_block: Option<Vec<u8>> = (|| {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(path).ok()?;
            let mut h = [0u8; 0x24];
            f.read_exact(&mut h).ok()?;
            let fdo = u32::from_le_bytes(h[0x0C..0x10].try_into().ok()?);
            let index_len = (fdo as usize).checked_sub(0x24)?;
            f.seek(SeekFrom::Start(0x24)).ok()?;
            let mut block = vec![0u8; index_len];
            f.read_exact(&mut block).ok()?;
            Some(block)
        })();
        for k in 0u16..=255 {
            let k = k as u8;
            if let Some(block) = &probe_block {
                if !Self::probe_key(block, head_file_count, file_len, first_data_off, k, 8) {
                    continue;
                }
            }
            let Ok(idx) = Self::from_path(path, k) else {
                continue;
            };
            if idx.entries.is_empty() || !tol(idx.entries.len(), idx.header.file_count as usize) {
                continue;
            }
            let sc = Self::structural_score(&idx.entries, file_len, first_data_off);
            if std::env::var_os("YURIS_TRACE_BOOT").is_some() {
                eprintln!("[ypf] candidate key={k} entries={} score={sc:.3}", idx.entries.len());
            }
            let better = best.as_ref().map(|(_, s, _)| sc > *s).unwrap_or(true);
            if better {
                best = Some((IndexDialect::NulXor(k), sc, idx));
            }
        }
        match best {
            Some((d, sc, idx)) => Ok((idx, d, sc)),
            None => Err(Error::format("ypf 索引方言判别失败: 两族都无法一致解析")),
        }
    }

    /// 从磁盘文件解析索引区。
    pub fn from_path(path: &std::path::Path, name_key: u8) -> Result<Self> {
        use std::io::{Read, Seek, SeekFrom};

        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; 0x24];
        f.read_exact(&mut head)?;
        let mut r = Reader::new(&head);
        let magic = r.u32_bytes()?;
        if magic != YPF_MAGIC {
            return Err(Error::BadMagic {
                expected: YPF_MAGIC,
                actual: magic,
            });
        }
        let version = r.u32_le()?;
        let file_count = r.u32_le()?;
        let first_data_off = r.u32_le()?;

        // 索引区(0x24..first_data_off)整体读入后按同一游走逻辑解析
        let index_len = (first_data_off as usize)
            .checked_sub(0x24)
            .ok_or_else(|| Error::format("ypf index: first_data_off < 0x24"))?;
        f.seek(SeekFrom::Start(0x24))?;
        let mut block = vec![0u8; index_len];
        f.read_exact(&mut block)?;

        let mut r = Reader::new(&block);
        let file_len = std::fs::metadata(path)?.len() as u32;
        let mut names = std::collections::HashSet::with_capacity(file_count as usize);
        let mut flags = Vec::with_capacity(file_count as usize);
        let mut entries: Vec<YpfEntryInfo> = Vec::with_capacity(file_count as usize);
        let mut map: std::collections::HashMap<Vec<u8>, usize> =
            std::collections::HashMap::with_capacity(file_count as usize);
        for _ in 0..file_count {
            // ---- 名字边界:NUL 候选结构化验证 ----
            // cstring 扫描假设名字不含存储态 0x00,但 SJIS 名的合法尾字节
            // 0xC9 与 name_key 异或后恰为 0x00(如「に」= 0x82C9,NEKO-NIN
            // exHeart se.ypf 实证),会在名字中途提前命中 NUL → 整包解析
            // 漂移。因此对 rest 内每个 0x00 候选做字段结构校验
            // (stored 特征 uncomp==comp / bn 边界落界),取首个通过者;
            // 全部失败时回退首个 NUL(维持旧行为)。
            let rest = r.rest();
            let base = r.pos();
            let mut nul_rel: Option<usize> = None;
            let mut scan = 0usize;
            while let Some(rel) = rest[scan..].iter().position(|&b| b == 0) {
                let cand = scan + rel;
                let np = base + cand + 1;
                let name = decode_xor_cstring(&rest[..cand], name_key);
                if name_boundary_valid(&block, np, name.as_slice(), file_len, first_data_off) {
                    nul_rel = Some(cand);
                    break;
                }
                scan = cand + 1;
            }
            let cand = nul_rel.unwrap_or_else(|| {
                rest.iter().position(|&b| b == 0).unwrap_or(rest.len())
            });
            let mut name_bytes = decode_xor_cstring(&rest[..cand], name_key);
            let raw_len = name_bytes.len();
            let np = r.pos() + raw_len + 1; // 名字(根前缀+路径+可能尾缀码) + NUL 后
            r.skip(raw_len + 1)?;
            let b0 = block.get(np).copied().unwrap_or(0);
            // 布局判别(成果 63 勘误,probe_census.py 全包实证):
            // 名字段 = 虚拟根字节(1B) + 路径 + [类型码(1B,仅 se 型)];
            // bn 型(name NUL flag uncomp...):flag∈{0,1}(zlib/stored);
            // se 型(name 类型码 NUL uncomp...):类型码为**存储原值** 2/5/6。
            // 复合判别(2026-09-05 勘误:原实现此处取反,真实 bn 条目
            // (b0∈{0,1} 且 bn 解释落界)全部被错判成 se → sc.ypf 等纯 bn 包
            // 自第 2 条起逐条漂移 1 字节;此前被「se 包走类型码路径 / bn 包走
            // YpfArchive 分区解析器」双巧合掩盖,sc_zlib_text_read 暴露)。
            // se 型类型码按**存储原值**判(跨版本勘误,见 `name_boundary_valid`)。
            // 注意 `np` 指向 NUL 之后,故原值为 `block[np-1]`。
            let raw_code = np.checked_sub(1).and_then(|i| block.get(i)).copied();
            let code = match raw_code {
                // 0x02=PNG / 0x06=OGG / 0x05=WAV(实测;与 name_key 无关)
                Some(c @ (2 | 5 | 6)) => Some(c),
                _ => None,
            };
            let is_bn = if code.is_none() {
                if b0 <= 1 {
                    let off_a = u32::from_le_bytes(
                        block.get(np + 9..np + 13).unwrap_or(&[0xff; 4]).try_into().unwrap(),
                    );
                    let comp_a = u32::from_le_bytes(
                        block.get(np + 5..np + 9).unwrap_or(&[0xff; 4]).try_into().unwrap(),
                    );
                    first_data_off <= off_a && off_a.saturating_add(comp_a) <= file_len
                } else {
                    false
                }
            } else {
                false
            };
            let (flag, code_stripped) = if is_bn {
                (b0, false)
            } else if let Some(c) = code {
                (c, true)
            } else {
                // se 未知码(如 se.ypf 4 个 2 字节条目):不剥尾,记 Unknown
                (b0, false)
            };
            if code_stripped {
                name_bytes.truncate(raw_len - 1);
            }
            // bn 型:flag 字节占位须先消费(字段在 NUL 后 flag+1 起)
            if is_bn {
                r.skip(1)?;
            }
            let uncompressed_len = r.u32_le()?;
            let compressed_len = r.u32_le()?;
            let offset = r.u32_le()?;
            let _reserved = r.u32_le()?;
            // tail 8 字节(末条可被 first_data_off 截断)
            let avail = (block.len() - r.pos()).min(8);
            r.skip(avail)?;
            flags.push(flag);
            // 双索引:全名(根+路径)与剥根路径并存(引擎侧两种查询形态:
            // YpfScriptHost 用 $ysbin\...(带根);FILEINFO EXIST 用 cg/...(无根))
            map.entry(name_bytes.clone()).or_insert(entries.len());
            if name_bytes.len() > 1 {
                map.entry(name_bytes[1..].to_vec()).or_insert(entries.len());
            }
            entries.push(YpfEntryInfo {
                name: name_bytes.clone(),
                flag,
                uncompressed_len,
                compressed_len,
                offset,
            });
            names.insert(name_bytes);
        }

        Ok(Self {
            names,
            header: YpfHeader {
                version,
                file_count,
                first_data_off,
            },
            flags,
            entries,
            map,
        })
    }
}

/// NUL 候选结构化校验(名字边界消歧)。
///
/// `np` = 假定 NUL 之后的字段起点,`name` = 该候选下 XOR 解码的名字。
/// 通过条件(满足其一):
/// - **se 型**:NUL 前的字节为**原始**类型码(`2`/`5`/`6`)且 uncomp/comp 字段
///   落在索引块内 —— stored 特征 `uncomp == comp`(PNG/OGG/WAV 全部明文
///   等长,NEKO-NIN exHeart 全包实证)再验 offset 落在文件范围;
/// - **bn 型**:NUL 后首字节 ∈ {0,1}(flag)且 offset ≥ first_data_off、
///   offset+comp ≤ 文件长(与主判别一致)。
/// - 其余形态:不通过(交由上层继续尝试下一候选/回退首个 NUL)。
fn name_boundary_valid(
    block: &[u8],
    np: usize,
    name: &[u8],
    file_len: u32,
    first_data_off: u32,
) -> bool {
    // se 型判据:紧贴 NUL 前的字节 = **存储原值**类型码(`2=PNG / 5=WAV / 6=OGG`)。
    //
    // 勘误(跨版本):旧实现判的是**解码后**的首字节表 `{0xCB,0xCF,0xCC}` ——
    // 那恰好是 `key = 0xC9` 时 `{2,5,6} ^ 0xC9` 的结果,等价于把 name_key
    // 硬编码进校验器。换一把 key(实测另一发行作品为 `0xFF`)后该分支永不命中,
    // 索引游走每一步都退回「首个 NUL」→ 全表名字漂移。
    //
    // 类型码是**数据侧**值,与 name_key 无关,故按原始字节判。
    let _ = name;
    let raw_tail = np.checked_sub(1).and_then(|i| block.get(i)).copied();
    if matches!(raw_tail, Some(2 | 5 | 6)) {
        let Some(b) = block.get(np..np + 8) else {
            return false;
        };
        let uncomp = u32::from_le_bytes(b[0..4].try_into().unwrap());
        let comp = u32::from_le_bytes(b[4..8].try_into().unwrap());
        return uncomp == comp && comp > 0;
    }
    let b0 = block.get(np).copied().unwrap_or(0xFF);
    if b0 <= 1 {
        let Some(b) = block.get(np + 1..np + 17) else {
            return false;
        };
        let comp = u32::from_le_bytes(b[4..8].try_into().unwrap());
        let off = u32::from_le_bytes(b[8..12].try_into().unwrap());
        return off >= first_data_off && off.saturating_add(comp) <= file_len;
    }
    false
}

/// seek 式单包读取器(P6.3,成果 64):`File` + [`YpfIndex`],按需随机读取。
///
/// cg.ypf 级封包(827MB)不整载 —— 索引一次解析,条目按 `offset` seek 读。
/// flag=1(bn 型)→ zlib 解压;其余 stored 原样(se 型类型码 0x02/0x06 均
/// 明文,实证见成果 63)。
pub struct YpfReader {
    file: std::fs::File,
    index: YpfIndex,
}

impl YpfReader {
    /// 打开封包并解析索引区。
    ///
    /// `name_key` 为文件名 XOR key(样本 `0xC9`)。
    /// 用**已解析的索引**构建读取器(方言判别后复用,避免二次扫描)。
    pub fn from_index(path: &std::path::Path, index: YpfIndex) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(Self { file, index })
    }

    /// 按路径 + `name_key` 打开:先解析索引,再持有文件句柄供随机读。
    pub fn open(path: &std::path::Path, name_key: u8) -> Result<Self> {
        let index = YpfIndex::from_path(path, name_key)?;
        let file = std::fs::File::open(path)?;
        Ok(Self { file, index })
    }

    /// 索引(条目名/flag/长度/偏移)。
    pub fn index(&self) -> &YpfIndex {
        &self.index
    }

    /// 读取条目(原始字节名,精确匹配)并按 flag 解压。
    pub fn read(&mut self, name: &[u8]) -> Result<Vec<u8>> {
        let e = self
            .index
            .map
            .get(name)
            .map(|&i| &self.index.entries[i])
            .ok_or_else(|| Error::format(format!("entry not found: {:?}", String::from_utf8_lossy(name))))?;
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(e.offset as u64))?;
        let mut raw = vec![0u8; e.compressed_len as usize];
        self.file.read_exact(&mut raw)?;
        // 压缩判据走**数据侧**：`flag == 1` 或「头部是 zlib 且长度不等」。
        //
        // 理由：`flag` 字节在两种索引方言里含义不同(一族是压缩标记,另一族是
        // 内容类型码),而 zlib 头(`78 01/9c/da`)是数据本身的事实 —— 按它判
        // 无需知道方言,也不会因方言差异漏解压。
        let looks_zlib = looks_zlib(&raw, e.uncompressed_len, e.compressed_len);
        if e.flag == FLAG_ZLIB || looks_zlib {
            let mut out = Vec::with_capacity(e.uncompressed_len as usize);
            ZlibDecoder::new(&raw[..]).read_to_end(&mut out)?;
            if out.len() != e.uncompressed_len as usize {
                return Err(Error::format(format!(
                    "entry {:?}: decompressed {} bytes, header says {}",
                    String::from_utf8_lossy(name),
                    out.len(),
                    e.uncompressed_len
                )));
            }
            Ok(out)
        } else {
            Ok(raw)
        }
    }
}

#[cfg(test)]
mod name_ext_tests {
    use super::name_has_known_ext;

    #[test]
    fn recognizes_known_extensions() {
        assert!(name_has_known_ext(br"$ysbin\yst00001.ybn"));
        assert!(name_has_known_ext(&[0x16, b'v', b'o', b'i', b'c', b'e', 0x5c, b'a', b'.', b'o', b'g', b'g']));
        assert!(name_has_known_ext(br"cg\a.PNG"), "大小写不敏感");
        assert!(name_has_known_ext(b"a.txt"));
    }

    /// se 型类型码紧贴名字尾，须先剥掉再判扩展名。
    #[test]
    fn ignores_trailing_type_code_byte() {
        assert!(name_has_known_ext(b"voice.ogg"));
        assert!(name_has_known_ext(b"cg.png"));
        assert!(name_has_known_ext(&[0x12, b'a', b'.', b'y', b'b', b'n']));
    }

    #[test]
    fn rejects_garbage_and_unknown_ext() {
        assert!(!name_has_known_ext(b""));
        assert!(!name_has_known_ext(b"\x00\x01\x02"));
        assert!(!name_has_known_ext(b"something.xyz"));
        assert!(!name_has_known_ext(b"noext"));
    }
}

#[cfg(test)]
mod data_side_tests {
    use super::*;

    #[test]
    fn zlib_predicate_needs_header_and_length_gap() {
        assert!(looks_zlib(&[0x78, 0xda, 0x00], 100, 50));
        assert!(looks_zlib(&[0x78, 0x01, 0x00], 100, 50));
        assert!(looks_zlib(&[0x78, 0x9c, 0x00], 100, 50));
        // 明文 stored:长度相等 → 不是压缩(即 PNG/OGG/WAV 条目)
        assert!(!looks_zlib(&[0x78, 0xda, 0x00], 50, 50));
        // 非 zlib 头(实测 .jpg/.psd 走 zlib,PNG 走明文)
        assert!(!looks_zlib(&[0x89, 0x50, 0x4e, 0x47], 100, 50));
    }

    #[test]
    fn image_predicate_by_magic_not_by_index_code() {
        const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert!(looks_like_image(&PNG));
        assert!(looks_like_image(&[0xff, 0xd8, 0xff, 0xe0]));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPVP8 ");
        assert!(looks_like_image(&webp));
        assert!(looks_like_image(b"BMzz"));
        assert!(!looks_like_image(&[0x78, 0xda, 0x00]));
        assert!(!looks_like_image(b"OggSzzzz"));
    }
}

#[cfg(test)]
mod probe_key_tests {
    //! 方言预筛的回归（2026-10-18 启动性能修复）。
    //!
    //! 背景：`detect_dialect` 的 NUL 族原有实现对 **256 个候选 key** 各做一次
    //! **全量**索引解析。实测 3.3 GB / 12,211 条的发行包上这一步要 **2.5 秒**
    //! （启动总耗时 26s 的主要构成）。
    //!
    //! 预筛必须真的能淘汰错误 key —— 最初版本只校验「字段结构」（名字非空、
    //! 偏移落在数据区），结果 **256 个 key 全部通过**（错误 key 异或出来的
    //! 字节同样能凑出合法偏移），优化完全不生效。加上「名字必须是含分隔符的
    //! 可打印路径」后才真正筛掉。

    use super::*;

    /// 构造一个最小 NUL 族索引块：`name XOR key` + NUL + flag + 5×u32。
    fn block_for(names: &[&str], key: u8, first_data_off: u32) -> Vec<u8> {
        let mut b = Vec::new();
        for n in names {
            for byte in n.as_bytes() {
                b.push(byte ^ key);
            }
            b.push(0); // NUL 终止
            b.push(0); // flag（bn 型 0/1）
            b.extend_from_slice(&100u32.to_le_bytes()); // uncompressed
            b.extend_from_slice(&100u32.to_le_bytes()); // compressed
            b.extend_from_slice(&first_data_off.to_le_bytes()); // offset
            b.extend_from_slice(&0u32.to_le_bytes()); // reserved
        }
        b
    }

    #[test]
    fn accepts_the_correct_key() {
        let names = ["cg/ev/a.jpg", "voice/b.ogg"];
        let block = block_for(&names, 0xC9, 0x1000);
        assert!(YpfIndex::probe_key(&block, names.len() as u32, 1 << 20, 0x1000, 0xC9, 8));
    }

    #[test]
    fn rejects_wrong_keys() {
        let names = ["cg/ev/a.jpg", "voice/b.ogg"];
        let block = block_for(&names, 0xC9, 0x1000);
        let mut passed = 0;
        for k in 0u16..=255 {
            let k = k as u8;
            if k == 0xC9 {
                continue;
            }
            if YpfIndex::probe_key(&block, names.len() as u32, 1 << 20, 0x1000, k, 8) {
                passed += 1;
            }
        }
        // 允许极少数「碰巧也像合法路径」的错误 key（实测 2 条名字时 5 个），
        // 关键量级：从 **256 次全量解析** 降到个位数。
        assert!(
            passed <= 16,
            "预筛淘汰率不足:{} 个错误 key 通过(会把启动拖回 2.5s 级)",
            passed
        );
    }

    #[test]
    fn rejects_names_without_separator() {
        let block = block_for(&["abcdef"], 0, 0x1000);
        assert!(!YpfIndex::probe_key(&block, 1, 1 << 20, 0x1000, 0, 8));
    }

    #[test]
    fn rejects_control_characters_in_name() {
        let block = block_for(&["cg/a.jpg"], 0, 0x1000);
        assert!(!YpfIndex::probe_key(&block, 1, 1 << 20, 0x1000, 0, 8));
    }

    #[test]
    fn rejects_truncated_block() {
        let names = ["cg/a.jpg"];
        let mut block = block_for(&names, 0, 0x1000);
        let n = block.len();
        block.truncate(n - 4);
        assert!(!YpfIndex::probe_key(&block, 1, 1 << 20, 0x1000, 0, 8));
    }
}
