//! 跨脚本上下文:多脚本执行宿主(P0)。
//!
//! 单脚本 [`GroupVm`](crate::GroupVm) 是引擎执行单元的核心;但真实游戏流程
//! 在 `yst%05d.ybn` 之间不断跳转(GOSUB 占语料 27065 组,最高频)。
//! 本模块提供「按脚本号取脚本上下文」的接口与实现,供 GroupVm 跨脚本切换。
//!
//! [`YpfScriptHost`] 从 REAL YPF 封包按 script_id 读 `%ysbin\yst%05d.ybn`。

use std::collections::HashMap;

use yuris_core::{Error, Result};
use yuris_format::ystb::{CommandGroup, YstbFile};

/// 一个已解析脚本的可执行上下文(组表 + 各组窗口起始下标)。
#[derive(Debug, Clone)]
pub struct ScriptCtx {
    /// 脚本号(yst%05d)。
    pub script_id: u16,
    /// 已解析的 YSTB。
    pub script: YstbFile,
    /// 命令组表(part1 区)。
    pub groups: Vec<CommandGroup>,
    /// 每组第一个窗口在 slots 区的下标(引擎 obj+0x10 记录指针数组的等价)。
    pub first_slots: Vec<usize>,
}

impl ScriptCtx {
    /// 从脚本字节解析出上下文。组模型校验失败(非 v555 形态)→ `Err`。
    pub fn parse(script_id: u16, script: YstbFile) -> Result<Self> {
        let groups = script.groups()?;
        let first_slots = script.group_first_slots(&groups);
        Ok(Self {
            script_id,
            script,
            groups,
            first_slots,
        })
    }
}

/// 脚本加载抽象:按脚本号取 [`ScriptCtx`]。
///
/// 实现方负责从 YPF 封包读取并解析(如 `YpfScriptHost`),可缓存。
pub trait ScriptHost {
    /// 取脚本号 `id` 的上下文(惰性加载 + 缓存由实现方决定)。
    fn load(&mut self, id: u16) -> Result<ScriptCtx>;

    /// 全部可用脚本号(启动链全量声明消费用;默认空)。
    fn script_ids(&mut self) -> Result<Vec<u16>> {
        Ok(Vec::new())
    }
}

/// 基于内存脚本表(测试/少量脚本用)的宿主。
///
/// 由外部预注入 `id → YstbFile`,按需 parse + 缓存。
#[derive(Debug, Clone, Default)]
pub struct InMemoryHost {
    scripts: HashMap<u16, YstbFile>,
}

impl InMemoryHost {
    /// 空宿主。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入一个脚本。
    pub fn insert(&mut self, id: u16, script: YstbFile) -> &mut Self {
        self.scripts.insert(id, script);
        self
    }
}

impl ScriptHost for InMemoryHost {
    fn load(&mut self, id: u16) -> Result<ScriptCtx> {
        let script = self
            .scripts
            .get(&id)
            .ok_or_else(|| Error::format(format!("script {id} not injected")))?
            .clone();
        ScriptCtx::parse(id, script)
    }
}


/// 基于 YPF 封包的宿主(真实游戏用):**拥有** [`YpfArchive`],
/// 按 script_id 拼 `$ysbin\yst%05d.ybn` 从封包读取并解析,结果缓存(惰性加载)。
///
/// 拥有所有权使其为 `'static`,可放进 [`Box<dyn ScriptHost>`](crate::ScriptHost)。
pub struct YpfScriptHost {
    /// 封包来源:**内存**(小包/测试)或**文件**(大包,随机访问,不整载)。
    src: PackSource,
    /// 脚本区 XOR 密钥(4 字节循环;逐游戏不同,由 `guess_key` 得出)。
    key: [u8; 4],
    /// 缓存(script_id → 已解析上下文)。
    cache: HashMap<u16, ScriptCtx>,
}

/// 封包来源。
///
/// 大包(实测有 3.3 GB 的发行数据)必须走文件型随机访问 —— 整载既慢又吃内存。
pub enum PackSource {
    /// 已整载到内存的封包。
    Mem(yuris_format::ypf::YpfArchive),
    /// 文件 + 已解析索引(按需 seek 读)。
    File {
        /// 读取器。
        reader: yuris_format::ypf::YpfReader,
        /// 索引方言(诊断用)。
        dialect: yuris_format::ypf::IndexDialect,
    },
}

impl YpfScriptHost {
    /// 从封包字节构建宿主(装载 ypf 全量到内存并解析索引)。
    pub fn from_ypf_bytes(bytes: Vec<u8>, name_key: u8, key: [u8; 4]) -> Result<Self> {
        let ypf = yuris_format::ypf::YpfArchive::from_bytes(bytes, name_key)?;
        Ok(Self {
            src: PackSource::Mem(ypf),
            key,
            cache: HashMap::new(),
        })
    }

    /// **通用**构造:按路径打开封包,自动判别索引方言,并自动猜测脚本密钥。
    ///
    /// 不再需要调用方提供 `name_key` 或 `ystb_key` —— 两者都是**逐游戏/逐包**
    /// 的事实,由格式层从数据侧判别(见 `YpfIndex::detect_dialect` 与
    /// `ystb::guess_key`)。
    pub fn open(pack: &std::path::Path) -> Result<Self> {
        use yuris_format::ypf::{YpfIndex, YpfReader};
        let (index, dialect, _score) = YpfIndex::from_path_auto(pack)?;
        let first = index
            .entries
            .iter()
            .find(|e| is_script_entry(&e.name))
            .map(|e| e.name.clone())
            .ok_or_else(|| {
                Error::format("封包内无脚本条目(ysbin 下的 yst*.ybn),无法确定 YSTB 密钥")
            })?;
        let mut reader = YpfReader::from_index(pack, index)?;
        // 脚本密钥:对**首个脚本条目**跑 guess_key(逐游戏不同;不判则无法解脚本)。
        let blob = reader.read(&first)?;
        let (key, _rate) = yuris_format::ystb::guess_key(&blob)?;
        Ok(Self {
            src: PackSource::File { reader, dialect },
            key,
            cache: HashMap::new(),
        })
    }

    /// 索引方言(诊断;仅文件型来源可知)。
    pub fn dialect(&self) -> Option<yuris_format::ypf::IndexDialect> {
        match &self.src {
            PackSource::File { dialect, .. } => Some(*dialect),
            PackSource::Mem(_) => None,
        }
    }

    /// 自动猜测出的脚本密钥。
    pub fn script_key(&self) -> [u8; 4] {
        self.key
    }

    /// 读封包内任一条目(启动链读 YSLB/YSVR 等非脚本数据用)。
    pub fn read_entry(&mut self, path: &str) -> Result<Vec<u8>> {
        match &mut self.src {
            PackSource::Mem(a) => a.read(path),
            PackSource::File { reader, .. } => reader.read(path.as_bytes()),
        }
    }

    /// 封包内全部条目名(lossy UTF-8;SJIS 名会带替换字符,仅用于**后缀匹配**)。
    ///
    /// 条目名的**虚拟根前缀**(`$` / `%` / `9` …)逐包不同,故调用方应按后缀
    /// 查找(如 `ysbin\ysc.ybn`),不要写死全名。
    pub fn entry_names(&self) -> Vec<String> {
        match &self.src {
            PackSource::Mem(a) => a.entries().iter().map(|e| e.name.clone()).collect(),
            PackSource::File { reader, .. } => reader
                .index()
                .entries
                .iter()
                .map(|e| String::from_utf8_lossy(&e.name).to_string())
                .collect(),
        }
    }

    /// 脚本条目名(任意根前缀下的 `ysbin` 目录 + `yst*.ybn`)。
    pub fn script_entries(&self) -> Vec<String> {
        self.entry_names()
            .into_iter()
            .filter(|n| is_script_entry(n.as_bytes()))
            .collect()
    }
}

impl ScriptHost for YpfScriptHost {
    fn load(&mut self, id: u16) -> Result<ScriptCtx> {
        if let Some(ctx) = self.cache.get(&id) {
            return Ok(ctx.clone());
        }
        let path = self.script_path(id)?;
        let bytes = self.read_entry(&path)?;
        let script = yuris_format::ystb::YstbFile::from_bytes(&bytes, self.key)?;
        let ctx = ScriptCtx::parse(id, script)?;
        self.cache.insert(id, ctx.clone());
        Ok(ctx)
    }

    fn script_ids(&mut self) -> Result<Vec<u16>> {
        let mut ids: Vec<u16> = self
            .entry_names()
            .iter()
            .filter_map(|n| script_id_of(n.as_bytes()))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }
}

/// 是否为脚本条目:任意**根前缀**下的 `ysbin` 目录 + `yst*.ybn`。
///
/// 根前缀逐包不同(`$` / `%` / `9` / 长度码…),故按目录段 + 文件名判定。
pub fn is_script_entry(name: &[u8]) -> bool {
    script_id_of(name).is_some()
}

/// 由条目名取脚本号:任意**根前缀**下的 `…ysbin\ystNNNNN.ybn`。
///
/// 根前缀(`$` / `%` / `9` …)逐包不同,故按**后缀**判定而非 `strip_prefix("$")`。
fn script_id_of(name: &[u8]) -> Option<u16> {
    let s = String::from_utf8_lossy(name).replace('/', "\\");
    let (dir, file) = s.rsplit_once('\\')?;
    if !dir.ends_with("ysbin") {
        return None;
    }
    let digits = file.strip_suffix(".ybn")?.strip_prefix("yst")?;
    if digits.len() != 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u16>().ok()
}

impl YpfScriptHost {
    /// 按脚本号解析条目全名(用包内实际根前缀,不写死 `$`)。
    fn script_path(&self, id: u16) -> Result<String> {
        let want = format!("ysbin\\yst{id:05}.ybn");
        self.entry_names()
            .into_iter()
            .find(|n| n.replace('/', "\\").ends_with(&want))
            .ok_or_else(|| Error::format(format!("脚本条目不存在: yst{id:05}.ybn")))
    }
}

/// 游戏**虚拟文件系统**存在性索引(P5.2,引擎 FILEINFO EXIST 语义)。
///
/// 引擎的变量 FS = 游戏目录下的 `pac\*.ypf` 封包 + 松散文件;脚本侧
/// FILEINFO(0x15) 的 EXIST 查询即在此 FS 上判定(实证:s207 es.R18Check
/// 查 `cg/thumb_cg/A_HAN_2002_a.png`,引擎返回 1)。
///
/// 只解析各封包**索引区**(`YpfIndex`,数据区不触碰),松散文件按查询时
/// `Path::exists` 判定。
#[derive(Debug, Default)]
pub struct PacFileIndex {
    names: std::collections::HashSet<Vec<u8>>,
    roots: Vec<std::path::PathBuf>,
    /// 解析失败被跳过的封包(如头加密的 op.ypf —— 密钥派生属 P6 资源层)。
    pub skipped: Vec<String>,
    /// P1 收尾:保留各封包的解析索引(条目偏移/类型码),供
    /// [`Self::image_dims`](PacFileIndex::image_dims) 按需读图像头
    /// (CGINFO 槽13/14 = 图像真实宽/高,引擎 watch oracle 实证)。
    packs: Vec<(std::path::PathBuf, yuris_format::ypf::YpfIndex)>,
    /// **图像尺寸缓存**(成果 168):`CG` 每条带 FILE 的命令都会问尺寸,
    /// 旧实现每次都「打开封包 → 读整段压缩数据 → **整块 zlib 解压** →
    /// 只读宽高」。boot 期数千条 CG ⇒ 单帧数百次全量解压(实测 VM 段
    /// 12–19ms 的主要构成)。键 = FILE 原始字节(CP932 保真),值 =
    /// 命中结果(**含 `None`**,避免对缺失资源重复解析)。
    dims_cache: std::sync::Mutex<HashMap<Vec<u8>, Option<(i64, i64)>>>,
    /// **VFS 索引**(成果 136):归一化键 → (pack, entry)。
    ///
    /// 引擎契约(CP932 名空间 / 大小写不敏感 / 分隔符等价 / 扩展名探测序 /
    /// 剥虚拟根前缀)全部在 [`crate::vfs`] 一处实现 —— 旧实现是
    /// `HashMap<Vec<u8>,…>` + 逐处手写的候选生成,实测漏掉了前两条契约
    /// (见 `vfs` 模块文档)。
    ///
    /// 旧实现另插入"剥 1 字节"的别名键(`e.name[1..]`);现在由
    /// [`crate::vfs::ResolveStage::RootPrefixStrip`] 阶段按**首段**通用处理,
    /// 因此索引不再需要别名键,内存与语义都更干净。
    vfs: crate::vfs::VfsIndex,
}

impl PacFileIndex {
    /// 扫描游戏目录:`<game>\pac\*.ypf` 与 `<game>\*.ypf` 的索引并入;
    /// `<game>`、`<game>\pac`、`<game>\save`(存档,引擎 VFS 原生前缀之一,
    /// s25/s35 的 *.sd 存在性查询在此命中 —— 成果 59e)作为松散文件查找根。
    /// 传 `pac` 目录本身亦可(自动上溯一级作为游戏根)。
    pub fn scan_game_dir(dir: &std::path::Path, name_key: u8) -> Result<Self> {
        let (game_root, pac_dir) = if dir.join("pac").is_dir() {
            (dir.to_path_buf(), dir.join("pac"))
        } else {
            (
                dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.to_path_buf()),
                dir.to_path_buf(),
            )
        };
        let mut idx = Self::default();
        for root in [game_root.clone(), pac_dir.clone()] {
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("ypf") {
                        match yuris_format::ypf::YpfIndex::from_path(&p, name_key) {
                            Ok(y) => {
                                let pi = idx.packs.len();
                                for (ei, e) in y.entries.iter().enumerate() {
                                    idx.vfs.insert(&e.name, pi as u32, ei as u32);
                                }
                                idx.packs.push((p.clone(), y));
                                // names 并入(保留原行为:纯存在性判定)
                                for e in &idx.packs[pi].1.entries {
                                    idx.names.insert(e.name.clone());
                                }
                            }
                            Err(e) => {
                                // 头加密等不可解析包:跳过(不阻断存在性检查),
                                // 但必须留痕 —— 静默跳包会让整包资源「无声消失」
                                // (NEKO-NIN exHeart se.ypf 实证)。
                                yuris_log::warn!(
                                    "[pac] 跳过不可解析包 {}: {e}",
                                    p.file_name()
                                        .map(|s| s.to_string_lossy())
                                        .unwrap_or_default()
                                );
                                idx.skipped.push(
                                    p.file_name()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                );
                            }
                        }
                    }
                }
            }
        }
        idx.roots = vec![game_root.clone(), pac_dir, game_root.join("save")];
        Ok(idx)
    }

    /// **通用**扫描:逐包**自动判别索引方言**,不需要调用方提供 `name_key`。
    ///
    /// 与 [`Self::scan_game_dir`] 的差别仅在"每包用哪个 key"—— 后者要求调用方
    /// 提供一个全局 key(样本专用),本函数让格式层按数据侧判据决定。
    pub fn scan_game_dir_auto(dir: &std::path::Path) -> Result<Self> {
        let (game_root, pac_dir) = if dir.join("pac").is_dir() {
            (dir.to_path_buf(), dir.join("pac"))
        } else {
            (
                dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| dir.to_path_buf()),
                dir.to_path_buf(),
            )
        };
        let mut idx = Self::default();
        let t_scan = std::time::Instant::now();
        for root in [game_root.clone(), pac_dir.clone()] {
            let Ok(rd) = std::fs::read_dir(&root) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("ypf") {
                    continue;
                }
                let t_parse = std::time::Instant::now();
                let parsed = yuris_format::ypf::YpfIndex::from_path_auto(&p);
                if std::env::var_os("YURIS_TRACE_BOOT").is_some() {
                    yuris_log::trace_forced!(
                        "[boot] parse {} {:.0}ms",
                        p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                        t_parse.elapsed().as_secs_f64() * 1000.0
                    );
                }
                match parsed {
                    Ok((y, dialect, score)) => {
                        let pi = idx.packs.len();
                        let t_ins = std::time::Instant::now();
                        for (ei, ent) in y.entries.iter().enumerate() {
                            idx.vfs.insert(&ent.name, pi as u32, ei as u32);
                        }
                        for ent in &y.entries {
                            idx.names.insert(ent.name.clone());
                        }
                        if std::env::var_os("YURIS_TRACE_BOOT").is_some() {
                            yuris_log::trace_forced!(
                                "[boot] insert {} entries {:.0}ms (total {:.0}ms)",
                                y.entries.len(),
                                t_ins.elapsed().as_secs_f64() * 1000.0,
                                t_scan.elapsed().as_secs_f64() * 1000.0
                            );
                        }
                        if score < 0.9 {
                            yuris_log::warn!(
                                "[pac] {} 方言 {dialect:?} 结构自洽率仅 {:.1}% —— 索引可能漂移",
                                p.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
                                score * 100.0
                            );
                        }
                        idx.packs.push((p.clone(), y));
                    }
                    Err(e) => {
                        yuris_log::warn!(
                            "[pac] 跳过不可解析包 {}: {e}",
                            p.file_name().map(|s| s.to_string_lossy()).unwrap_or_default()
                        );
                        idx.skipped.push(
                            p.file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                        );
                    }
                }
            }
        }
        idx.roots = vec![game_root.clone(), pac_dir, game_root.join("save")];
        Ok(idx)
    }

    /// 注入虚拟条目(仅存在性判定)。
    ///
    /// 用途:复现 oracle 采集时的文件系统输入 —— 引擎 trace 时刻游戏目录
    /// 存在松散的 R18 标记文件(`cg/thumb_cg/A_HAN_2002_a.png`,引擎
    /// FindFirstFileA/VFS 命中 → EXIST=1),现安装包内无此文件。重放对拍
    /// 须复现相同输入;内容从不读取,无版权物引入。
    pub fn add_virtual(&mut self, path: &str) {
        // 与 `exists` 同一键空间(归一化:CP932 + 折叠大小写 + 统一分隔符)
        self.names.insert(crate::vfs::normalize_request(path).as_bytes().to_vec());
    }

    /// 路径是否存在(封包条目 / 松散文件)。
    ///
    /// **成果 136 统一**:包内判定改走 [`crate::vfs`] 解析管线 ——
    /// 因此 CP932 名、大小写、分隔符三条契约与读取路径**完全一致**
    /// (旧实现这里另有一套 `names` 精确匹配,与读取路径两套规则,
    /// 实测存在「能查到却读不到」的错位)。
    /// `names` 保留给 [`Self::add_virtual`] 注入的重放占位名。
    pub fn exists(&self, path: &str) -> bool {
        if self.vfs.resolve(path).is_some() {
            return true;
        }
        let key = crate::vfs::normalize_request(path);
        if self.names.contains(key.as_bytes()) {
            return true;
        }
        let rel = key.display().replace('\\', "/");
        self.roots.iter().any(|r| r.join(&rel).exists())
    }

    /// 读松散文件内容(LOAD/YSSD 装载用)。
    ///
    /// 只查 `roots`(游戏根/pac/save);封包内条目不在此读(YSSD 存档
    /// 恒为 save/ 下松散文件,引擎 VFS 原生前缀,成果 59e)。分隔符归一
    /// 与 [`Self::exists`] 一致。
    pub fn read_loose(&self, path: &str) -> Option<Vec<u8>> {
        let norm = path.replace('\\', "/");
        self.roots
            .iter()
            .map(|r| r.join(&norm))
            .find_map(|p| std::fs::read(p).ok())
    }

    /// 读条目**载荷**:按数据侧判据解压(`yuris_format::ypf::looks_zlib`)。
    ///
    /// **勘误(2026-10-13)**:本文件此前四处按 `e.flag != 0xCB`(另有 `0xCF/0xCC`)
    /// 放行 —— 那是「`name_key = 0xC9` 时 PNG/OGG/WAV 类型码异或后的值」,等于把
    /// **某一款游戏的 key 硬编码进了资源层**(同 `ypf.rs` 顶部的跨版本勘误)。
    /// 实测另一发行作品(`name_key = 0xFF`,方言 `LenEncNot`):类型码是**原始值**
    /// `0x02=PNG / 0x05=WAV / 0x06=OGG / 0x03=JPG(zlib) / 0x07=PSD(zlib)`,
    /// 于是全部图像与音频条目都被判「不支持」,纹理永不装载。
    /// 现改为:要的是不是图/音,看**数据**(魔数 / zlib 头),不看方言相关字节。
    fn read_entry_payload(
        &self,
        pack_path: &std::path::Path,
        e: &yuris_format::ypf::YpfEntryInfo,
    ) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(pack_path).ok()?;
        f.seek(SeekFrom::Start(e.offset as u64)).ok()?;
        let mut raw = vec![0u8; e.compressed_len as usize];
        f.read_exact(&mut raw).ok()?;
        if yuris_format::ypf::looks_zlib(&raw, e.uncompressed_len, e.compressed_len) {
            let mut out = Vec::with_capacity(e.uncompressed_len as usize);
            flate2::read::ZlibDecoder::new(&raw[..])
                .read_to_end(&mut out)
                .ok()?;
            return Some(out);
        }
        Some(raw)
    }

    /// 读图像条目头部,返回 (宽, 高)(P1 收尾,CGINFO 槽13/14 真值源)。
    ///
    /// 引擎 watch oracle 实证:CGINFO SX/SY 应答 = CG 装载图像的真实尺寸
    /// (script9 g1089 写 @1705:occ1 = 1.0 = `tip_meswindow.png`(1×1)、
    /// occ2 = 1350.0 = `tip_meswindow_txspace.png`(1350×200);两图均经
    /// 包扫描确认存在)。
    ///
    /// **JPEG 支持(2026-10-16,成果 143)**:旧实现**只认 PNG**,JPEG 一律
    /// 返回 `None`。而本作片头 LOGO 就是 `cg/ev/サークル.jpg` —— 于是
    /// `CGINFO[… SX/SY]` 应答恒 0,官方 `es_Sprite.yst` 的
    /// `CG[ID=$LID+… SX=@sx SY=@sy]`(显示层 = **尺寸为图像大小的图层组**)
    /// 退化成 1×1 ⇒ 即使层被点亮也画不出东西。JPEG 尺寸从 SOF 段读。
    ///
    /// 路径形态勘误:脚本 FILE 参数**无扩展名**且用 `/`(引擎 VFS 自行
    /// 补扩展名;实测 `cgsys/main/button/type1/tip_meswindow`),故候选名
    /// = 原/归一路径 + {.png,.jpg,.bmp,.gif}(YSCM 图像类型族)。
    pub fn image_dims(&self, path: &str) -> Option<(i64, i64)> {
        use std::io::{Read, Seek, SeekFrom};
        let (pack_path, e) = self.resolve_entry(path, true)?;
        // zlib 条目(CG 资源典型形态)先解压;否则读原始头。
        // **勘误(2026-10-13)**:旧实现按索引类型码 `0xCB` 放行,
        // 那是「key=0xC9 时 PNG 码异或后的值」——换方言(`LenEncNot`/key=0xFF,
        // 原始类型码 0x02)后恒 None。判据改数据侧(见 `read_entry_payload`)。
        let payload: Vec<u8> = {
            let mut f = std::fs::File::open(pack_path).ok()?;
            f.seek(SeekFrom::Start(e.offset as u64)).ok()?;
            let mut h = [0u8; 24];
            f.read_exact(&mut h).ok()?;
            if yuris_format::ypf::looks_zlib(&h, e.uncompressed_len, e.compressed_len) {
                self.read_entry_payload(pack_path, e)?
            } else {
                // 未压缩:按声明长度读出(JPEG 的 SOF 可能离头较远)。
                let n = (e.uncompressed_len as usize).min(1 << 20);
                let mut buf = vec![0u8; n];
                let mut f = std::fs::File::open(pack_path).ok()?;
                f.seek(SeekFrom::Start(e.offset as u64)).ok()?;
                f.read_exact(&mut buf).ok()?;
                buf
            }
        };
        if let Some(wh) = png_dims(&payload) {
            return Some(wh);
        }
        jpeg_dims(&payload)
    }

    /// 解析 FILE 参数 → (包路径, 条目)。候选 = 原/归一路径 +
    /// {.png,.jpg,.bmp,.gif}(`image_dims` 文档),再退 basename 前缀模糊。
    /// `png_only` = 限定 stored PNG(尺寸查询用);内容读取放宽。
    ///
    /// `_N` 变体剥离回退(2026-09-08,成果 74;**Likely** —— 引擎侧解析
    /// 代码未在反编译子集内,以下为脚本↔封包对拍拟合):
    /// 系统剧本(sid 253/254 es.BT.CG.SET)以字面量请求
    /// `config/sound_2/tip_cgauge`、`config/back_sound_2`、
    /// `config/btn_tab_sound_2_on` 等形态,而 cgsys_ec.ypf 只实存
    /// `cgsys\config\sound\tip_cgauge.png`、`back_sound_.png`、
    /// `btn_tab_sound_on.png`(剥 `_2` 形态,6+ 例全中,0 反例)→ 引擎
    /// VFS 命中失败时按「从右向左逐层剥离 `_单数字` token」回退。
    /// 双位数(`_08`)不剥(防 btn_page08_over 误伤);真缺失文件
    /// (btn_all_mask、btn_c01~16_bt4、other/*、btn_35〈日文〉_bt4、
    /// tip_mes_preview_1..3 —— 全 12 包字节级检索终证不存在)剥离后
    /// 依旧未命中,与引擎同表现(按钮空图,不阻断)。
    /// **CP932 名回退(2026-10-15,成果 135;Confirmed)**
    ///
    /// 引擎的真值是**包内条目名 = CP932(SJIS)字节**。而脚本把 FILE 参数
    /// 当字符串求值,本内核内部一律 UTF-8 ⇒ 凡含非 ASCII 的路径
    /// (`cg\ev\サークル.jpg`),`base.as_bytes()` 给出的是 **UTF-8** 字节,
    /// 与索引键(CP932)永不相等 —— 实测:
    ///
    /// ```text
    /// "cg/ev/サークル"  CP932 = 63 67 5c 65 76 5c 83 54 81 5b 83 4e 83 8b
    ///                   UTF-8 = 63 67 5c 65 76 5c e3 82 b5 e3 83 bc e3 82 af e3 83 ab
    /// ```
    ///
    /// 后果不是"少几个素材":**片头 LOGO 就是 `cg\ev\サークル.jpg`**
    /// (全包少数几个日文名资源之一),`MAC.EV("サークル")` 因此取不到图 ——
    /// 玩家看到的就是"开屏动画没有"。本作 12211 条条目里 **2304 条(18.9%)**
    /// 名字含非 ASCII 字符,全都走这条路径。
    ///
    /// 修法:每个候选键**同时**产出 UTF-8 与 CP932 两种字节形态,再查索引。
    /// ASCII 路径两形态相同(开销只是一次额外的 HashMap miss),
    /// 不改变任何既有命中行为。
    fn resolve_entry(&self, path: &str, _png_only: bool) -> Option<(&std::path::PathBuf, &yuris_format::ypf::YpfEntryInfo)> {
        // 解析管线整体下沉到 `crate::vfs`(成果 136):归一化键空间
        // 让 CP932/大小写/分隔符三条契约在**每个阶段**都成立。
        let r = self.vfs.resolve(path)?;
        let (pack_path, index) = &self.packs[r.source as usize];
        let e = &index.entries[r.index as usize];
        Some((pack_path, e))
    }

    /// 读 FILE 参数指向的图像**内容**(播放器渲染用;P8 集成)。
    /// 仅 stored(se 型)条目直接 seek 读;zlib 条目返回 None
    /// (内容读取走 `yuris_resource::ResourceStack`,此处不重复解压栈)。
    pub fn read_image_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let (pack_path, e) = self.resolve_entry(path, false)?;
        let payload = self.read_entry_payload(pack_path, e)?;
        if !yuris_format::ypf::looks_like_image(&payload) {
            return None;
        }
        Some(payload)
    }

    /// **按原始字节**读图像(CP932 名安全;**成果 139**)。
    ///
    /// 存在的理由:`CG` 的 FILE 槽在 VM 侧是**字节串**(脚本给什么就是什么),
    /// 而调用方常见的写法是先 `String::from_utf8_lossy` 再传 `&str` ——
    /// 对 CP932 名(`cg/ev/サークル`)这会**销毁字节**(非 ASCII 全变 U+FFFD),
    /// 之后无论 VFS 多正确都救不回来。实测表现:
    /// `[player] CG 图像解析失败:cg/ev/�T�[�N��`。
    ///
    /// 本方法跳过字符串往返:字节直接进 [`crate::vfs`] 归一化。
    pub fn read_image_bytes_raw(&self, raw: &[u8]) -> Option<Vec<u8>> {
        let (pack_path, e) = self.resolve_entry_raw(raw, false)?;
        let payload = self.read_entry_payload(pack_path, e)?;
        if !yuris_format::ypf::looks_like_image(&payload) {
            return None;
        }
        Some(payload)
    }

    /// **纹理解析失败的原因**(诊断用;不改变任何解析行为)。
    ///
    /// 区分两类完全不同的情况,避免告警只丢一句「检查资源是否在封包内」:
    /// 1. **包内没有这个名字**(`NoEntry`):引擎/脚本请求了不存在的资源
    ///    —— 语料实测 `<game-b>` 的 `cgsys/main/nextvoice/icon_01`、
    ///    `cg/face/face_back` 属此类(它们只在"语音播放中"提示图标里用,
    ///    该作品封包未提供);
    /// 2. **有名字但载荷不是可解码图像**(`NotImage`):解压/魔数问题。
    pub fn explain_image_miss(&self, raw: &[u8]) -> ImageMiss {
        let Some((pack_path, e)) = self.resolve_entry_raw(raw, false) else {
            return ImageMiss::NoEntry;
        };
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let Some(payload) = self.read_entry_payload(pack_path, e) else {
            return ImageMiss::Unreadable(name);
        };
        if yuris_format::ypf::looks_like_image(&payload) {
            // 名字与载荷都正常 ⇒ 失败发生在解码/上传阶段(宿主侧)。
            ImageMiss::DecodeFailed(name, payload.len())
        } else {
            ImageMiss::NotImage(name, payload.len())
        }
    }

    /// **按原始字节**取图像尺寸(PNG/JPEG;CP932 名安全)。    ///
    /// 与 [`Self::image_dims`] 同义,只是不做字符串往返 —— `CG` 的 FILE 槽
    /// 是字节串,`from_utf8_lossy` 会把 CP932 名毁成 U+FFFD,于是
    /// `CGINFO SX/SY` 应答恒 0,官方脚本的
    /// `CG[ID=$LID+… SX=@sx SY=@sy]`(显示层 = 图像大小的图层组)
    /// 退化成 1×1 —— **片头 LOGO 画不出的直接原因**(成果 143)。
    pub fn image_dims_raw(&self, raw: &[u8]) -> Option<(i64, i64)> {
        // **缓存(成果 168)**:`CG` 每条带 FILE 的命令都会问一次尺寸。
        // 旧路径每问一次就「打开封包 + 读整段 + 整块 zlib 解压」——
        // boot 期单帧数百次、每次解压在百微秒~毫秒级,是 VM 段的主要成本。
        // 尺寸是**封包静态属性**,可安全永久缓存(含 None)。
        if let Ok(g) = self.dims_cache.lock() {
            if let Some(hit) = g.get(raw) {
                return *hit;
            }
        }
        let dims = self
            .resolve_entry_raw(raw, true)
            .and_then(|(pack_path, e)| self.read_entry_payload(pack_path, e))
            .and_then(|payload| png_dims(&payload).or_else(|| jpeg_dims(&payload)));
        if let Ok(mut g) = self.dims_cache.lock() {
            g.insert(raw.to_vec(), dims);
        }
        dims
    }

    /// 按原始字节解析条目(**不走 lossy 字符串**;同 [`Self::read_image_bytes_raw`])。
    fn resolve_entry_raw(
        &self,
        raw: &[u8],
        _png_only: bool,
    ) -> Option<(&std::path::PathBuf, &yuris_format::ypf::YpfEntryInfo)> {
        let r = self.vfs.resolve_bytes(raw)?;
        let (pack_path, index) = &self.packs[r.source as usize];
        let e = &index.entries[r.index as usize];
        Some((pack_path, e))
    }
/// scenario 资源名解析(播放器;P7.3):
    /// 1. 直名/归一/补扩展名(`resolve_entry`);
    /// 2. `cg\{path}`(\S 的 path 相对 cg\);
    /// 3. `cgg\{name}.png` / `cg\item\{name}.png`(\BG/\S 常用族);
    /// 4. `cg\stand\<档位>\...\{name}.png`(立绘;**档位确定性优先**,
    ///    见 [`crate::vfs::VfsIndex::tachie_candidates`];旧「多候选取首个」
    ///    实现遍历 HashMap → 每次启动命中随机档位,已勘误 —— layout-fix-plan §2.1)。
    pub fn read_cg_bytes(&self, path_or_name: &str) -> Option<Vec<u8>> {
        let norm = path_or_name.replace('/', "\\");
        let trimmed = norm.trim();
        // 候选族(引擎的逻辑名映射;`cg\` 相对根的常用形态)。
        // 逐条喂给 VFS 解析管线 —— 编码/大小写/分隔符与回退链**全部**
        // 由 `crate::vfs` 统一处理(成果 136)。
        let cands: Vec<String> = vec![
            trimmed.to_string(),
            format!("cg\\{}", trimmed),
            format!("cg\\bg\\{}.png", trimmed),
            format!("cg\\item\\{}.png", trimmed),
            format!("{}.png", trimmed),
        ];
        for c in &cands {
            if let Some(b) = self.read_image_bytes(c) {
                return Some(b);
            }
        }
        // 立绘:确定性档位选择(不依赖遍历顺序,见 layout-fix-plan P0)
        let key = self.vfs.tachie_candidates(trimmed)?;
        self.read_key_bytes(&key)
    }

    /// **读取已解析条目**(按归一化键直查;音频/资源层复用)。
    ///
    /// 不走 `display()` 回灌 —— 对 CP932 名那是有损的(成果 136)。
    pub fn read_key_bytes(&self, key: &crate::vfs::LookupKey) -> Option<Vec<u8>> {
        let r = self.vfs.entry_of_key(key)?;
        let (pack_path, index) = &self.packs[r.source as usize];
        let e = &index.entries[r.index as usize];
        self.read_entry_payload(pack_path, e)
    }

    /// 条目键遍历(音频名解析等;归一化键空间)。
    pub fn keys(&self) -> impl Iterator<Item = &crate::vfs::LookupKey> {
        self.vfs.keys()
    }

    /// 读 stored 条目内容(se 型明文 OGG/PNG/WAV;P9.1 音频用)。
    pub fn read_stored_bytes(&self, path: &str) -> Option<Vec<u8>> {
        let (pack_path, e) = self.resolve_entry(path, false)?;
        self.read_entry_payload(pack_path, e)
    }

}

/// 纹理解析失败的原因([`PacFileIndex::explain_image_miss`])。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageMiss {
    /// 全部封包 + 松散根里都没有这个名字(VFS 候选族已穷举)。
    NoEntry,
    /// 条目存在但载荷读不出来(压缩/偏移问题)。
    Unreadable(String),
    /// 条目存在且能读,但魔数不是图像。
    NotImage(String, usize),
    /// 载荷是图像但宿主解码/上传失败。
    DecodeFailed(String, usize),
}

impl ImageMiss {
    /// 面向用户的一句话说明(含条目名与实际字节数)。
    pub fn describe(&self) -> String {
        match self {
            ImageMiss::NoEntry => {
                "封包内**不存在**该名字(已按扩展名探测序 / CP932 / 大小写归一检索)".into()
            }
            ImageMiss::Unreadable(n) => format!("条目存在但载荷读取失败: {n}"),
            ImageMiss::NotImage(n, len) => {
                format!("条目存在({n})但载荷不是图像魔数(解压后 {len}B)")
            }
            ImageMiss::DecodeFailed(n, len) => {
                format!("条目存在且是图像({n}, {len}B)但解码/上传失败")
            }
        }
    }
}

/// PNG 尺寸(IHDR 块;`[16..20]`=宽 / `[20..24]`=高,大端)。
fn png_dims(d: &[u8]) -> Option<(i64, i64)> {
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if d.len() < 24 || d[..8] != PNG_MAGIC {
        return None;
    }
    let w = u32::from_be_bytes(d[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(d[20..24].try_into().ok()?);
    Some((w as i64, h as i64))
}

/// JPEG 尺寸:扫描到 SOF0/1/2/3/5/6/7/9/10/11/13/14/15 段
/// (`FFC0..FFCF` 去掉 C4(霍夫曼表)/C8/Cc(算术编码)),其载荷
/// `[精度:1][高:2 BE][宽:2 BE]`。
///
/// **为什么必须支持**:本作片头 LOGO = `cg/ev/サークル.jpg`;旧实现只认 PNG
/// ⇒ `CGINFO SX/SY` 应答 0 ⇒ 显示层组退化成 1×1(成果 143)。
pub fn jpeg_dims(d: &[u8]) -> Option<(i64, i64)> {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < d.len() {
        if d[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = d[i + 1];
        // 填充字节 FF 串
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // 无载荷标记(SOI/EOI/RSTn/TEM)
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let seg_len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        if seg_len < 2 {
            return None;
        }
        // SOF 族:C0..CF 去掉 C4/C8/CC
        let is_sof = (0xC0..=0xCF).contains(&marker)
            && marker != 0xC4
            && marker != 0xC8
            && marker != 0xCC;
        if is_sof {
            // 载荷:[len:2][精度:1][高:2][宽:2]
            let p = i + 2;
            if p + 7 > d.len() {
                return None;
            }
            let h = u16::from_be_bytes([d[p + 3], d[p + 4]]) as i64;
            let w = u16::from_be_bytes([d[p + 5], d[p + 6]]) as i64;
            return Some((w, h));
        }
        // SOS(FFDA)之后就是熵编码数据,尺寸必在此之前;没找到 = 不支持。
        if marker == 0xDA {
            return None;
        }
        i += 2 + seg_len;
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::vfs::{ResolveStage, VfsIndex};

    fn keys(list: &[&str]) -> Vec<Vec<u8>> {
        list.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    /// 建一个只含"包内条目名"的 VFS(模拟索引侧)。
    fn idx_of(names: &[Vec<u8>]) -> VfsIndex {
        let mut v = VfsIndex::default();
        for (i, n) in names.iter().enumerate() {
            v.insert(n, 0, i as u32);
        }
        v
    }

    #[test]
    fn pick_stand_key_prefers_m050_regardless_of_order() {
        let k = keys(&[
            "cg\\stand\\m_066\\a_han\\a_han_1a\\a_han_1a0100.png",
            "cg\\stand\\m_030\\a_han\\a_han_1a\\a_han_1a0100.png",
            "cg\\stand\\m_050\\a_han\\a_han_1a\\a_han_1a0100.png",
            "cg\\stand\\m_040\\a_han\\a_han_1a\\a_han_1a0100.png",
        ]);
        let v = idx_of(&k);
        let got = v.tachie_candidates("a_han_1a0100").unwrap();
        assert!(got.display().contains("m_050"));

        let k2: Vec<Vec<u8>> = k.iter().rev().cloned().collect();
        let v2 = idx_of(&k2);
        assert!(v2.tachie_candidates("a_han_1a0100").unwrap().display().contains("m_050"));
    }

    #[test]
    fn pick_stand_key_fallbacks_and_filters() {
        let k = keys(&[
            "cg\\face\\a_han\\a_han_1a\\a_han_1a0100.png",
            "cg\\stand\\m_066\\a_han\\a_han_1a\\a_han_1a0100.png",
            "cg\\stand\\m_050\\a_han\\a_han_1a\\a_han_1a0200.png",
        ]);
        let v = idx_of(&k);
        // m_050 存在但文件名不匹配 → 回退 m_066;face 不参与
        assert!(v.tachie_candidates("a_han_1a0100").unwrap().display().contains("m_066"));
        assert!(v.tachie_candidates("nope").is_none());
    }

    /// **回归(成果 136)**:玩家侧 `read_cg_bytes` 的立绘路径必须与
    /// VFS 的档位选择一致 —— 旧实现在这里另算一遍 `to_ascii_lowercase`,
    /// 与 `pick_stand_key` 的忽略大小写比较是两套规则。
    #[test]
    fn tachie_stage_is_likely_not_contract() {
        let k = keys(&["cg\\stand\\m_050\\x\\x_1a01.png"]);
        let v = idx_of(&k);
        // 立绘档位是 Likely 启发式(证据=对拍拟合),不得标记为契约阶段
        assert!(!ResolveStage::TachieTier.is_contract());
        assert!(ResolveStage::Exact.is_contract());
        let _ = v;
    }
}

#[cfg(test)]
mod script_id_tests {
    use super::script_id_of;

    /// 脚本号解析必须**对任意虚拟根前缀**成立。
    ///
    /// 实测:同一引擎不同封包的根前缀不同(`$` / `%` / `9` / 其它单字节),
    /// 旧实现 `strip_prefix("$ysbin\\yst")` 只在 `$` 根下成立。
    #[test]
    fn accepts_any_root_prefix() {
        assert_eq!(script_id_of(br"$ysbin\yst00034.ybn"), Some(34));
        assert_eq!(script_id_of(br"%ysbin\yst00034.ybn"), Some(34));
        assert_eq!(script_id_of(br"9ysbin\yst00034.ybn"), Some(34));
        // 非可打印根前缀（实测存在）
        assert_eq!(script_id_of(b"\x16ysbin\\yst00123.ybn"), Some(123));
        assert_eq!(script_id_of(b"\x12ysbin\\yst00001.ybn"), Some(1));
    }

    #[test]
    fn accepts_forward_slashes() {
        assert_eq!(script_id_of(b"$ysbin/yst00034.ybn"), Some(34));
    }

    #[test]
    fn rejects_non_scripts() {
        assert_eq!(script_id_of(br"$ysbin\ysc.ybn"), None);
        assert_eq!(script_id_of(br"$ysbin\yst1234.ybn"), None); // 位数不足
        assert_eq!(script_id_of(br"$ysbin\yst123456.ybn"), None);
        assert_eq!(script_id_of(br"$ysbin\ystabcde.ybn"), None);
        assert_eq!(script_id_of(br"$ysbin\yst00034.ybnx"), None);
        assert_eq!(script_id_of(br"$other\yst00034.ybn"), None);
        assert_eq!(script_id_of(b"yst00034.ybn"), None); // 无目录
    }
}

#[cfg(test)]
mod jpeg_dims_tests {
    use super::*;

    #[test]
    fn sof0_dims() {
        // SOI, APP0(len 16), SOF0(len 17, prec 8, H=825, W=1100, 3 comp), EOI
        let mut d = vec![0xFF, 0xD8];
        d.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        d.extend_from_slice(b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
        d.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        d.extend_from_slice(&825u16.to_be_bytes());
        d.extend_from_slice(&1100u16.to_be_bytes());
        d.extend_from_slice(&[0x03]);
        d.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(jpeg_dims(&d), Some((1100, 825)));
    }

    #[test]
    fn progressive_sof2_dims() {
        let mut d = vec![0xFF, 0xD8];
        d.extend_from_slice(&[0xFF, 0xC2, 0x00, 0x11, 0x08]);
        d.extend_from_slice(&200u16.to_be_bytes());
        d.extend_from_slice(&300u16.to_be_bytes());
        d.extend_from_slice(&[0x03, 0xFF, 0xD9]);
        assert_eq!(jpeg_dims(&d), Some((300, 200)));
    }

    #[test]
    fn skips_dht_and_restart_markers() {
        let mut d = vec![0xFF, 0xD8];
        // DHT (C4) 必须跳过,不能当 SOF
        d.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x06, 1, 2, 3, 4]);
        // RST0 无载荷
        d.extend_from_slice(&[0xFF, 0xD0]);
        d.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        d.extend_from_slice(&64u16.to_be_bytes());
        d.extend_from_slice(&48u16.to_be_bytes());
        d.extend_from_slice(&[0x03]);
        assert_eq!(jpeg_dims(&d), Some((48, 64)));
    }

    #[test]
    fn png_dims_ok_and_jpeg_rejected() {
        let mut p = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        p.extend_from_slice(&[0, 0, 0, 13]); // IHDR len
        p.extend_from_slice(b"IHDR");
        p.extend_from_slice(&640u32.to_be_bytes());
        p.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(png_dims(&p), Some((640, 480)));
        assert_eq!(png_dims(&[0xFF, 0xD8, 0, 0]), None);
    }

    #[test]
    fn non_jpeg_is_none() {
        assert_eq!(jpeg_dims(b"not an image"), None);
        assert_eq!(jpeg_dims(&[]), None);
    }
}
