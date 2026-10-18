//! `MATH` 命令的**数学内核**:标准 CRC-32(`HASH`)与 MT19937(`RND`)。
//!
//! 证据链(两条独立线,均为**引擎二进制取证**,见 `PROGRESS.md` 成果 144
//! 与 AGENTS §4):
//!
//! - **`HASH` = 标准 CRC-32**(多项式 `0xEDB88320`、初值 `0xFFFFFFFF`、
//!   末尾取反)。查表实现,表与标准表 256 项逐项相同;检查值
//!   `"123456789" → 0xCBF43926`。
//! - **`RND` 的随机源 = MT19937**(32 位 Mersenne Twister):常数
//!   `0x270`=624、`0xe3`=227=624−397、上下掩码拆分、矩阵常数表
//!   `{0, 0x9908B0DF}` 四条证据唯一指向;`RND` 本身是
//!   `lo + rand64() % (hi − lo + 1)`(两参数按**整数**读,反着给会交换)。
//!
//! ## 为什么只有这两个算法落在这里(其余全用 Rust 标准库)
//!
//! `MATH` 的**一元/二元数学函数**(SIN/COS/TAN/ASIN/ACOS/ATAN/SINH/COSH/
//! TANH/SQRT/EXP/LOG/LOG10/ATAN2/POW)在本内核里**一律直接用 Rust 的
//! `f64` 方法**(见 `exec/misc.rs` 的 `cmd_math`)—— 引擎是"读双精度 → x87
//! 指令/CRT 函数 → 写双精度",与 Rust 同属 IEEE-754 双精度,无需自研数学库。
//!
//! 精度一致性分两类,不得混为一谈:
//!
//! - **四则运算与 `SQRT`**:IEEE-754 要求**正确舍入** ⇒ 任何合规实现
//!   逐位相同(`f64::sqrt` 即引擎 `fsqrt` 的等价物)。
//! - **超越函数**(`SIN`/`EXP`/`POW`/`ATAN2`…):IEEE-754 **不要求**正确舍入
//!   ⇒ 各 CRT/libm 实现可在**最后 1 ULP** 内不同。这一点**无法也不必**逐位
//!   复刻(取决于游戏链接的 MSVC CRT 版本与 x87 中间精度),而对游戏逻辑
//!   (坐标/透明度/进度)无影响;唯一风险面 = 脚本对计算结果做**精确相等**
//!   比较(语料罕见,出现时按具体案例取证)。
//!
//! 而本模块的两个算法**不是**"数学运算",标准库没有对应物:
//!
//! - **CRC-32**:是**校验和算法**,`std` 不提供;但"标准算法"本身就是唯一
//!   真值 —— 表已与引擎表 256 项逐项相同、检查值已钉死,故 30 行自持实现
//!   比引入依赖更小更安全。
//! - **MT19937**:是**状态化 PRNG**,`std` 亦不提供(Rust 的 `rand` 是第三方
//!   且默认不是 MT19937)。"逐位一致"在这里**不由算法决定**,而由**种子与
//!   抽样顺序**决定 —— 那正是引擎侧尚未取证的部分(见 [`Mt19937`] 文档)。
//!   算法本体已按标准 mt19937ar 对齐公开向量。

/// **标准 CRC-32**(`MATH HASH`;引擎对 `SETSTR` 串求此值写回 `LET` 目标)。
///
/// 与 `yuris-core::hash` 的 murmur2 不同:那是引擎的**对象名哈希**,
/// 这是 `MATH` 命令暴露给脚本的校验和 —— 两者用途不同,不得混用。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = CRC32_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// 标准 CRC-32 查表表(编译期生成;等价引擎 `0x5b41c0` 的 256 项)。
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// MT19937 的状态字数(`0x270`;引擎同样用 624)。
pub const N: usize = 624;
/// 中间参数 M(`0xe3` = 227 与状态字数一起确认 397)。
pub const M: usize = 397;
const MATRIX_A: u32 = 0x9908_B0DF;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7FFF_FFFF;

/// MT19937(32 位版;引擎 `MATH RND` 的随机源)。
///
/// **种子来源未知**(引擎 `0x4211c2` 起的初始化路径未取证)—— 本类型提供
/// 显式 [`Mt19937::new`];VM 侧缺省用时间派生种子并在文档登记为 UNVERIFIED
/// (可经 `GroupVm::set_math_seed` 注入做确定性对拍)。**取值范围语义与引擎
/// 一致**(`lo + draw % span`),逐位相同的随机序列需先取证种子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mt19937 {
    state: [u32; N],
    index: usize,
}

impl Mt19937 {
    /// 按标准 MT19937 初始化(种子的选择见类型文档)。
    #[must_use]
    pub fn new(seed: u32) -> Self {
        let mut state = [0u32; N];
        state[0] = seed;
        for i in 1..N {
            state[i] = 1_812_433_253u32
                .wrapping_mul(state[i - 1] ^ (state[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Self { state, index: N }
    }

    /// 抽一个 32 位无符号数(标准回火)。
    pub fn next_u32(&mut self) -> u32 {
        if self.index >= N {
            self.generate();
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9D2C_5680;
        y ^= (y << 15) & 0xEFC6_0000;
        y ^= y >> 18;
        y
    }

    /// 生成下一批 624 个状态(引擎 `0x270` 分支同构)。
    fn generate(&mut self) {
        for i in 0..N {
            let y = (self.state[i] & UPPER_MASK) | (self.state[(i + 1) % N] & LOWER_MASK);
            let mut next = self.state[(i + M) % N] ^ (y >> 1);
            if y & 1 != 0 {
                next ^= MATRIX_A;
            }
            self.state[i] = next;
        }
        self.index = 0;
    }

    /// 抽一个 64 位值(引擎返回值同时用到两个 32 位寄存器)。
    ///
    /// 组合顺序**未逐位取证**(登记 UNVERIFIED):此处按「先抽低 32 位、
    /// 再抽高 32 位」。影响面 = 逐位序列,不影响取值范围。
    pub fn next_u64(&mut self) -> u64 {
        let lo = u64::from(self.next_u32());
        let hi = u64::from(self.next_u32());
        lo | (hi << 32)
    }

    /// 引擎 `RND` 的取模:`lo + draw % (hi − lo + 1)`(区间端点乱序则交换)。
    pub fn range(&mut self, a: i64, b: i64) -> i64 {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let span = hi.wrapping_sub(lo).wrapping_add(1);
        if span <= 0 {
            return lo;
        }
        let span = span as u64;
        let r = self.next_u64() % span;
        lo.wrapping_add(r as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CRC-32 的两个独立锚点:标准检查值 + 引擎表前几项(逐项相同已比对)。
    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(CRC32_TABLE[0], 0x0000_0000);
        assert_eq!(CRC32_TABLE[1], 0x7707_3096);
        assert_eq!(CRC32_TABLE[128], 0xEDB8_8320);
        assert_eq!(CRC32_TABLE[255], 0x2D02_EF8D);
    }

    /// MT19937 公开向量(mt19937ar.c `init_genrand(5489)` 前 5 个输出)。
    #[test]
    fn mt19937_matches_reference_vectors() {
        let mut rng = Mt19937::new(5489);
        let want = [3499211612u32, 581869302, 3890346734, 3586334585, 545404204];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(rng.next_u32(), *w, "第 {i} 个输出");
        }
    }

    /// `RND` 区间语义:结果恒在 `[lo, hi]`;端点反给会交换;跨度 1 恒定。
    #[test]
    fn rnd_range_is_closed_and_swaps_bounds() {
        let mut rng = Mt19937::new(1);
        for _ in 0..200 {
            let v = rng.range(3, 10);
            assert!((3..=10).contains(&v), "越界:{v}");
            let v = rng.range(10, 3);
            assert!((3..=10).contains(&v), "越界:{v}");
        }
        assert_eq!(rng.range(7, 7), 7, "跨度 1 ⇒ 恒定");
    }
}
