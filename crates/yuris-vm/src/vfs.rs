//! VFS —— **引擎语义的虚拟文件系统**(成果 136)。
//!
//! ## 为什么要独立成模块
//!
//! 旧实现把"资源名 → 包内条目"的解析**散成 5 层启发式**,各写各的:
//!
//! | 位置 | 规则 |
//! |---|---|
//! | `resolve_entry` | 原/归一名 + 补扩展名 |
//! | `variant_strips` | 剥 `_单数字` |
//! | `fuzzy_lookup` | 同目录前缀模糊 |
//! | `pick_stand_key` | 立绘档位选择 |
//! | `read_cg_bytes` | `cg\` / `cg\bg\` / `cg\item\` 候选 |
//!
//! 每条都是某次修 bug 时单独加的,**各自生成候选键、各自决定顺序** ——
//! 后果是"每条规则的适用面"只能靠实测反推,而**全局契约**(编码、
//! 大小写、分隔符)反而无人把关。实测(2026-10-15)就是如此:
//!
//! - **CP932 名从未命中过**:索引键是 CP932 字节,而查询键取
//!   `str.as_bytes()`(UTF-8)⇒ 含非 ASCII 的路径永远不等
//!   (`cg\ev\サークル.jpg`;全包 2304/12211 条)
//! - **大小写完全不敏感没有**:索引键大小写敏感,而引擎的 `FUN_00468d40`
//!   (208B)是**不区分大小写的字符串比较**(二进制实证)
//!
//! 本模块是**唯一**的解析路径,全局契约在 [`Vfs::normalize_request`] /
//! [`Vfs::normalize_entry`] 一处实现 —— 两者输出同一形态,
//! 因此索引键与查询键**必然对齐**。
//!
//! ## 引擎契约(全部有证据)
//!
//! | # | 契约 | 证据 | 等级 |
//! |---|---|---|---|
//! | 1 | 包内条目名 = **CP932(SJIS)字节** | YPF 格式实测 | Confirmed |
//! | 2 | 查找**大小写不敏感** | `FUN_00468d40` 逐字节折叠比较 | Confirmed |
//! | 3 | `/` 与 `\` 等价 | Windows 语义 + 语料两种写法并存 | Confirmed |
//! | 4 | 多数据源按序号覆盖,命中即写源序号 | `FUN_0040db04`(数据源重试) | Confirmed |
//! | 5 | 扩展名省略时按 `.png`→`.jpg`→`.bmp`→`.gif` 探测 | 官方手册 `cg/cg.html` | Confirmed |
//! | 6 | 松散文件优先于封包 | FILEPRIORITY 机制(成果 63) | Confirmed |
//!
//! 未取证的启发式(剥 `_N` / 前缀模糊 / 立绘档位)保留在管线里,
//! 但**显式标注等级并集中在 [`ResolveStage`]** —— 它们不再是"隐式兜底",
//! 而是可枚举、可关闭、可单测的具名阶段。

use std::collections::HashMap;
use std::path::PathBuf;

/// 解析命中的来源阶段(诊断/单测用;顺序 = 优先级)。
///
/// 具名化的意义:旧实现的 5 层启发式互相嵌套、无法观测"到底是哪条规则
/// 命中的";出问题时只能靠 print 大法。现在每层有名字,`YURIS_TRACE_VFS=1`
/// 一次运行就能看出资源是靠哪条规则找到的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveStage {
    /// 阶段 1:归一化精确键(含显式扩展名)。引擎契约 1/2/3。
    Exact,
    /// 阶段 2:补扩展名探测 `.png`→`.jpg`→`.bmp`→`.gif`。契约 5(手册)。
    ExtensionProbe,
    /// 阶段 3:剥虚拟根前缀(YSTB 条目名的单字节前缀逐包不同)。
    RootPrefixStrip,
    /// 阶段 4:剥 `_单数字` 变体(成果 74;**Likely**)。
    VariantStrip,
    /// 阶段 5:同目录前缀模糊(成果 122 `type1/` 皮肤;**Likely**)。
    PrefixFuzzy,
    /// 阶段 6:立绘档位确定性选择(layout-fix-plan §2.1;**Likely**)。
    TachieTier,
}

impl ResolveStage {
    /// 该阶段是否已有二进制证据(供诊断区分"契约命中"与"启发式命中")。
    pub fn is_contract(&self) -> bool {
        matches!(
            self,
            Self::Exact | Self::ExtensionProbe | Self::RootPrefixStrip
        )
    }
}

/// 归一化后的查找键。
///
/// 内部是**CP932 字节**(ASCII 已折叠为小写)—— 与包内条目名同一空间,
/// 因此可以直接做字节级精确比较,不需要任何"再编码"试探。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LookupKey(Box<[u8]>);

impl LookupKey {
    /// 原始字节(诊断用)。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 有损解码(日志/展示用;非 UTF-8 时按 CP932 解)。
    ///
    /// ⚠ **不要**把它当作"可回灌的路径"再解析一遍 —— CP932→UTF-8 是
    /// 有损的,回灌会丢原始字节。需要按键读取时用
    /// [`VfsIndex::entry_of_key`]。
    pub fn display(&self) -> String {
        let (s, _, _) = encoding_rs::SHIFT_JIS.decode(&self.0);
        s.into_owned()
    }

    /// **剥虚拟根前缀**(1 字节;`$ysbin\ysl.ybn` → `ysbin\ysl.ybn`)。
    ///
    /// 证据形态:部分作品的 YPF 条目名带**单字节虚拟根**(`$` / `%` / `9` …),
    /// 而脚本侧引用**不带** —— 旧实现在索引里对每条同时插入 `name` 与
    /// `name[1..]` 两个键,查询天然覆盖两种形态。本方法把那条规则
    /// **收敛到一处**(原来散在 3 个扫描点里各写一遍)。
    ///
    /// **为何是 1 字节而不是"首段"**:本作(<game-b>,12211 条)实测
    /// 条目名**全部无根**;根前缀只在别的发行版出现,且仓库内既有证据
    /// (`is_script_entry` 按后缀判定、旧索引插入 `[1..]`)都指向单字节。
    /// 多字节根(长度码形态)属 **Hypothesis**,不在此实现(登记 G14)。
    ///
    /// 别名键**不覆盖**精确键:查询先走精确(阶段 1),
    /// 无根形态只在精确未命中时兜底(阶段 3)—— 因此对无根包是零影响。
    pub fn strip_root(&self) -> LookupKey {
        if self.0.is_empty() {
            return self.clone();
        }
        LookupKey(self.0[1..].to_vec().into_boxed_slice())
    }
}

/// 一条已解析的条目位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryRef {
    /// 数据源序号(挂载顺序 = 覆盖优先级,小的先)。
    pub source: u32,
    /// 该源内条目下标。
    pub index: u32,
    /// 命中阶段。
    pub stage: ResolveStage,
}

// ---------- 归一化:契约 1/2/3 的唯一实现处 ----------

/// SJIS 前导字节判定(唯一实现 = [`crate::util::is_sjis_lead`];
/// 表取证见该函数文档:v494 `0x5b40a0` = `0x81-0x9F` | `0xE0-0xFC`)。
///
/// 用途:大小写折叠必须**跳过双字节字的后继字节** —— 否则会把
/// 全角 `ａ`(0x82 0x61)折成 `Ａ`(0x82 0x41),让两个不同字符撞键。
use crate::util::is_sjis_lead;

/// 大小写折叠(仅 ASCII,**跳过 SJIS 双字节字**;见 [`is_sjis_lead`])。
///
/// 引擎侧的 `FUN_00468d40`(208B)是逐字节折叠比较 —— 那是**引擎查文件
/// 时大小写不敏感**的二进制证据。本实现取"只折 ASCII"的等价行为:
/// 对 ASCII 完全一致,且不会破坏双字节字。
fn fold_ascii_case(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if is_sjis_lead(c) && i + 1 < b.len() {
            out.push(c);
            out.push(b[i + 1]); // 后继字节原样(折叠会破坏双字节字)
            i += 2;
        } else {
            out.push(c.to_ascii_lowercase());
            i += 1;
        }
    }
    out
}

/// 分隔符归一:`/` → `\`(契约 3:引擎两种写法等价)。
fn unify_sep(b: &[u8]) -> Vec<u8> {
    b.iter().map(|&c| if c == b'/' { b'\\' } else { c }).collect()
}

/// 字节序列是否已是可原样使用的 CP932(全 ASCII,或合法的双字节结构)。
///
/// 用途:避免把**本来就是 CP932 的字节**当 UTF-8 再编码一次 ——
/// 那会得到 `?`,把正确名字毁掉。
fn is_valid_cp932_lossless(b: &[u8]) -> bool {
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            i += 1;
        } else if is_sjis_lead(c) && i + 1 < b.len() && b[i + 1] != 0x7F {
            i += 2;
        } else {
            // 半角片假名(0xA1..=0xDF)等单字节 CP932:往返恒等,判 false
            // 走"再编码"分支结果相同,不影响正确性。
            return false;
        }
    }
    true
}

/// **把脚本侧请求(UTF-8 `&str`)归一为查找键**(契约 1 的翻译点)。
///
/// 顺序:分隔符 → **CP932 编码** → 大小写折叠。
///
/// 关键在第二步:脚本内部字符串是 UTF-8,而名字空间是 CP932 ——
/// 这一步就是旧实现缺的"翻译",它的缺失让全部非 ASCII 名字不可达
/// (实测 `cg\ev\サークル.jpg`;全包 2304/12211 条)。
pub fn normalize_request(s: &str) -> LookupKey {
    let t = s.trim();
    let sepd = unify_sep(t.as_bytes());
    let cp932 = if is_valid_cp932_lossless(&sepd) {
        sepd
    } else {
        let (c, _, _) = encoding_rs::SHIFT_JIS.encode(t);
        unify_sep(c.as_ref())
    };
    LookupKey(fold_ascii_case(&cp932).into_boxed_slice())
}

/// **把请求(可能是 CP932 原始字节,也可能是 UTF-8 文本)归一为查找键**。
///
/// 与 [`normalize_request`] 的差别:入参是**字节**,因此不做任何字符串解码
/// —— 直接按「已是 CP932 ⇒ 原样用;否则当 UTF-8 文本编码」判定。
/// 供 `CG[FILE=…]` 这类**字节串直传**路径使用(成果 139:调用方若先
/// `from_utf8_lossy` 再传 `&str`,CP932 名会在那一步就被销毁)。
pub fn normalize_request_bytes(raw: &[u8]) -> LookupKey {
    let sepd = unify_sep(raw);
    if is_valid_cp932_lossless(&sepd) {
        return LookupKey(fold_ascii_case(&sepd).into_boxed_slice());
    }
    // 不是合法 CP932 结构 ⇒ 当作 UTF-8 文本再编码(与字符串路径同行为)
    let t = String::from_utf8_lossy(&sepd).into_owned();
    normalize_request(&t)
}

/// **把包内条目名(已是 CP932 字节)归一为查找键**。
///
/// 只做分隔符 + 大小写(不再编码 —— 它本来就在名字空间里)。
pub fn normalize_entry(name: &[u8]) -> LookupKey {
    LookupKey(fold_ascii_case(&unify_sep(name)).into_boxed_slice())
}

/// 扩展名探测序(引擎契约 5,官方手册 `cg/cg.html` 原文:
/// 「拡張子を探しに行く順番は『.PNG』『.BMP』『.JPG』『.GIF』の順」)。
pub const EXT_PROBE: [&str; 4] = [".png", ".bmp", ".jpg", ".gif"];

/// **VFS 索引**:归一化键 → 条目。
///
/// 单一职责:持有"键 → 位置"映射并提供解析管线。数据源(包/松散目录)
/// 的读取由调用方负责 —— 本模块只管**名字解析**,不管字节搬运。
#[derive(Debug, Default)]
pub struct VfsIndex {
    index: HashMap<LookupKey, (u32, u32)>,
    /// **剥虚拟根前缀后的键** → 条目(阶段 3 的查询面)。
    ///
    /// 为什么单列一张表:虚拟根前缀长在**条目名**上(`$ysbin\x`),
    /// 而脚本侧的请求**不带**它 —— 所以"剥"必须发生在条目侧,
    /// 不能拿请求去剥(那只会剥掉请求自己的首段)。
    rootless: HashMap<LookupKey, (u32, u32)>,
    /// 供阶段 4/5 的模糊扫描(按插入顺序;确定性,不依赖 HashMap 遍历序)。
    entries: Vec<(LookupKey, u32, u32)>,
}

impl VfsIndex {
    /// 插入一条条目(先插入者优先 —— 调用方按覆盖优先级从高到低插入)。
    pub fn insert(&mut self, name: &[u8], source: u32, index: u32) {
        let key = normalize_entry(name);
        let rl = key.strip_root();
        if rl.as_bytes() != key.as_bytes() {
            self.rootless.entry(rl.clone()).or_insert((source, index));
        }
        if self.index.contains_key(&key) {
            return; // 已存在(同键同条目的别名键):保留先插入者
        }
        self.index.insert(key.clone(), (source, index));
        self.entries.push((key, source, index));
    }

    /// 条目总数。
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// 全部归一化键(诊断/测试)。
    pub fn keys(&self) -> impl Iterator<Item = &LookupKey> {
        self.entries.iter().map(|(k, _, _)| k)
    }

    /// **按原始字节解析**(`CG[FILE=…]` 的字节串直传路径;成果 139)。
    ///
    /// 管线与 [`VfsIndex::resolve`] 完全相同,唯一差别是归一化走
    /// [`normalize_request_bytes`] —— 不做字符串往返,CP932 名零损耗。
    pub fn resolve_bytes(&self, raw: &[u8]) -> Option<EntryRef> {
        let key = normalize_request_bytes(raw);
        let hit = |st: ResolveStage, r: (u32, u32)| {
            Some(EntryRef { source: r.0, index: r.1, stage: st })
        };
        if let Some(&r) = self.index.get(&key) {
            return hit(ResolveStage::Exact, r);
        }
        let kb = key.as_bytes();
        let has_ext = [".png", ".bmp", ".jpg", ".gif"]
            .iter()
            .any(|e| kb.ends_with(e.as_bytes()));
        if !has_ext {
            for ext in EXT_PROBE {
                let mut w = kb.to_vec();
                w.extend_from_slice(ext.as_bytes());
                if let Some(&r) = self.index.get(&LookupKey(w.into_boxed_slice())) {
                    return hit(ResolveStage::ExtensionProbe, r);
                }
            }
        }
        if let Some(&r) = self.rootless.get(&key) {
            return hit(ResolveStage::RootPrefixStrip, r);
        }
        for cand in variant_strips(kb) {
            let ck = LookupKey(cand.clone().into_boxed_slice());
            if let Some(&r) = self.index.get(&ck) {
                return hit(ResolveStage::VariantStrip, r);
            }
            if let Some(&r) = self.rootless.get(&ck) {
                return hit(ResolveStage::VariantStrip, r);
            }
            for ext in EXT_PROBE {
                let mut w = cand.clone();
                w.extend_from_slice(ext.as_bytes());
                if let Some(r) = self.suffix_match(&w) {
                    return hit(ResolveStage::VariantStrip, r);
                }
            }
        }
        for ext in std::iter::once("").chain(EXT_PROBE) {
            let mut w = kb.to_vec();
            w.extend_from_slice(ext.as_bytes());
            if let Some(r) = self.suffix_match(&w) {
                return hit(ResolveStage::PrefixFuzzy, r);
            }
        }
        None
    }

    /// **按键直查**(键已在名字空间内 ⇒ 精确比较,无编码往返)。
    ///
    /// 供"已知键、要读内容"的调用方使用(音频清单、缓存预热等):
    /// 旧实现走 `display()` 回灌,对 CP932 名是有损的。
    pub fn entry_of_key(&self, key: &LookupKey) -> Option<EntryRef> {
        self.index.get(key).map(|&(source, index)| EntryRef {
            source,
            index,
            stage: ResolveStage::Exact,
        })
    }

    /// **解析管线**:请求名 → 条目(按阶段优先级短路返回)。
    ///
    /// 与旧实现的差别:**所有阶段共用同一个归一化键空间**,因此
    /// 编码/大小写/分隔符三条全局契约在**任何阶段**都成立 ——
    /// 旧实现里"阶段 1 正确、阶段 4 忘记转码"这类错位不可能再出现。
    pub fn resolve(&self, request: &str) -> Option<EntryRef> {
        let key = normalize_request(request);
        let trace = std::env::var_os("YURIS_TRACE_VFS").is_some();
        let hit = |st: ResolveStage, r: (u32, u32)| {
            if trace && yuris_log::enabled(yuris_log::Level::Trace) {
                yuris_log::trace_forced!(
                    "[vfs] {:?} ← {:?} (源{} 条目{})",
                    request,
                    key.display(),
                    r.0,
                    r.1
                );
            }
            Some(EntryRef { source: r.0, index: r.1, stage: st })
        };

        // ---- 阶段 1:精确(含显式扩展名)----
        if let Some(&r) = self.index.get(&key) {
            return hit(ResolveStage::Exact, r);
        }

        // ---- 阶段 2:扩展名探测(手册序)----
        let kb = key.as_bytes();
        let has_ext = [".png", ".bmp", ".jpg", ".gif"]
            .iter()
            .any(|e| kb.ends_with(e.as_bytes()));
        if !has_ext {
            for ext in EXT_PROBE {
                let cand = normalize_request(&format!("{request}{ext}"));
                if let Some(&r) = self.index.get(&cand) {
                    return hit(ResolveStage::ExtensionProbe, r);
                }
            }
        }

        // ---- 阶段 3:剥虚拟根前缀(条目侧)----
        // 根前缀逐包不同(`$` / `%` / `9` / 长度码),故不写死具体字节 ——
        // 索引侧已按**首段**剥好(见 [`VfsIndex::insert`]),这里直接用请求查。
        if let Some(&r) = self.rootless.get(&key) {
            return hit(ResolveStage::RootPrefixStrip, r);
        }

        // ---- 阶段 4:剥 `_单数字` 变体(Likely;成果 74)----
        //
        // 证据形态(语料实证 6+ 例、0 反例):请求 `config/sound_2/tip_cgauge`
        // 而包内实存 `cgsys\config\sound\tip_cgauge.png` —— 剥 `_2` 后
        // **仍差一个前缀段**(`cgsys\`)且要补扩展名。因此本阶段 = 变体候选 + 扩展名
        // 探测 + **整段后缀匹配**(不是子串:以 `\` 起段,避免 `x_y` 命中 `ax_y`)。
        for cand in variant_strips(kb) {
            let ck = LookupKey(cand.clone().into_boxed_slice());
            if let Some(&r) = self.index.get(&ck) {
                return hit(ResolveStage::VariantStrip, r);
            }
            if let Some(&r) = self.rootless.get(&ck) {
                return hit(ResolveStage::VariantStrip, r);
            }
            for ext in EXT_PROBE {
                let mut w = cand.clone();
                w.extend_from_slice(ext.as_bytes());
                if let Some(&(src, ei)) = self.index.get(&LookupKey(w.clone().into_boxed_slice()))
                {
                    return hit(ResolveStage::VariantStrip, (src, ei));
                }
                if let Some(r) = self.suffix_match(&w) {
                    return hit(ResolveStage::VariantStrip, r);
                }
            }
        }

        // ---- 阶段 5:整段后缀匹配(Likely;成果 122 `type1/` 皮肤变体)----
        //
        // 实证:`main/button/btn_skip_bt4` → `…\type1\btn_skip_bt4n.png`
        // (type1/ 是皮肤子目录)。按**完整段后缀**匹配,结果唯一才采纳 ——
        // 多条候选时**不猜**(返回 None,由调用方换名字)。
        for ext in std::iter::once("").chain(EXT_PROBE) {
            let mut w = kb.to_vec();
            w.extend_from_slice(ext.as_bytes());
            if let Some(r) = self.suffix_match(&w) {
                return hit(ResolveStage::PrefixFuzzy, r);
            }
        }

        None
    }

    /// **整段后缀匹配**:找到唯一以 `\<want>` 结尾的条目键。
    ///
    /// 段边界用 `\` 而非任意位置 —— 保证 `btn_skip` 不会命中
    /// `xbtn_skip`。多条候选 ⇒ `None`(**不猜**:猜测会引入"有时对"的
    /// 随机行为,比显式失败更难查)。
    fn suffix_match(&self, want: &[u8]) -> Option<(u32, u32)> {
        let mut found: Option<(u32, u32)> = None;
        for (k, src, ei) in &self.entries {
            let e = k.as_bytes();
            if e.len() < want.len() + 1 {
                continue;
            }
            let tail = &e[e.len() - want.len()..];
            if tail != want || e[e.len() - want.len() - 1] != b'\\' {
                continue;
            }
            if found.is_some() {
                return None; // 歧义 → 不猜
            }
            found = Some((*src, *ei));
        }
        found
    }

    /// 立绘档位候选(阶段 6 的输入):返回**归一化键**。
    ///
    /// 单独成方法是因为它需要"按档位优先级挑",而不是"首个命中"。
    pub fn tachie_candidates(&self, want: &str) -> Option<LookupKey> {
        let want_key = normalize_request(want);
        let want_b = want_key.as_bytes();
        let mut best: Option<(usize, LookupKey)> = None;
        for (k, _, _) in &self.entries {
            let e = k.as_bytes();
            let Some(rest) = e.strip_prefix(b"cg\\stand\\") else { continue };
            let fname = match rest.iter().rposition(|&c| c == b'\\') {
                Some(p) => &rest[p + 1..],
                None => rest,
            };
            let noext = fname.strip_suffix(b".png").unwrap_or(fname);
            if noext != want_b {
                continue;
            }
            let Some(rank) = TACHIE_TIER_PREF.iter().position(|t| {
                let tp = normalize_request(&format!("{t}\\")); // 档位名纯 ASCII
                rest.starts_with(tp.as_bytes())
            }) else {
                continue;
            };
            if best.as_ref().is_none_or(|(br, _)| rank < *br) {
                best = Some((rank, k.clone()));
            }
        }
        best.map(|(_, k)| k)
    }
}

/// 立绘档位优先级(剧本桥 `yst00062` 只为 `m_050`/`m_060` 预载缓存,
/// 参考图标定主档 = `m_050` 原始像素;docs/layout-fix-plan.md §2.1)。
pub const TACHIE_TIER_PREF: [&str; 5] = ["m_050", "m_040", "m_060", "m_030", "m_066"];

/// 剥 `_单数字` 变体(旧 `variant_strips` 的等价重写;Likely,成果 74)。
///
/// 语料实证:系统剧本以字面量请求 `config/sound_2/tip_cgauge`、
/// `config/back_sound_2` 等,而包内实存 `config\sound\tip_cgauge.png`、
/// `config\back_sound_.png`(剥 `_2` 形态 6+ 例全中、0 反例)。
/// 双位数(`_08`)不剥(防 `btn_page08_over` 误伤)。
fn variant_strips(kb: &[u8]) -> Vec<Vec<u8>> {
    let mut cuts: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i + 1 < kb.len() {
        if kb[i] == b'_'
            && kb[i + 1].is_ascii_digit()
            && (i + 2 >= kb.len() || !kb[i + 2].is_ascii_digit())
        {
            cuts.push(i);
            i += 2;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(cuts.len());
    let mut cur = kb.to_vec();
    for &c in cuts.iter().rev() {
        cur.remove(c);
        cur.remove(c); // 数字位(剥 '_' 后左移)
        out.push(cur.clone());
    }
    out
}

/// 一条松散文件记录(目录源;契约 6:松散优先于封包)。
#[derive(Debug, Clone)]
pub struct LooseRoot {
    /// 目录绝对路径。
    pub root: PathBuf,
}

impl LooseRoot {
    /// 判定目录下是否有该请求对应的文件(大小写不敏感,契约 2)。
    pub fn find(&self, request: &str) -> Option<PathBuf> {
        let key = normalize_request(request);
        let rel = key.display().replace('\\', std::path::MAIN_SEPARATOR_STR);
        let p = self.root.join(&rel);
        if p.is_file() {
            return Some(p);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_are_equivalent() {
        assert_eq!(
            normalize_request("cg/ev/x").as_bytes(),
            normalize_request(r"cg\ev\x").as_bytes()
        );
    }

    #[test]
    fn case_is_folded() {
        assert_eq!(
            normalize_request("CG/EV/X").as_bytes(),
            normalize_request("cg/ev/x").as_bytes()
        );
    }

    /// **回归(成果 136 的核心)**:日文名必须与包内 CP932 键归一。
    #[test]
    fn cp932_name_matches_package_key() {
        // 包内条目名(SJIS 字节)
        let entry = b"cg\\ev\\\x83\x54\x81\x5b\x83\x4e\x83\x8b.jpg";
        let q = normalize_request("cg/ev/サークル.jpg");
        assert_eq!(
            q.as_bytes(),
            normalize_entry(entry).as_bytes(),
            "CP932 请求与包内条目名必须归一到同一键"
        );
    }

    /// 大小写折叠**不得**破坏 SJIS 双字节字(全角 a vs 全角 A 不能撞键)。
    #[test]
    fn fold_does_not_corrupt_sjis_double_byte() {
        let upper = b"\x82\x41"; // Ａ(全角 A)
        let lower = b"\x82\x61"; // ａ(全角 a)
        assert_ne!(
            normalize_entry(upper).as_bytes(),
            normalize_entry(lower).as_bytes(),
            "双字节字被折叠 ⇒ 两个不同字符撞键"
        );
    }

    #[test]
    fn extension_probe_follows_manual_order() {
        let mut v = VfsIndex::default();
        v.insert(b"cg\\bg\\x.jpg", 0, 0);
        let r = v.resolve("cg/bg/x").expect("补扩展名应命中");
        assert_eq!(r.stage, ResolveStage::ExtensionProbe);
        assert!(r.stage.is_contract());
    }

    #[test]
    fn exact_wins_over_probe() {
        let mut v = VfsIndex::default();
        v.insert(b"cg\\bg\\x.png", 0, 0);
        v.insert(b"cg\\bg\\x", 0, 1);
        assert_eq!(v.resolve("cg/bg/x").unwrap().stage, ResolveStage::Exact);
    }

    #[test]
    fn root_prefix_is_stripped() {
        let mut v = VfsIndex::default();
        v.insert(b"$ysbin\\ysl.ybn", 0, 7);
        let r = v.resolve("ysbin/ysl.ybn").expect("剥根前缀应命中");
        assert_eq!(r.stage, ResolveStage::RootPrefixStrip);
        assert_eq!(r.index, 7);
        // 带根形态仍走精确命中(别名不得抢先)
        let r2 = v.resolve("$ysbin\\ysl.ybn").unwrap();
        assert_eq!(r2.stage, ResolveStage::Exact);
    }

    /// 无根包(本作形态)不得因别名键产生误命中。
    #[test]
    fn rootless_pack_is_unaffected() {
        let mut v = VfsIndex::default();
        v.insert(b"cg\\ev\\x.png", 0, 0);
        // `cg\ev\x` 的 1 字节剥形态是 `g\ev\x`,
        // 与任何请求都不该撞 —— 精确命中照常。
        assert_eq!(v.resolve("cg/ev/x").unwrap().stage, ResolveStage::ExtensionProbe);
    }

    #[test]
    fn variant_strip_matches_likely_evidence() {
        let mut v = VfsIndex::default();
        v.insert(b"cgsys\\config\\sound\\tip_cgauge.png", 0, 0);
        let r = v.resolve("config/sound_2/tip_cgauge").expect("剥 _2 应命中");
        assert_eq!(r.stage, ResolveStage::VariantStrip);
        assert!(!r.stage.is_contract(), "启发式阶段不得标为契约");
    }

    #[test]
    fn variant_strip_leaves_double_digit() {
        let mut v = VfsIndex::default();
        v.insert(b"cgsys\\saveload\\btn_page08_over.png", 0, 0);
        // `_08` 是双位数 ⇒ 不剥,故 `_8` 形态不应命中
        assert!(v.resolve("saveload/btn_page_8_over").is_none());
    }

    #[test]
    fn missing_is_none() {
        let v = VfsIndex::default();
        assert!(v.resolve("cg/ev/__NOPE__").is_none());
    }
}
