//! YSSD 系统数据文件（`save/*.sd`）+ 压缩层（zlib / SNP 双向）。
//!
//! 规格（**Confirmed**，2026-09-05 侦查，样本 = 标定样本 save/ 六文件 28 块全解码；
//! 2026-10-18 按 v494 引擎反汇编勘误压缩层与块类型，见下）：
//!
//! ```text
//! [Header 0x1010B]  magic b"YSSD" + version u32(0x1E0) + 块数 u32
//!                   + 偏移表容量 u32(0x400) + 偏移表 0x400 × u32
//!                   （表项[i] = 块号 i+1 的绝对偏移；0 = 空）
//! [Block]           u32 块号(=DNO) + u8 类型 + u8 严格维数标志
//!                   + u16 目标变量 id + u32 压缩长 + u32 未压长
//!                   + 压缩载荷
//! [载荷(解压后)]     u32 类型(1=INT 2=FLT 3=STR) + u32 维数 + u32[维数] 边界
//!                   + u32 数据长 + 数据（INT/FLT = 8B/元素；STR = 逐元素
//!                   {u32 长度 + 字节}，元素数 = 维数积）
//! ```
//!
//! 容器布局与偏移表寻址（`fseek(DNO*4 + 0xC)` ≡ `0x10 + (DNO-1)*4`）由
//! v494 引擎 LOAD 处理器（`0x446ed0`，`cmp edx, 0x44535359`@`0x447170`）
//! 与 SAVE 写盘路径（`0x44cba7` 起：magic/版本/块数/容量 4×u32 + memset
//! 0x1000 偏移表 + 逐块 `dno u32` + 预留体）双侧证实。
//!
//! 压缩层勘误（2026-10-18，v494 引擎反汇编）：
//! - **v494 构建没有 SNP**。LOAD 解压 = 静态链接 zlib `uncompress`
//!   （`0x4475bc` → `call 0x48c170`），SAVE 压缩 = `compress2(…, level=1)`
//!   （`call 0x48c224`@`0x44d707`/`0x44d8a9`，门控 `[0x8025d4]==0`）。
//!   标定样本（另一发行作品）save/ 全部 SNP（YSSNP.DLL 反汇编，成果 60），
//!   第二发行作品 v494 save/ 全部 zlib（`78 01` 头，本文件
//!   [`snp_uncompress`] 解不开、zlib 全量闭合）—— 压缩器是**按引擎构建**
//!   选择的，本模块读侧按**数据侧判据**（首字节 CMF）自动分流。
//! - 块类型 0 = 上述单变量载荷；类型 1 = 配置块（见
//!   [`parse_config_payload`]）；类型 2 = 多变量块（见
//!   [`parse_multi_payload`]，外层不压缩、子块各自压缩）。
//!
//! SNP = **snappy 变体**：头部 varint 未压长度（7-bit LE base-128）；
//! 元素 tag 低 2 位 00=字面量 / 01=copy1 / 10=copy2 / 11=copy4；
//! **字面量长度 = (tag>>2)+1**（非标准 snappy 的 tag>>2，引擎查表
//! `DAT_1000ba40`/`FUN_10001d40` 实锤）；长字面量（tag>>2 ∈ 60..63）
//! 后随 1..4 字节小端长度（同样 +1）；copy 族与标准 snappy 相同。
//! 本模块另提供按同一规格的**编码器** [`snp_compress`]（成果 60 登记的
//! 「SNP 编码器届时落地」），供读取侧回归与跨引擎写侧实验。

use std::io::{Read, Write};

use yuris_core::{Error, Result};

/// YSSD magic：`YSSD`。
pub const YSSD_MAGIC: [u8; 4] = *b"YSSD";

/// 偏移表容量（样本恒 0x400）。
pub const OFFSET_TABLE_LEN: usize = 0x400;

/// 一个 YSSD 块（压缩形态）。
#[derive(Debug, Clone, PartialEq)]
pub struct YssdBlock {
    /// 块号（= DNO，1 基；脚本 LOAD 槽 2）。
    pub dno: u32,
    /// 块类型（0 = 普通单块；2 = 多子块 —— 语料未出现，不实现）。
    pub btype: u8,
    /// 严格维数标志（1 = 载荷维数必须与变量声明完全一致）。
    pub strict: u8,
    /// 目标变量 id（与脚本槽 3 引用一致；双重记录）。
    pub var_id: u16,
    /// SNP 压缩载荷。
    pub compressed: Vec<u8>,
}

/// 已解析的 YSSD 文件。
#[derive(Debug, Clone)]
pub struct YssdFile {
    /// 版本（样本 0x1E0）。
    pub version: u32,
    /// 块表（按出现顺序；DNO → 块经 `block()` 查找）。
    blocks: Vec<YssdBlock>,
    /// DNO → blocks 下标（偏移表展开）。
    index: Vec<Option<u32>>,
}

impl YssdFile {
    /// 从字节解析（header + 偏移表 + 全部块头）。
    ///
    /// 块数据区整体读取（块间可能共享尾部容差 —— 逐块按压缩长切分）。
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() < 0x10 {
            return Err(Error::format("YSSD too short"));
        }
        if &data[0..4] != &YSSD_MAGIC {
            return Err(Error::BadMagic {
                expected: YSSD_MAGIC,
                actual: data[0..4].try_into().unwrap(),
            });
        }
        let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
        if version != 0x1E0 {
            return Err(Error::format(format!(
                "YSSD version {version:#x} != 0x1E0 (引擎 0x1a608 同族)"
            )));
        }
        let table_end = 0x10 + OFFSET_TABLE_LEN * 4;
        if data.len() < table_end {
            return Err(Error::format("YSSD offset table truncated"));
        }
        let mut blocks = Vec::new();
        let mut index = vec![None::<u32>; OFFSET_TABLE_LEN];
        for (i, off) in data[0x10..table_end]
            .chunks_exact(4)
            .enumerate()
            .map(|(i, c)| (i, u32::from_le_bytes(c.try_into().unwrap())))
        {
            if off == 0 {
                continue;
            }
            let off = off as usize;
            if off + 0x10 > data.len() {
                return Err(Error::format(format!(
                    "YSSD block[{}] offset {off:#x} out of range",
                    i + 1
                )));
            }
            let dno = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            let btype = data[off + 4];
            let strict = data[off + 5];
            let var_id = u16::from_le_bytes(data[off + 6..off + 8].try_into().unwrap());
            let clen = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap()) as usize;
            if off + 0x10 + clen > data.len() {
                return Err(Error::format(format!(
                    "YSSD block[{}] payload truncated (need {clen}B)",
                    i + 1
                )));
            }
            index[i] = Some(blocks.len() as u32);
            blocks.push(YssdBlock {
                dno,
                btype,
                strict,
                var_id,
                compressed: data[off + 0x10..off + 0x10 + clen].to_vec(),
            });
        }
        Ok(Self { version, blocks, index })
    }

    /// 按 DNO（1 基）取块。
    pub fn block(&self, dno: u32) -> Option<&YssdBlock> {
        if dno == 0 || dno as usize > OFFSET_TABLE_LEN {
            return None;
        }
        self.index
            .get((dno - 1) as usize)
            .and_then(|s| *s)
            .and_then(|i| self.blocks.get(i as usize))
    }

    /// 块总数。
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }
}

/// SNP（snappy 变体）解压。
///
/// 字面量长度 = `(tag>>2)+1`；copy1/2/4 与标准 snappy 一致
///（YSSNP.DLL `FUN_10001d40` + 派发表 `DAT_1000ba40` 实锤）。
pub fn snp_uncompress(src: &[u8]) -> Result<Vec<u8>> {
    // varint 未压长度（最多 5 字节）
    let mut expected: u64 = 0;
    let mut p = 0;
    let mut shift = 0;
    let mut done = false;
    while p < src.len() && shift < 35 {
        let b = src[p];
        p += 1;
        expected |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            done = true;
            break;
        }
        shift += 7;
    }
    if !done {
        return Err(Error::format("SNP varint header missing"));
    }
    let expected = expected as usize;
    let mut out = Vec::with_capacity(expected);
    while p < src.len() {
        let tag = src[p];
        match tag & 3 {
            0 => {
                // 字面量:长度 = (tag>>2)+1;60..63 → 1..4 额外长度字节(同样 +1)
                let mut n = (tag >> 2) as usize + 1;
                p += 1;
                if n > 0x3C {
                    let extra = n - 1 - 59;
                    if p + extra > src.len() {
                        return Err(Error::format("SNP literal length truncated"));
                    }
                    let mut v: usize = 0;
                    for (k, b) in src[p..p + extra].iter().enumerate() {
                        v |= (*b as usize) << (8 * k);
                    }
                    p += extra;
                    n = v + 1;
                }
                if p + n > src.len() {
                    return Err(Error::format("SNP literal truncated"));
                }
                out.extend_from_slice(&src[p..p + n]);
                p += n;
            }
            1 => {
                // copy1:len = 4 + ((tag>>2)&7);offset = ((tag>>5)<<8)|下一字节
                if p + 2 > src.len() {
                    return Err(Error::format("SNP copy1 truncated"));
                }
                let n = 4 + ((tag >> 2) & 7) as usize;
                let off = (((tag >> 5) as usize) << 8) | src[p + 1] as usize;
                p += 2;
                copy_back(&mut out, off, n)?;
            }
            2 => {
                // copy2:len = (tag>>2)+1;offset = u16 LE
                if p + 3 > src.len() {
                    return Err(Error::format("SNP copy2 truncated"));
                }
                let n = (tag >> 2) as usize + 1;
                let off = u16::from_le_bytes([src[p + 1], src[p + 2]]) as usize;
                p += 3;
                copy_back(&mut out, off, n)?;
            }
            _ => {
                // copy4:len = (tag>>2)+1;offset = u32 LE
                if p + 5 > src.len() {
                    return Err(Error::format("SNP copy4 truncated"));
                }
                let n = (tag >> 2) as usize + 1;
                let off = u32::from_le_bytes(src[p + 1..p + 5].try_into().unwrap()) as usize;
                p += 5;
                copy_back(&mut out, off, n)?;
            }
        }
    }
    if out.len() != expected {
        return Err(Error::format(format!(
            "SNP length mismatch: got {}, header says {expected}",
            out.len()
        )));
    }
    Ok(out)
}

/// 回引用拷贝（offset 相对 out 末尾；逐字节 —— 引擎语义允许重叠）。
fn copy_back(out: &mut Vec<u8>, off: usize, n: usize) -> Result<()> {
    if off == 0 || off > out.len() {
        return Err(Error::format(format!(
            "SNP copy offset {off} out of range (out len {})",
            out.len()
        )));
    }
    for _ in 0..n {
        let b = out[out.len() - off];
        out.push(b);
    }
    Ok(())
}

/// 解压后的 YSSD 载荷（`FUN_0044564d` 消费形态）。
#[derive(Debug, Clone, PartialEq)]
pub struct YssdPayload {
    /// 类型：1=INT 2=FLT 3=STR（须与目标变量描述符类型一致）。
    pub ty: u8,
    /// 维数（0 = 标量）。
    pub dims: Vec<u32>,
    /// 数据（INT/FLT = 8B/元素；STR = 逐元素 {u32 长度 + 字节}）。
    pub data: Vec<u8>,
}

impl YssdPayload {
    /// 从解压后的字节解析。
    pub fn from_bytes(raw: &[u8]) -> Result<Self> {
        if raw.len() < 12 {
            return Err(Error::format("YSSD payload too short"));
        }
        let ty = u32::from_le_bytes(raw[0..4].try_into().unwrap()) as u8;
        let dim = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
        if dim > 8 {
            return Err(Error::format(format!(
                "YSSD payload dim count {dim} > 8 (引擎 900000 同族)"
            )));
        }
        if raw.len() < 8 + dim * 4 + 4 {
            return Err(Error::format("YSSD payload dims truncated"));
        }
        let dims = raw[8..8 + dim * 4]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let dlen = u32::from_le_bytes(
            raw[8 + dim * 4..12 + dim * 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let data_start = 12 + dim * 4;
        if raw.len() < data_start + dlen {
            return Err(Error::format("YSSD payload data truncated"));
        }
        Ok(Self {
            ty,
            dims,
            data: raw[data_start..data_start + dlen].to_vec(),
        })
    }

    /// 序列化（[`YssdPayload::from_bytes`] 的逆；SAVE 写侧用）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.dims.len() * 4 + self.data.len());
        out.extend_from_slice(&(self.ty as u32).to_le_bytes());
        out.extend_from_slice(&(self.dims.len() as u32).to_le_bytes());
        for &d in &self.dims {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }
}

/// 块载荷解压(数据侧自动分流;zlib 与 SNP 按**可解性**确定,不靠单字节
/// 猜测 —— SNP varint 首字节的低 4 位也可能是 8,单看 CMF 位会误判,
/// 实测 ulen=24 的 SNP 流首字节 0x18 即触发)。
///
/// v494 引擎产 zlib(`0x4475bc` → 静态 zlib uncompress)、标定样本引擎产
/// SNP(YSSNP.DLL);文件本身即判据,不依赖引擎版本推断。
pub fn decompress_block_payload(payload: &[u8]) -> Result<Vec<u8>> {
    // zlib 头判据:CMF 低 4 位 = 8(deflate)且 (CMF<<8|FLG) % 31 == 0(RFC1950)。
    let zlib_header = payload.len() >= 2
        && payload[0] & 0x0f == 0x08
        && u16::from_be_bytes([payload[0], payload[1]]) % 31 == 0;
    let mut zlib_err = None;
    if zlib_header {
        let mut out = Vec::new();
        match flate2::read::ZlibDecoder::new(payload).read_to_end(&mut out) {
            Ok(_) => return Ok(out),
            Err(e) => zlib_err = Some(e.to_string()),
        }
    }
    snp_uncompress(payload).map_err(|snp_e| {
        Error::format(match zlib_err {
            Some(z) => format!("载荷既非 zlib({z})也非 SNP({snp_e})"),
            None => snp_e.to_string(),
        })
    })
}

/// 块载荷压缩（v494 引擎对齐：`compress2(level=1)`，`0x44d707` 实测）。
pub fn compress_block_payload(raw: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(
        Vec::with_capacity(raw.len() / 2 + 64),
        flate2::Compression::new(1),
    );
    enc.write_all(raw)
        .expect("ZlibEncoder over Vec never fails on write_all");
    enc.finish().expect("ZlibEncoder finish never fails")
}

/// SNP（snappy 变体）压缩 —— [`snp_uncompress`] 的逆。
///
/// 字面量长度 = (tag>>2)+1（60..63 → 后随 1..4 字节 LE 长度，同样 +1）；
/// copy1/copy2/copy4 与标准 snappy 同形。哈希链贪心匹配（4 字节步进），
/// 输出只保证「被 [`snp_uncompress`] 解回原文」，不追求与 YSSNP.DLL
/// 逐字节一致（压缩器实现细节未逆向，也不影响互通）。
pub fn snp_compress(src: &[u8]) -> Vec<u8> {
    fn put_varint(out: &mut Vec<u8>, mut v: usize) {
        loop {
            if v < 0x80 {
                out.push(v as u8);
                return;
            }
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
    }
    fn put_literal(out: &mut Vec<u8>, lit: &[u8]) {
        let n = lit.len();
        // 长度编码:1..60 → tag>>2 = n-1(单 tag);61..256 → 60 + 1B;以此类推。
        if n <= 60 {
            out.push(((n - 1) as u8) << 2);
        } else {
            let (tag, extra): (u8, usize) = if n <= 0x100 {
                (60u8 << 2, 1)
            } else if n <= 0x10000 {
                (61u8 << 2, 2)
            } else if n <= 0x1000000 {
                (62u8 << 2, 3)
            } else {
                (63u8 << 2, 4)
            };
            out.push(tag);
            out.extend_from_slice(&(n - 1).to_le_bytes()[..extra]);
        }
        out.extend_from_slice(lit);
    }
    let mut out = Vec::with_capacity(src.len() + src.len() / 8 + 16);
    put_varint(&mut out, src.len());
    // 4 字节哈希 → 位置表(与标准 snappy 的 block hash 同思路;整块单表)。
    let table_bits: u32 = 20;
    let table_len = 1usize << table_bits;
    let mut table = vec![u32::MAX; table_len];
    let mut lit_start = 0usize;
    let mut i = 0usize;
    let hash = |p: &[u8]| -> usize {
        let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
        (((v.wrapping_mul(0x1e35a7bd)) >> (32 - table_bits)) as usize) & (table_len - 1)
    };
    while i + 4 <= src.len() {
        let h = hash(&src[i..]);
        let cand = table[h];
        table[h] = i as u32;
        let Some(cand) = (cand != u32::MAX).then_some(cand as usize) else {
            i += 1;
            continue;
        };
        let max_off = i - cand;
        if max_off == 0 || max_off > 0xffff {
            i += 1;
            continue;
        }
        // 先验证 4 字节确相等(哈希只筛不证;不证会写出错误 copy)。
        if src[cand..cand + 4] != src[i..i + 4] {
            i += 1;
            continue;
        }
        // 向两端延伸匹配(copy2 上限 64 字节 = (tag>>2)+1 的编码上界)。
        let mut len = 4usize;
        while i + len < src.len() && src[cand + len] == src[i + len] && len < 64 {
            len += 1;
        }
        if lit_start < i {
            put_literal(&mut out, &src[lit_start..i]);
        }
        let off = i - cand;
        if len < 12 && off < 0x800 {
            // copy1: len = 4+((tag>>2)&7), off = (tag>>5)<<8 | b1
            out.push(1 | (((off >> 8) as u8) << 5) | ((((len - 4) as u8) & 7) << 2));
            out.push(off as u8);
        } else {
            // copy2: len = (tag>>2)+1, off = u16
            out.push(2 | (((len - 1) as u8) << 2));
            out.extend_from_slice(&(off as u16).to_le_bytes());
        }
        // 登记匹配窗内位置,保证后续拷贝可被引用。
        for k in i + 1..i + len {
            if k + 4 <= src.len() {
                let hh = hash(&src[k..]);
                table[hh] = k as u32;
            }
        }
        i += len;
        lit_start = i;
    }
    if lit_start < src.len() {
        put_literal(&mut out, &src[lit_start..]);
    }
    out
}

/// btype=2 多变量块的子块（引擎 LOAD `0x4472bd` 循环:
/// `pad u16 @+0 / var u16 @+2 / clen u32 @+4 / ulen u32 @+8 / data[clen] @+12`）。
#[derive(Debug, Clone, PartialEq)]
pub struct YssdSubBlock {
    /// 头部 pad(引擎从不读取;语料六处恒 `0x0100`,语义 Unknown,不猜)。
    pub pad: u16,
    /// 子变量 id（写回目标；与 LET 槽无关）。
    pub var_id: u16,
    /// 压缩长。
    pub clen: u32,
    /// 未压长（与 zlib 解压长精确闭合）。
    pub ulen: u32,
    /// 压缩载荷（zlib，见 [`decompress_block_payload`]）。
    pub compressed: Vec<u8>,
}

/// 解析 btype=2 外层载荷（**裸**，不整体压缩）。
///
/// 布局（引擎 LOAD `0x4472bd` 循环 + 第二发行作品语料实测）:
/// `子块{pad u16, var u16, clen u32, ulen u32, zlib[clen]}×n`——
/// **没有子块计数字段**，引擎循环按「已消费 ≥ 块 clen」终止
/// （`lea ecx,[esi+ebp]; cmp ecx,[esp+0x3c]; jb`），首子块从偏移 0 起。
/// 实测:gsd DNO=3 单子块(333B)、save00001 DNO=5 双子块(339+60=399B)。
pub fn parse_multi_payload(raw: &[u8]) -> Result<Vec<YssdSubBlock>> {
    let mut subs = Vec::new();
    let mut p = 0usize;
    while p < raw.len() {
        if p + 12 > raw.len() {
            return Err(Error::format(format!("YSSD 多变量子块{}头截断", subs.len())));
        }
        let pad = u16::from_le_bytes([raw[p], raw[p + 1]]);
        if pad != 0x0100 {
            return Err(Error::format(format!(
                "YSSD 多变量子块{} pad {pad:#06x} != 语料恒值 0x0100(语义 Unknown,不猜)",
                subs.len()
            )));
        }
        let var_id = u16::from_le_bytes([raw[p + 2], raw[p + 3]]);
        let clen = u32::from_le_bytes(raw[p + 4..p + 8].try_into().unwrap());
        let ulen = u32::from_le_bytes(raw[p + 8..p + 12].try_into().unwrap());
        p += 12;
        if p + clen as usize > raw.len() {
            return Err(Error::format(format!(
                "YSSD 多变量子块{}载荷截断(需 {}B,余 {}B)",
                subs.len(),
                clen,
                raw.len() - p
            )));
        }
        subs.push(YssdSubBlock {
            pad,
            var_id,
            clen,
            ulen,
            compressed: raw[p..p + clen as usize].to_vec(),
        });
        p += clen as usize;
    }
    Ok(subs)
}

/// 编码 btype=2 块体（读侧 [`parse_multi_payload`] 的逆）。
///
/// 外层头 = `btype=2, strict=1, var=0, clen=ulen=内层长`,内层 =
/// `子块{pad=0x0100, var, clen, ulen, zlib[clen]}×n`
/// （实测 gsd.sd DNO=3/4 单子块、save00001 DNO=5 双子块均此形态）。
pub fn encode_multi_body(subs: &[(&'_ u16, &[u8])]) -> Vec<u8> {
    let mut inner = Vec::new();
    for (var, raw) in subs {
        let c = compress_block_payload(raw);
        inner.extend_from_slice(&0x0100u16.to_le_bytes());
        inner.extend_from_slice(&var.to_le_bytes());
        inner.extend_from_slice(&(c.len() as u32).to_le_bytes());
        inner.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        inner.extend_from_slice(&c);
    }
    let mut out = Vec::with_capacity(12 + inner.len());
    out.push(2);
    out.push(1);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    out.extend_from_slice(&(inner.len() as u32).to_le_bytes());
    out.extend_from_slice(&inner);
    out
}

/// btype=1 配置块载荷（引擎 `0x44760c`）:
/// `u32 段A长(实测恒 0x640) + u32 段B长 + 段A + 段B`。
///
/// 语义:段A = 配置描述结构(引擎 memcpy 到局部,首 u32 = 元素计数),
/// 段B = 变量值 blob(按段A 计数 × 元素宽重分配缓冲后整块拷入)。
/// 段内字段含义未逆向(逐字段语义 Unknown),本模块只保证**结构**往返。
#[derive(Debug, Clone, PartialEq)]
pub struct YssdConfigPayload {
    /// 段A(描述结构,原样字节)。
    pub section_a: Vec<u8>,
    /// 段B(值 blob,原样字节)。
    pub section_b: Vec<u8>,
}

/// 解析 btype=1 配置载荷(`raw` = 已解压字节)。
pub fn parse_config_payload(raw: &[u8]) -> Result<YssdConfigPayload> {
    if raw.len() < 8 {
        return Err(Error::format("YSSD 配置载荷截断(无段长头)"));
    }
    let a_len = u32::from_le_bytes(raw[0..4].try_into().unwrap()) as usize;
    let b_len = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
    if raw.len() < 8 + a_len + b_len {
        return Err(Error::format(format!(
            "YSSD 配置载荷截断(头称 A={a_len} B={b_len},实有 {})",
            raw.len() - 8
        )));
    }
    Ok(YssdConfigPayload {
        section_a: raw[8..8 + a_len].to_vec(),
        section_b: raw[8 + a_len..8 + a_len + b_len].to_vec(),
    })
}

/// 编码 btype=1 配置载荷（[`parse_config_payload`] 的逆）。
pub fn encode_config_payload(section_a: &[u8], section_b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + section_a.len() + section_b.len());
    out.extend_from_slice(&(section_a.len() as u32).to_le_bytes());
    out.extend_from_slice(&(section_b.len() as u32).to_le_bytes());
    out.extend_from_slice(section_a);
    out.extend_from_slice(section_b);
    out
}

/// 块体序列化（dno u32 之后的 `btype u8 + strict u8 + var u16 + clen u32 +
/// ulen u32 + 载荷`;SAVE 预留/写盘形态,引擎 `0x44d5a7`/`0x44d740`）。
pub fn encode_block_body(
    btype: u8,
    strict: u8,
    var_id: u16,
    raw_payload: &[u8],
    compress: bool,
) -> Vec<u8> {
    let (clen, payload): (u32, Vec<u8>) = if compress {
        let c = compress_block_payload(raw_payload);
        (c.len() as u32, c)
    } else {
        (raw_payload.len() as u32, raw_payload.to_vec())
    };
    let mut out = Vec::with_capacity(12 + payload.len());
    out.push(btype);
    out.push(strict);
    out.extend_from_slice(&var_id.to_le_bytes());
    out.extend_from_slice(&clen.to_le_bytes());
    out.extend_from_slice(&(raw_payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

/// YSSD 容器写出器（SAVE[FILE] 刷盘形态;引擎 `0x44cba7` 写盘路径）。
///
/// 头 = `magic + 0x1E0 + 块数 + 0x400`,偏移表 0x400×u32(表项[i] =
/// 块号 i+1 的绝对偏移),块 = `dno u32 + 块体`。空构建 = 只有头+全零表
/// (引擎对「FILE 给定但无预留」也写出完整容器)。
#[derive(Debug, Default)]
pub struct YssdBuilder {
    blocks: Vec<(u32, Vec<u8>)>,
}

impl YssdBuilder {
    /// 登记一块(dno 1..=1024;body = [`encode_block_body`] 等的产物)。
    pub fn set_block(&mut self, dno: u32, body: Vec<u8>) {
        debug_assert!((1..=1024).contains(&dno), "DNO 越界(手册 1~1024)");
        self.blocks.push((dno, body));
    }

    /// 已登记块数。
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// 产出完整 .sd 字节。
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(0x1010 + self.blocks.iter().map(|(_, b)| b.len() + 4).sum::<usize>());
        out.extend_from_slice(&YSSD_MAGIC);
        out.extend_from_slice(&0x1E0u32.to_le_bytes());
        out.extend_from_slice(&(self.blocks.len() as u32).to_le_bytes());
        out.extend_from_slice(&(OFFSET_TABLE_LEN as u32).to_le_bytes());
        let table_at = out.len();
        out.resize(table_at + OFFSET_TABLE_LEN * 4, 0);
        for (dno, body) in self.blocks {
            let off = out.len() as u32;
            out.extend_from_slice(&dno.to_le_bytes());
            out.extend_from_slice(&body);
            out[table_at + (dno as usize - 1) * 4..table_at + dno as usize * 4]
                .copy_from_slice(&off.to_le_bytes());
        }
        out
    }
}
