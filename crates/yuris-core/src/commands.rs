//! 命令 Profile：**运行期**的命令名 ↔ 引擎序号映射。
//!
//! ## 为什么必须有这个模块
//!
//! `CONTEXT.md` 早就写了「跨版本差异必须走 Profile，不得散落硬编码」，但
//! 代码里长期 0 命中 —— VM 直接 `match g.command_type { cmd::GO => … }`，
//! 把某一版的序号当成了宇宙常量。
//!
//! 实测两版命令表（`docs/cross-version-commands.md`，生成器
//! `scripts/probe_cross_version.py`）：
//!
//! | 命令 | 引擎 555 序号 | 引擎 494 序号 |
//! |---|---|---|
//! | `CG` | 1 | 1 |
//! | `GO` | 42 | 43 |
//! | `LET` | 53 | 54 |
//! | `RETURN` | 79 | 77 |
//! | `VARACT` | 102 | 100 |
//! | `WAIT` | 104 | 102 |
//!
//! 116 条共有命令里 **104 条序号不同**。所以命令序号**只能来自游戏自己的
//! `ysc.ybn`**：YSTB 的 part1 byte0 存的就是该游戏 YSCM 表的下标，解码它的
//! 唯一正确依据是同一款游戏装载的 YSCM。
//!
//! ## 证据等级
//!
//! - 结构（名 → 序号，序号 = YSCM 表次序）：**Confirmed**。两侧独立实测，
//!   `scripts/probe_cross_version.py` 可复现。
//! - 内置表按**引擎版本号**选取（[`CommandProfile::for_engine_version`]），
//!   **不存在跨版本兜底**（勘误 2026-10-18：旧文写「[`CommandProfile::v555`]
//!   是兜底默认，尚未装载 YSCM 时使用」—— 与实现相悖，实现自 T2 起未知
//!   版本一律落到空表「未解析分派」，显式报未实现，见 CONTEXT
//!   「未解析分派」）。`v555()`/`v494()`/`sdk_*()` 仅在**游戏自己声明的
//!   版本号命中**时被选用；版本号未命中时调用方必须注入游戏自身 YSCM。

use std::collections::HashMap;

/// 引擎 v555 的 YSCM 命令名表，按序号。
///
/// 与 `docs/opcode/yscm-commands-555.json` 逐条一致（见本模块测试）。
pub const V555_COMMAND_NAMES: [&str; 121] = [
    "ALIAS", "CG", "CGACT", "CGEND", "CGINFO", "CLIPACT", "CLIPINFO", "CSV", "CSVINFO",
    "DEBUGLIST", "DIALOG", "ELSE", "EMOTE", "END", "ERROR", "ERRORINFO", "F_FLT", "F_INT",
    "F_STR", "F_VAR", "FILEACT", "FILEINFO", "FLASH", "FLASHEND", "FLASHINFO", "FLT", "FONT",
    "FONTINFO", "FPS", "G_FLT", "G_FLT2", "G_FLT3", "G_FLT4", "G_INT", "G_INT2", "G_INT3",
    "G_INT4", "G_STR", "G_STR2", "G_STR3", "G_STR4", "G_VAR", "GO", "GOSUB", "IF", "IFBLEND",
    "IFBREAK", "IFCONTINUE", "IFEND", "INPUT", "INT", "LABEL", "LABELINFO", "LET", "LOAD",
    "LOOP", "LOOPBREAK", "LOOPCONTINUE", "LOOPEND", "MACRO", "MATH", "MATH3D", "MENU",
    "MENUACT", "MENUEND", "MENUINFO", "MIDI", "MIDIEND", "MIDIINFO", "MOUSE", "MOVIE",
    "MOVIEEND", "MOVIEINFO", "MOVIE2", "MOVIE2END", "MOVIE2INFO", "PLUG", "PREP", "REGINFO",
    "RETURN", "RETURNBREAK", "RETURNCODE", "S_FLT", "S_INT", "S_STR", "S_VAR", "SAVE",
    "SAVEEND", "SAVEINFO", "SOUND", "SOUNDEND", "SOUNDINFO", "STR", "SYSTEM", "SYSTEMINFO",
    "TASK", "TASKEND", "TASKINFO", "TEXT", "TEXTINFO", "TRAY", "VAR", "VARACT", "VARINFO",
    "WAIT", "WINDOW", "WINDOWEND", "WINDOWINFO", "WORD", "YMV", "YMVEND", "YMVINFO", "_",
    "_IFDEF", "_IFNDEF", "_IFEND", "PRODUCT", "SYSTEMMODE", "COMPILEMODE", "RELEASEMODE",
    "PROJECTFOLDER",
];

/// 官方 SDK 0.495 的命令名表，按序号（`YSCom.ycd` 的版本字段 = 495）。
///
/// **一等公民**：官方 SDK 是命令语义的权威来源。
/// 与运行期 YSCM（`docs/opcode/yscm-commands-494.json`）**逐条同序** —— 
/// 即「SDK 编译器命令序 == 运行期 YSCM 序」，故这一张表同时覆盖 494 与 495。
pub const SDK_495_COMMAND_NAMES: [&str; 119] = [
    "CDDA", "CG", "CGACT", "CGEND", "CGINFO", "CLIPACT", "CLIPINFO", "CSV", "CSVINFO",
    "DEBUGLIST", "DIALOG", "ELSE", "EMOTE", "EMOTEINFO", "END", "ERROR", "ERRORINFO", "F_FLT",
    "F_INT", "F_STR", "F_VAR", "FILEACT", "FILEINFO", "FLASH", "FLASHEND", "FLASHINFO", "FLT",
    "FONT", "FONTINFO", "FPS", "G_FLT", "G_FLT2", "G_FLT3", "G_FLT4", "G_INT", "G_INT2",
    "G_INT3", "G_INT4", "G_STR", "G_STR2", "G_STR3", "G_STR4", "G_VAR", "GO", "GOSUB", "IF",
    "IFBLEND", "IFBREAK", "IFCONTINUE", "IFEND", "INPUT", "INT", "LABEL", "LABELINFO", "LET",
    "LOAD", "LOOP", "LOOPBREAK", "LOOPCONTINUE", "LOOPEND", "MACRO", "MATH", "MATH3D", "MENU",
    "MENUACT", "MENUEND", "MENUINFO", "MIDI", "MIDIEND", "MIDIINFO", "MOUSE", "MOVIE",
    "MOVIEEND", "MOVIEINFO", "PLUG", "PREP", "REGINFO", "RETURN", "RETURNBREAK", "RETURNCODE",
    "S_FLT", "S_INT", "S_STR", "S_VAR", "SAVE", "SAVEEND", "SAVEINFO", "SOUND", "SOUNDEND",
    "SOUNDINFO", "STR", "SYSTEM", "SYSTEMINFO", "TASK", "TASKEND", "TASKINFO", "TEXT",
    "TEXTINFO", "TRAY", "VAR", "VARACT", "VARINFO", "WAIT", "WINDOW", "WINDOWEND",
    "WINDOWINFO", "WORD", "YMV", "YMVEND", "YMVINFO", "_", "_IFDEF", "_IFNDEF", "_IFEND", ");",
    "SYSTEMMODE", "COMPILEMODE", "RELEASEMODE", "PROJECTFOLDER",
];

/// 官方 SDK 0.479 的命令名表，按序号（`YSCom.ycd` 的版本字段 = 479）。
///
/// 与 `docs/opcode/yscm-commands-479.json` 逐条一致（见本模块测试）。
/// 相对 495：少 15 条 —— `EMOTE`/`EMOTEINFO`/`F_FLT`/`F_INT`/`F_STR`/`F_VAR`/
/// `G_FLT2..4`/`G_INT2..4`/`G_STR2..4`（见对照表 §2）。
pub const SDK_479_COMMAND_NAMES: [&str; 104] = [
    "CDDA", "CG", "CGACT", "CGEND", "CGINFO", "CLIPACT", "CLIPINFO", "CSV", "CSVINFO",
    "DEBUGLIST", "DIALOG", "ELSE", "END", "ERROR", "ERRORINFO", "FILEACT", "FILEINFO", "FLASH",
    "FLASHEND", "FLASHINFO", "FLT", "FONT", "FONTINFO", "FPS", "G_FLT", "G_INT", "G_STR",
    "G_VAR", "GO", "GOSUB", "IF", "IFBLEND", "IFBREAK", "IFCONTINUE", "IFEND", "INPUT", "INT",
    "LABEL", "LABELINFO", "LET", "LOAD", "LOOP", "LOOPBREAK", "LOOPCONTINUE", "LOOPEND",
    "MACRO", "MATH", "MATH3D", "MENU", "MENUACT", "MENUEND", "MENUINFO", "MIDI", "MIDIEND",
    "MIDIINFO", "MOUSE", "MOVIE", "MOVIEEND", "MOVIEINFO", "PLUG", "PREP", "REGINFO", "RETURN",
    "RETURNBREAK", "RETURNCODE", "S_FLT", "S_INT", "S_STR", "S_VAR", "SAVE", "SAVEEND",
    "SAVEINFO", "SOUND", "SOUNDEND", "SOUNDINFO", "STR", "SYSTEM", "SYSTEMINFO", "TASK",
    "TASKEND", "TASKINFO", "TEXT", "TEXTINFO", "TRAY", "VAR", "VARACT", "VARINFO", "WAIT",
    "WINDOW", "WINDOWEND", "WINDOWINFO", "WORD", "YMV", "YMVEND", "YMVINFO", "_", "_IFDEF",
    "_IFNDEF", "_IFEND", ");", "SYSTEMMODE", "COMPILEMODE", "RELEASEMODE", "PROJECTFOLDER",
];

/// 本内核内置的引擎版本表（版本号 → 命令名表）。
///
/// 覆盖官方 SDK 的两个版本 + 实测样本版本。新版本只需在
/// [`CommandProfile::for_engine_version`] 里加一行；**命令名跨版本稳定**，
/// 因此新增版本通常不需要改 VM。
pub const BUILTIN_ENGINE_VERSIONS: [u32; 4] = [479, 494, 495, 555];

/// FNV-1a：把参数名折成 32 位键（**零分配**）。
///
/// 用途：`slot_of` 的热路径索引键。参数名是短的 ASCII 标识符（`FILE`/
/// `VOLUME`/`COM_CG`…），FNV 在这里足够分散；命中后仍会**核对真实名字**，
/// 所以哈希碰撞不会导致错误槽位（只多一次不必要的比对）。
fn fnv1a(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// 一个引擎版本的**命令名 ↔ 序号**表,以及**命令 → 参数名表**。
///
/// 由启动链从游戏自己的 YSCM（`ysbin\ysc.ybn`）构建；序号 = YSCM 表次序，
/// YSTB part1 的 `command_type` 字节即此序号。
///
/// ## 为什么还要参数名表
///
/// YSTB 每个参数窗口的 `tag & 0xff` 是**参数槽位**（= 该命令在 YSCM 参数表里的
/// 下标）。这个下标**逐版本漂移**，与命令序号漂移是同一类问题：
///
/// | 参数 | 引擎 555 | SDK 495 | SDK 479 |
/// |---|---|---|---|
/// | `CG.FILE` | 槽 46 | 槽 33 | 槽 29 |
/// | `CG.MODE` | 槽 54 | 槽 41 | 槽 36 |
/// | `TEXT.CLEAR` | 槽 2 | 槽 16 | 槽 16 |
///
/// 早期实现把 v555 的槽号写死进 VM 各处理器（如「`CG` 的 `FILE` 读槽 46」），
/// 遇到别的版本时该参数**永远读不到** —— 表现为 CG 事件 `file: None`、
/// 纹理永不装载、画面全黑。故取值必须走 [`CommandProfile::slot_of`]。
#[derive(Debug, Clone, Default)]
pub struct CommandProfile {
    /// 序号 → 命令名（次序即 YSCM 表次序）。
    names: Vec<String>,
    /// 序号 → 参数名表（下标 = 槽位）。空 = 该命令无参数数据。
    params: Vec<Vec<String>>,
    /// 命令名 → 序号（首个命中；YSCM 名唯一性由构建侧校验）。
    index_of: HashMap<String, u16>,
    /// **槽位索引：(序号, 参数名) → 槽号**（构建时一次性预计算）。
    ///
    /// 为什么不现查：旧实现 `slot_of` 对参数表做**线性字符串比较**，而处理器
    /// 每次执行都要按名取槽（`TASKINFO` 一次要 21 个）—— 每帧数万次字符串
    /// 扫描。这里建索引后查询是 O(1)，且不改变任何语义（同名取首个，与
    /// 线性 `position` 一致）。
    slot_index: HashMap<(u16, u32), u8>,
    /// 引擎版本号（YSCM header 的 version 字段；0 = 未知）。
    engine_version: u32,
    /// 来源描述（诊断用）。
    source: String,
}

impl CommandProfile {
    /// 由按序号排列的命令名构建（**无参数表**）。
    ///
    /// 名字重复时保留**首个**序号（后出现者不覆盖），与引擎「按名字查表」
    /// 的语义一致；重复本身是可诊断的数据异常，用 [`Self::duplicate_names`] 查。
    pub fn from_names<I, S>(names: I, engine_version: u32, source: impl Into<String>) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names: Vec<String> = names.into_iter().map(Into::into).collect();
        let mut index_of = HashMap::with_capacity(names.len());
        for (i, n) in names.iter().enumerate() {
            index_of.entry(n.clone()).or_insert(i as u16);
        }
        Self {
            names,
            params: Vec::new(),
            index_of,
            slot_index: HashMap::new(),
            engine_version,
            source: source.into(),
        }
    }

    /// 由命令名表 + 参数名表构建（**推荐**：槽位解析需要参数表）。
    ///
    /// `params[i]` = 序号 `i` 的命令的参数名（下标即槽位）。长度不足者按
    /// 空表处理；多余项忽略。
    pub fn from_names_and_params<I, S, P, Q>(
        names: I,
        params: P,
        engine_version: u32,
        source: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
        P: IntoIterator<Item = Q>,
        Q: IntoIterator,
        Q::Item: AsRef<str>,
    {
        let mut p = Self::from_names(names, engine_version, source);
        p.params = params
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|s| s.as_ref().to_string())
                    .collect()
            })
            .collect();
        // 预计算槽位索引（同名取**首个**，与 `position()` 语义一致）。
        let total: usize = p.params.iter().map(Vec::len).sum();
        p.slot_index = HashMap::with_capacity(total);
        for (idx, row) in p.params.iter().enumerate() {
            for (slot, name) in row.iter().enumerate() {
                p.slot_index
                    .entry((idx as u16, fnv1a(name.as_bytes())))
                    .or_insert(slot as u8);
            }
        }
        p
    }

    /// 官方 SDK 0.495 的 profile（**默认基准**：官方 SDK 为准）。
    pub fn sdk_495() -> Self {
        Self::from_names_and_params(
            SDK_495_COMMAND_NAMES,
            crate::command_params::SDK_495_COMMAND_PARAMS,
            495,
            "official SDK 0.495 (YSCom.ycd)",
        )
    }

    /// 官方 SDK 0.479 的 profile。
    pub fn sdk_479() -> Self {
        Self::from_names_and_params(
            SDK_479_COMMAND_NAMES,
            crate::command_params::SDK_479_COMMAND_PARAMS,
            479,
            "official SDK 0.479 (YSCom.ycd)",
        )
    }

    /// 最新官方 SDK 的 profile（当前 = 495）。
    ///
    /// 用于「没有游戏 YSCM、也没有版本线索」的兜底场景（工具/测试）。
    /// **生产路径不得依赖它**：运行期必须用游戏自身的 YSCM。
    pub fn sdk_latest() -> Self {
        Self::sdk_495()
    }

    /// 引擎 v555 的内置表（实测样本版本；非 SDK 版本）。
    pub fn v555() -> Self {
        Self::from_names_and_params(
            V555_COMMAND_NAMES,
            crate::command_params::V555_COMMAND_PARAMS,
            555,
            "builtin 555 command table",
        )
    }

    /// 引擎 494 的运行期 YSCM 表（与 SDK 495 同序）。
    ///
    /// 参数表用 SDK 495 的：两侧仅 `VARINFO`/`SYSTEMMODE` 两条命令的参数数
    /// 不同（21/36 vs 19/32），其余逐条一致（`scripts/gen_command_params.py`
    /// 可复现）。运行期由游戏自身 YSCM 覆盖，这里只是无 YSCM 时的兜底。
    pub fn v494() -> Self {
        Self::from_names_and_params(
            SDK_495_COMMAND_NAMES,
            crate::command_params::SDK_495_COMMAND_PARAMS,
            494,
            "builtin 494 command table",
        )
    }

    /// 按**引擎版本号**取内置表。
    ///
    /// 返回 `None` = 本内核不认识该版本 → 调用方**不得猜测**，应由启动链
    /// 提供游戏自身的 YSCM（`ysbin\ysc.ybn` 永远存在，是权威来源）。
    pub fn for_engine_version(version: u32) -> Option<Self> {
        match version {
            479 => Some(Self::sdk_479()),
            494 => Some(Self::v494()),
            495 => Some(Self::sdk_495()),
            555 => Some(Self::v555()),
            _ => None,
        }
    }

    /// 引擎版本号（YSCM header 字段）。
    pub fn engine_version(&self) -> u32 {
        self.engine_version
    }

    /// 命令条数。
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// 来源描述。
    pub fn source(&self) -> &str {
        &self.source
    }

    /// 按序号取命令名。
    pub fn name_of(&self, index: u16) -> Option<&str> {
        self.names.get(index as usize).map(String::as_str)
    }

    /// 按命令名取序号。
    pub fn index_of(&self, name: &str) -> Option<u16> {
        self.index_of.get(name).copied()
    }

    /// 全部命令名（按序号）。
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// 是否带参数名表（槽位解析可用）。
    pub fn has_params(&self) -> bool {
        !self.params.is_empty()
    }

    /// 序号 `index` 的命令的参数名表（下标 = 槽位）。
    pub fn params_of(&self, index: u16) -> &[String] {
        self.params.get(index as usize).map(Vec::as_slice).unwrap_or(&[])
    }

    /// **参数名 → 槽位**。
    ///
    /// 这是 VM 读取参数窗口的唯一正确入口：槽位号逐版本漂移，硬编码必错。
    /// 返回 `None` = 该命令无参数表，或表里没有这个参数名（调用方须按
    /// 「未确认」处理，不得回落到某个版本的常量）。
    pub fn slot_of(&self, index: u16, param: &str) -> Option<u8> {
        // 索引未建（纯 `from_names` 构造）→ 退回线性扫描（保持旧行为）。
        if self.slot_index.is_empty() {
            return self
                .params_of(index)
                .iter()
                .position(|p| p == param)
                .map(|i| i as u8);
        }
        let h = fnv1a(param.as_bytes());
        self.slot_index
            .get(&(index, h))
            .copied()
            .filter(|slot| {
                // 哈希冲突防护:命中后核对真实名字(极便宜,仅比对一处)。
                self.params_of(index)
                    .get(*slot as usize)
                    .map(|n| n == param)
                    .unwrap_or(false)
            })
    }

    /// **参数名 → 槽位**（按命令名；`CmdKind` 类调用方用）。
    pub fn slot_of_named(&self, command: &str, param: &str) -> Option<u8> {
        let i = self.index_of(command)?;
        self.slot_of(i, param)
    }

    /// 重复出现的命令名（数据异常诊断；正常 YSCM 应为空）。
    pub fn duplicate_names(&self) -> Vec<String> {
        let mut seen = HashMap::<&str, usize>::new();
        for n in &self.names {
            *seen.entry(n.as_str()).or_insert(0) += 1;
        }
        let mut d: Vec<String> = seen
            .into_iter()
            .filter(|(_, c)| *c > 1)
            .map(|(n, _)| n.to_string())
            .collect();
        d.sort();
        d
    }

    /// 与另一 profile 的序号差异（仅列出两边都有的名字）。
    ///
    /// 返回 `(命令名, self 序号, other 序号)`，按名字排序。用于跨版本核对与
    /// 测试断言「两版序号确实不同」。
    pub fn index_diffs(&self, other: &CommandProfile) -> Vec<(String, u16, u16)> {
        let mut out = Vec::new();
        for (i, n) in self.names.iter().enumerate() {
            if let Some(j) = other.index_of(n) {
                if j != i as u16 {
                    out.push((n.clone(), i as u16, j));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从 `docs/opcode/yscm-commands-<ver>.json` 抽取命令名。
    ///
    /// 文件由 `scripts/probe_cross_version.py` 生成，格式稳定（每条一行
    /// `"name": "X"`）；此处避免为一个测试引入 serde 依赖。
    fn doc_names(ver: u32) -> Option<Vec<String>> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../docs/opcode/yscm-commands-{ver}.json"));
        let s = std::fs::read_to_string(p).ok()?;
        let mut names = Vec::new();
        for line in s.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("\"name\": \"") {
                if let Some(end) = rest.find('"') {
                    names.push(rest[..end].to_string());
                }
            }
        }
        Some(names)
    }

    #[test]
    fn builtin_v555_list_matches_doc_json() {
        let Some(names) = doc_names(555) else {
            eprintln!("docs/opcode/yscm-commands-555.json 不可读，skip");
            return;
        };
        assert_eq!(names.len(), V555_COMMAND_NAMES.len(), "命令条数应一致");
        assert_eq!(
            names,
            V555_COMMAND_NAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "内嵌 555 名单必须与 docs/opcode/yscm-commands-555.json 逐条一致"
        );
    }

    #[test]
    fn builtin_v494_list_matches_doc_json() {
        let Some(names) = doc_names(494) else {
            eprintln!("docs/opcode/yscm-commands-494.json 不可读，skip");
            return;
        };
        assert_eq!(names.len(), SDK_495_COMMAND_NAMES.len(), "命令条数应一致");
        assert_eq!(
            names,
            SDK_495_COMMAND_NAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "内嵌 494 名单必须与 docs/opcode/yscm-commands-494.json 逐条一致"
        );
    }

    #[test]
    fn builtin_profile_lookups() {
        let p = CommandProfile::v555();
        assert_eq!(p.engine_version(), 555);
        assert_eq!(p.len(), 121);
        assert_eq!(p.index_of("GO"), Some(0x2a));
        assert_eq!(p.index_of("CG"), Some(0x01));
        assert_eq!(p.index_of("VARACT"), Some(0x66));
        assert_eq!(p.index_of("WAIT"), Some(0x68));
        assert_eq!(p.name_of(0x68).as_deref(), Some("WAIT"));
        assert!(p.duplicate_names().is_empty(), "正常 YSCM 不应有重名");
    }

    #[test]
    fn unknown_name_and_index_are_none() {
        let p = CommandProfile::v555();
        assert_eq!(p.index_of("NO_SUCH_COMMAND"), None);
        assert_eq!(p.name_of(9999), None);
    }

    /// 跨版本序号漂移：494 与 555 对照（P1 的立论依据）。
    #[test]
    fn cross_version_indices_differ_for_most_shared_commands() {
        let v555 = CommandProfile::v555();
        let v494 = CommandProfile::v494();
        assert_eq!(v494.len(), 119);

        assert_eq!(v555.index_of("GO"), Some(42));
        assert_eq!(v494.index_of("GO"), Some(43));
        assert_eq!(v555.index_of("WAIT"), Some(104));
        assert_eq!(v494.index_of("WAIT"), Some(102));
        assert_eq!(v555.index_of("VARACT"), Some(102));
        assert_eq!(v494.index_of("VARACT"), Some(100));
        assert_eq!(v555.index_of("RETURN"), Some(79));
        assert_eq!(v494.index_of("RETURN"), Some(77));

        let diffs = v555.index_diffs(&v494);
        let shared = v555
            .names()
            .iter()
            .filter(|n| v494.index_of(n).is_some())
            .count();
        assert_eq!(shared, 116, "两版共有命令应为 116 条");
        assert_eq!(diffs.len(), 104, "其中 104 条序号不同（实测）");
    }

    /// 494 与 SDK 编译器侧 495 命令表逐条同序。
    #[test]
    fn v494_matches_sdk_compiler_table_495() {
        let Some(names) = doc_names(495) else {
            eprintln!("docs/opcode/yscm-commands-495.json 不可读，skip");
            return;
        };
        assert_eq!(
            names,
            SDK_495_COMMAND_NAMES.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "运行期 YSCM 序 == SDK 编译器命令序"
        );
    }
}
