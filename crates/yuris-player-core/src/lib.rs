//! 平台无关播放器核心(P-core)。
//!
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//!
//! 从 yuris-cli 抽出:PlayerCore(ScenarioHost 实现)/ Player(tick/按键/快存快读)/
//! scenario 播放器 / yuris-audio 音频 / letterbox 数学。winit 事件循环、窗口创建与
//! 输入翻译归平台壳(desktop `yuris-cli` / android `neko-android`);本 crate
//! 不依赖窗口创建路径 —— `WgpuBackend` 仅在壳持窗后注入。
//!
//! 依赖 winit 是经由 yuris-render 的 `Arc<winit::window::Window>`;winit 0.30
//! 官方支持 Android(android-activity 后端),故本 crate 可直接复用于移动端。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use yuris_render::{logical_h, logical_w, WgpuBackend};
use yuris_runtime::GraphicsBackend;
use yuris_scene::{Layer, ResourceId, Scene};
use yuris_stage::{LayerId, TexId};

use yuris_vm::bridge::{fnv1a, SceneBridge};
use yuris_vm::host::PacFileIndex;
use yuris_vm::GroupVm;

pub mod audio;
pub mod audio_probe;
pub mod overlay;
pub mod scenario;

pub use scenario::{ScenarioHost, ScenarioPlayer, TitleMenuAction};

/// 去掉引擎声音层组前缀 `:/段/`（如 `:/S0/`）。
///
/// 语料实测：脚本 `SOUND[ID=":/S0/" + …]` 的 ID 是**层组命名空间**，
/// 而 `FILE=` 才是磁盘/VFS 名字；两者都可能带此前缀。
///
/// **当前无调用者**(2026-10-18 清理告警时发现):保留依据待接线;若确认不需要可删。
#[allow(dead_code)]
fn strip_layer_prefix(name: &str) -> &str {
    let Some(rest) = name.strip_prefix(":/") else {
        return name;
    };
    match rest.find('/') {
        Some(idx) => &rest[idx + 1..],
        None => name,
    }
}

/// 字节版 [`strip_layer_prefix`]（CP932 原始名）。
fn strip_layer_prefix_bytes(name: &[u8]) -> &[u8] {
    if name.len() < 3 || &name[..2] != b":/" {
        return name;
    }
    match name[2..].iter().position(|b| *b == b'/') {
        Some(idx) => &name[2 + idx + 1..],
        None => name,
    }
}

/// `VmEvent::Sound` → [`yuris_audio::SoundCommand`]（成果 162）。
///
/// 纯映射：不查资源、不改状态，便于单元测试逐参数核对。
/// `ID` 为空时返回 `None`（引擎里 ID 是声层键，缺失即无法定位）。
fn sound_command_from_event(ev: &yuris_vm::VmEvent) -> Option<yuris_audio::SoundCommand> {
    let yuris_vm::VmEvent::Sound {
        id,
        enabled,
        play,
        pause,
        fade_ms,
        start_ms,
        restart_ms,
        end_ms,
        loop_count,
        volume,
        pan,
        pitch,
        speed,
        ..
    } = ev
    else {
        return None;
    };
    let id = id.as_deref().filter(|s| !s.is_empty())?;
    let mut cmd = yuris_audio::SoundCommand::new(id);
    cmd.enabled = *enabled;
    cmd.play = *play;
    cmd.pause = *pause;
    cmd.fade_ms = *fade_ms;
    cmd.start_ms = *start_ms;
    cmd.restart_ms = *restart_ms;
    cmd.end_ms = *end_ms;
    cmd.loop_count = *loop_count;
    cmd.volume = *volume;
    cmd.pan = *pan;
    cmd.pitch = *pitch;
    // SPEED：v494 处理器不读；仅保留原值供诊断。
    cmd.speed = *speed;
    Some(cmd)
}

/// `VmEvent::Sound` 的 `FILE` 原始字节（CP932）。
fn sound_event_file(ev: &yuris_vm::VmEvent) -> Option<&[u8]> {
    match ev {
        yuris_vm::VmEvent::Sound { file, .. } => file.as_deref(),
        _ => None,
    }
}

/// 脚本给的路径 → 可安全拼进游戏根目录的相对路径。
///
/// 引擎侧 `FILEACT` 的路径是相对游戏目录的（`0x403e68` 的 `~cwd`），
/// 故这里只做两件事：统一分隔符为 `/`，并**拒绝向外逃逸**（`..` 段与
/// 绝对路径）。脚本是外部数据，不能让它写到游戏目录之外。
fn normalize_script_path(raw: &str) -> std::path::PathBuf {
    let unified = raw.replace('\\', "/");
    let mut out = std::path::PathBuf::new();
    for seg in unified.split('/') {
        match seg {
            "" | "." => {}
            // 盘符段（`C:`）与 `..` 一律丢弃：不允许逃出游戏根。
            ".." => {
                out.pop();
            }
            s if s.ends_with(':') => {}
            s => out.push(s),
        }
    }
    out
}

/// 存档文件名规范化：手册 `file/fileact.html` 例[5]——
/// 「`temp` と指定した場合は `temp.sd` を指定したことになります」。
fn save_file_name(raw: &str) -> std::path::PathBuf {
    let p = normalize_script_path(raw);
    if p.extension().is_some() {
        return p;
    }
    let mut s = p.into_os_string();
    s.push(".sd");
    std::path::PathBuf::from(s)
}

/// `SOUND FILE=` → 资源候选名（成果 162）。
///
/// 语料实测形态：`bgm/xxx`、`voice/xxx`（**带目录、无扩展名**）；
/// 手册允许省略扩展名且搜索顺序为 `原名 → .OGG → .WAV`。
/// 裸名额外尝试六个音频根（`bgm/se/voice/sysse/sysvo`）。
///
/// 全部按 **CP932 原始字节**处理（名字含日文，不能先转 UTF-8）。
fn audio_name_candidates(name: &[u8]) -> Vec<Vec<u8>> {
    let stripped = strip_layer_prefix_bytes(name);
    if stripped.is_empty() {
        return Vec::new();
    }
    let has_sep = stripped.iter().any(|b| *b == b'\\' || *b == b'/');
    let leaf_has_dot = stripped
        .rsplit(|b| *b == b'\\' || *b == b'/')
        .next()
        .map(|leaf| leaf.contains(&b'.'))
        .unwrap_or(false);
    let roots: &[&[u8]] = if has_sep {
        &[b""]
    } else {
        &[
            b"",
            b"bgm/",
            b"se/",
            b"voice/",
            b"sysse/",
            b"sysvo/",
        ]
    };
    let mut out = Vec::new();
    for root in roots {
        let mut base = root.to_vec();
        base.extend_from_slice(stripped);
        if leaf_has_dot {
            out.push(base);
        } else {
            let mut ogg = base.clone();
            ogg.extend_from_slice(b".ogg");
            let mut wav = base.clone();
            wav.extend_from_slice(b".wav");
            out.push(ogg);
            out.push(wav);
            out.push(base);
        }
    }
    out
}

/// 按平台候选路径加载 CJK 字体(繁中优先,其次简中/日文)。
pub fn load_cjk_font() -> Option<Vec<u8>> {
    const CANDIDATES: &[&str] = &[
        // Windows
        r"C:\Windows\Fonts\msjh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
        // macOS
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Android(系统 Noto CJK;TC 优先 —— 语料为繁中)
        "/system/fonts/NotoSansTC-Regular.otf",
        "/system/fonts/NotoSansCJK-Regular.ttc",
        "/system/fonts/NotoSansSC-Regular.otf",
        // Linux(Noto CJK 常见路径)
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ];
    CANDIDATES.iter().find_map(|p| std::fs::read(p).ok())
}

/// 窗口像素 → 逻辑坐标（**兼容保留**；新代码用 [`yuris_input::Viewport`]）。
///
/// **勘误（2026-10-17，成果 166）**：本函数旧实现用 `scale = max(sx, sy)`
/// （cover）且不加黑边偏移，与渲染用的 `letterbox_matrix`
/// （`scale = min(w/lw, h/lh)` + 居中偏移）**互逆关系不成立** ——
/// 窗口比例 ≠ 逻辑比例时点击会漂移。输入层已改用严格逆映射
/// （`yuris_input::Viewport::to_logical`）；本函数仅保留给旧调用方，
/// 语义改为与渲染一致的 letterbox 逆映射。
pub fn letterbox_logical(ww: u32, wh: u32, px: f64, py: f64) -> (i64, i64) {
    let vp = yuris_input::Viewport::new(ww, wh, logical_w(), logical_h());
    let p = vp.to_logical(px, py);
    (p.x, p.y)
}

/// Unix 秒 → "YYYY-MM-DD HH:MM"(civil 算法,无外部依赖;LOAD 存档行显示)。
fn fmt_unix_time(secs: u64) -> String {
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, mi) = (rem / 3600, (rem % 3600) / 60);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// VM UI 部件 z 分层带(P8.2b 第 3 步初值;实现选择,待引擎截图对拍):
/// 按钮族(路径含 `btn_`)= 95(台词之上,可点击);底框/提示族 = 70
/// (立绘之上、台词之下)。原生 Z 槽语义 Unknown,先按功能分带。
/// cgsys 层在**无 Z 槽**时的兜底档位。
///
/// **勘误(2026-10-17,成果 169)**:旧值 95/70 与实测档位**完全脱节** ——
/// PROGRESS 里所有 cgsys 层的真实 z 是 **998 / 1000 / 2010**:
/// - `BT.OFFES.CONFIG.TIP.BG=1=1` 首建 `Z=998`(成果 161);
/// - `BT.{OFF,OVER,ON,ONOV,NA,MASK}ES.CONFIG.WINDOW.*` `Z=1000`(f1 trace);
/// - `BT.*ES.*.BTN.*` `Z=2010`(成果 159 的 f190 trace)。
///
/// 70/95 的后果:凡是「CGEND 后重建、命令不带 Z」的界面件全部沉到
/// 游戏 CG(0 档)附近甚至文本层之下 —— 实机「按钮有时候跑到黑幕后面」。
/// 现按**实测档位带**取兜底;`btn_` 族归按钮带 2010,其余归窗口带 1000。
fn vm_ui_z_for_path(path_lower: &str) -> i64 {
    if path_lower.contains("btn_") {
        2010
    } else {
        1000
    }
}

/// VM UI 层过滤(P8.2b):允许全部 `cgsys\` 系统 UI 素材(游戏内按钮/消息
/// 窗/存档钮等分属 main/saveload/config 等子目录 —— 成果 82 勘误 7:
/// 仅限 main 会把 saveload 的 SAVE/LOAD/QS/QL 组过滤掉,参考图按钮组
/// 不全),deny:debug 覆盖层(成果 69 B3)、title/extra(内置标题与子画面
/// 接管,防双份)。标题等待期由 allow_vm_ui=false 整体关闭。
/// VM FILE 参数用 `/`(成果 66),归一为 `\` 再判。
fn vm_ui_layer_allowed(path_lower: &str, allow_vm_ui: bool, builtin_title: bool) -> bool {
    if !allow_vm_ui {
        return false; // 标题菜单等待期:内置标题接管视觉
    }
    // 别名根归一:`cgsys_c` 是运行期别名根(known-issues 2.4),
    // 否则 cgsys_c\main\pop 等状态件会绕过 deny(实机:右上 AUTO 药丸)。
    let norm = path_lower
        .replace('/', "\\")
        .replace("cgsys_c\\", "cgsys\\");
    if norm.contains(r"\debug\") {
        return false; // 引擎 debug 覆盖层
    }
    if !builtin_title {
        // **画面归属(黑屏勘误 2026-10-13,实机)**:下面这些 deny
        // (内置标题/EXTRA、pop/autoskipicon/skipicon/autoicon/count 状态件)
        // 全是为「播放器自己画标题、自己管按钮态」那一套服务的 —— 而那一套
        // **由 scenario 层驱动**。
        //
        // 没有明文剧本的作品(scenario 层空:**剧本以编译后的 yst*.ybn 存在包里**,
        // 由 VM 执行)时,内置标题永远不会出现;这些 deny 一旦留着,游戏的
        // **自己的标题画面**(`cgsys\title\`)就被拒 ⇒ 屏幕永远全黑
        // (实测:167 层全部建好又被 CGEND 正确清掉,之后一个上屏的都没有)。
        //
        // 此时把画面交还给游戏:只挡 debug 覆盖层,其余由游戏自己的 CG/CGEND
        // 事件决定显隐 —— 游戏的状态机就在我们正在执行的脚本里。
        return true;
    }
    if norm.contains(r"cgsys\title\") || norm.contains(r"cgsys\extra\") {
        return false; // 内置标题/EXTRA 子画面接管
    }
    // 状态件家族(P2-1,layout-fix-plan §2.4):pop/tip 药丸、AUTO+SKIP/
    // SKIP 图标、页码 count 图标在参考图常显面均不可见(仅悬停/激活时
    // 出现);状态机实装前默认隐藏,否则全量注册表重放会叠成药丸堆。
    if norm.contains(r"cgsys\main\pop\")
        || norm.contains(r"cgsys\main\autoskipicon\")
        || norm.contains(r"cgsys\main\skipicon\")
        || norm.contains(r"cgsys\main\autoicon\")
        || norm.contains(r"cgsys\main\count\")
    {
        return false;
    }
    norm.contains(r"cgsys\")
}

/// ADV 主按钮行(layout-fix-plan P2-2):参考图 1920×1080 模板匹配标定
/// (相关系数 0.73~0.99;HOME 0.44 待实机复核)。
/// 字段:(层 id, x, y, 宽, 高, 素材路径, 图集段数)。
/// 素材 = `cgsys\main\button\type1\btn_*.png` 横排图集,首段 = OFF。
pub const ADV_BUTTONS: &[(u64, f32, f32, u32, u32, &str, usize)] = &[
    (
        0x5C_9000_0001,
        1048.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_backlog_bt3",
        3,
    ),
    (
        0x5C_9000_0002,
        1148.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_auto_bt4",
        4,
    ),
    (
        0x5C_9000_0003,
        1248.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_skip_bt4n",
        5,
    ),
    (
        0x5C_9000_0004,
        1348.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_save_bt3n",
        4,
    ),
    (
        0x5C_9000_0005,
        1448.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_load_bt3n",
        4,
    ),
    (
        0x5C_9000_0006,
        1548.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_qsave_bt3n",
        4,
    ),
    (
        0x5C_9000_0007,
        1648.0,
        1040.0,
        114,
        39,
        "cgsys/main/button/type1/btn_qload_bt3n",
        4,
    ),
    (
        0x5C_9000_0008,
        1748.0,
        1040.0,
        69,
        39,
        "cgsys/main/button/type1/btn_volume_bt3",
        3,
    ),
    (
        0x5C_9000_0009,
        1803.0,
        1040.0,
        69,
        39,
        "cgsys/main/button/type1/btn_config_bt3",
        3,
    ),
    (
        0x5C_9000_000A,
        1856.0,
        1040.0,
        64,
        39,
        "cgsys/main/button/type1/btn_menulock_bt4",
        4,
    ),
    (
        0x5C_9000_000B,
        1882.0,
        952.0,
        31,
        31,
        "cgsys/main/button/type1/btn_title_bt3",
        3,
    ),
    (
        0x5C_9000_000C,
        1882.0,
        989.0,
        31,
        31,
        "cgsys/main/button/type1/btn_end_bt3",
        3,
    ),
];

/// CG 名 → 场景层基名(层 id = `fnv1a(基名)`)。
///
/// **通用化(2026-10-15,成果 132)**:本函数曾经把 v555 样本的按钮命名
/// `…BT.OFF/OVER/ON/ONOV/NA` 折叠成同一个基名(「同钮各态收敛一层」)——
/// 那是 **E 槽尚未被消费时期**的补丁:各态图都会上屏、只能靠"只留优先级
/// 最低者"压住。
///
/// 现在 E 语义已按手册实装(`E=0` → 层无效),引擎的真模型(REA
/// `docs/engine/render/02-layer-model.md` §1)是**每个 CG ID 各自一个
/// 持久图层对象、由 E 决定进不进绘制**,没有"同钮合并"这回事 ——
/// 折叠反而会让 `BT.NAES.*`(常态)/`BT.ONES.*`(按下)互相覆盖。
/// 故本函数现在 = 恒等映射,保留函数只为标注这处历史。
/// CGACT[SIZE] 的**源窗口 → 目标尺寸**采样(成果 153):
/// 产物 = `SX×SY` 的 RGBA;像素 (u,v) 按最近邻取源
/// `[x1 + u·(x2−x1+1)/sx, y1 + v·(y2−y1+1)/sy]`;**源越界 → 透明**
/// (官方 es.CGACT_SIZE 的部分出屏分支把负值窗口边界直接传进来,
/// 语义即"窗口可越过源位图,越界侧不上色")。
/// `// UNVERIFIED`:采样插值按最近邻 —— 引擎 CPU blit 的精确插值
/// (SIZE2 中画质档似为 2:1 平均,见效果实现区 SIMD 函数)未取证;
/// LOGO 主路径 = 1:1 全窗直拷,不受影响。
pub(crate) fn crop_scale_rgba(
    src: &[u8],
    sw: u32,
    sh: u32,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
    sx: i64,
    sy: i64,
) -> Option<(Vec<u8>, u32, u32)> {
    if sw == 0 || sh == 0 || sx <= 0 || sy <= 0 {
        return None;
    }
    let (dw, dh) = (sx as u32, sy as u32);
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let cw = (x2 - x1 + 1).max(1);
    let ch = (y2 - y1 + 1).max(1);
    let (swi, shi) = (sw as i64, sh as i64);
    for dy in 0..sy {
        let syy = y1 + dy * ch / sy;
        for dx in 0..sx {
            let sxx = x1 + dx * cw / sx;
            if sxx >= 0 && sxx < swi && syy >= 0 && syy < shi {
                let s = ((syy * swi + sxx) * 4) as usize;
                let d = ((dy * sx + dx) * 4) as usize;
                out[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
    }
    Some((out, dw, dh))
}

/// CGACT[SIZE] 产物纹理 id:目标层 / 源层换代号 / 窗口 / 尺寸的决定性哈希。
/// 与素材名 rid(同 `fnv1a(名)` 族)共用一个哈希空间;前缀字节 `CGSZ\0`
/// 隔开名空间。参数缺位时记 `i64::MIN`(稳态;缺位 → 真默认值回填发生在
/// 解码之后,同参数帧的 rid 不受影响)。
pub(crate) fn cgact_size_rid(dst_lid: u64, src_lid: u64, gen: u64, p: [i64; 6]) -> u64 {
    let mut b = b"CGSZ\x00".to_vec();
    b.extend_from_slice(&dst_lid.to_le_bytes());
    b.extend_from_slice(&src_lid.to_le_bytes());
    b.extend_from_slice(&gen.to_le_bytes());
    for v in p {
        b.extend_from_slice(&v.to_le_bytes());
    }
    fnv1a(&b)
}

/// 渲染台词/选择肢到 RGBA 白字(fontdue;按内容定宽,自动换行)。
/// 上限:200 字符 / 6 行(解码漂移产生的超长串会撑爆 canvas,实测踩坑)。
///
/// **槽池复用(成果 127)**:同文哈希命中 → 直接复用 rid(零栅格化/零上传,
/// 每帧重发的台词不再付 CPU/GPU 代价);新文 → 轮转写入 64 槽之一,
/// 旧台词纹理被覆写而非累积(修 `backend.images` 无界增长)。
/// rid 空间 = `0x5C_1000_0000 | 槽号`(旧方案是哈希尾,改槽号属实现选择;
/// 旧存档里引用旧 rid 的文本层只当"资源缺失"跳过,无碍)。
///
/// 自由函数形态(成果 158):Stage 宿主([`StageAssets`])与播放器选择肢
/// 共用;不再挂在 `PlayerCore` 上(Stage 文本路径没有 `&mut self`)。
pub(crate) fn rasterize_text(
    font: &fontdue::Font,
    text_slots: &mut [Option<(u64, ResourceId)>],
    text_seq: &mut usize,
    backend: Option<&mut WgpuBackend>,
    text: &str,
    px: f32,
) -> Option<ResourceId> {
    let text: String = text.chars().take(200).collect();
    let text_hash = fnv1a(text.as_bytes());
    if let Some((_, rid)) = text_slots.iter().flatten().find(|(h, _)| *h == text_hash) {
        return Some(*rid);
    }
    let slot = *text_seq % TEXT_SLOT_MAX;
    *text_seq = text_seq.wrapping_add(1);
    // 字号单位 = **游戏画布像素**(逻辑坐标),故不放缩(k≡1)。
    //
    // 勘误(成果 124):此处上一版写成 `min(logical_w()/1920, logical_h()/1080)`
    // = 0.573 —— 那是把「1920×1080 参考画布」的概念又套了一层,而本作的
    // 排版坐标本来就是 1100×825 画布像素 ⇒ 字号被缩到 23px(实机「字小」)。
    // 换行宽取画布 75%(本作 ≈825,与游戏自己的消息窗 `…MESW.TX` 宽 827 吻合)。
    // 引擎真值来自 FONT 命令(字号/字间/行间)—— 本内核尚未解析该子系统,
    // 故字号仍是**内核缺省值**(登记 U27)。
    // 字号 = **调用方给的游戏真值**(`FONT` 的 SX;成果 175)。
    // 调用方负责缺省与钳制,这里只按值排版。
    let px = px.max(6.0);
    let line_h = px * 1.3;
    let max_w = (logical_w() * 0.75).max(200.0);
    let max_rows = 6usize;
    // 打包行:每行 = (metrics, bitmap, xmin, ymin, advance)
    let mut rows: Vec<Vec<(usize, Vec<u8>, i32, i32, i32)>> = vec![Vec::new()];
    let mut row_w: Vec<i32> = vec![0];
    let mut cursor_x = 0i32;
    for ch in text.chars() {
        if ch == ' ' {
            cursor_x += 18;
            *row_w.last_mut().unwrap() = cursor_x;
            continue;
        }
        let (m, bmp) = font.rasterize(ch, px);
        if cursor_x + m.advance_width as i32 > max_w as i32 && rows.len() < max_rows {
            rows.push(Vec::new());
            row_w.push(0);
            cursor_x = 0;
        }
        if rows.len() > max_rows {
            break;
        }
        cursor_x += m.advance_width as i32;
        *row_w.last_mut().unwrap() = cursor_x;
        rows.last_mut()
            .unwrap()
            .push((m.width, bmp, m.xmin, m.ymin, m.advance_width as i32));
    }
    let width = (*row_w.iter().max().unwrap_or(&0)).clamp(60, max_w as i32) as usize;
    // fontdue Metrics.ymin 语义 = **从行顶(ascent 线)向下**的偏移
    // (实测 STHeiti 40px:'猫' ymin=-4 height=37,即字形顶略越行顶、
    // 底距行顶 33 < ascent 34.4 —— 若按基线解释会整字下坠 34px,
    // 底部全被 canvas 裁掉,即「文字只显示一半」)。因此字形位 =
    // 行顶 + ymin,canvas 高按行盒算,与基线无关。
    let (ascent, descent) = match font.horizontal_line_metrics(px) {
        Some(hm) => (hm.ascent, hm.descent.min(0.0)),
        None => (px * 0.9, -px * 0.15),
    };
    let pad_top = 4.0f32;
    let line_box = ((ascent - descent) as usize).max(1); // 行盒高(≈40)
    let height =
        (pad_top as usize * 2 + (rows.len().max(1) - 1) * line_h as usize + line_box).max(20);
    let mut canvas = vec![0u8; width * height * 4];
    for (ri, row) in rows.iter().enumerate() {
        let mut pen_x = 0i32;
        let row_top = pad_top as i32 + (ri as i32) * line_h as i32;
        for (w, bmp, xmin, ymin, adv) in row {
            for i in 0..bmp.len() {
                let a = bmp[i];
                if a < 8 {
                    continue;
                }
                let bx = xmin + (i % *w) as i32;
                let by = ymin + (i / *w) as i32;
                let cx = (pen_x + bx) as usize;
                let cy = (row_top + by) as i64;
                if cx >= width || cy < 0 || cy as usize >= height {
                    continue;
                }
                let o = (cy as usize * width + cx) * 4;
                canvas[o] = 255;
                canvas[o + 1] = 255;
                canvas[o + 2] = 255;
                canvas[o + 3] = a;
            }
            pen_x += *adv;
        }
    }
    let rid = ResourceId(0x5C_1000_0000 | slot as u64);
    text_slots[slot] = Some((text_hash, rid));
    let b = backend?;
    b.load_image_rgba(rid, &canvas, width as u32, height as u32)
        .ok()?;
    Some(rid)
}

fn vm_ui_base_prio(name: &[u8]) -> (Vec<u8>, u8) {
    (name.to_vec(), 0)
}

/// 旧折叠规则(v555 按钮态命名;已被 [`vm_ui_base_prio`] 的恒等映射取代)。
///
/// 保留实现与测试:若将来需要按**引擎版本 Profile** 恢复"同钮收敛"
/// (即遇确认过的旧引擎,其按钮态确实靠同址叠图而非 E),这里可以直接
/// 接回去 —— 但不得再无条件套用到所有作品。
#[allow(dead_code)]
fn vm_ui_base_prio_legacy(name: &[u8]) -> (Vec<u8>, u8) {
    let Some(pos) = name.windows(4).rposition(|w| w == b".BT.") else {
        return (name.to_vec(), 0);
    };
    let base = name[..pos].to_vec();
    let tok = &name[pos + 4..];
    let state_end = tok.iter().position(|&b| b == b'=').unwrap_or(tok.len());
    let prio = match &tok[..state_end] {
        b"OFF" => 0u8,
        b"ON" => 1,
        b"OVER" => 2,
        b"ONOV" => 3,
        b"NA" => 4,
        unknown => {
            // 状态令牌不在已确认集合 → 落在最低档。按钮三态可能因此
            // 叠放顺序不对,以前完全无提示。
            let tok_s = String::from_utf8_lossy(unknown).into_owned();
            yuris_log::alert_once(
                module_path!(),
                format!("ui-prio-token:{tok_s}"),
                format!(
                    "UI 按钮状态令牌未知,按最低优先级处理\n\
                     令牌: {tok_s:?}\n\
                     影响: 该状态图与其它状态图重叠时的 z 顺序可能不符引擎\n\
                     处置: 已确认集合 = OFF/ON/OVER/ONOV/NA"
                ),
            );
            5
        }
    };
    (base, prio)
}

/// 立绘角色键:名字前两段(`L_NYA_1A0100` → `L_NYA`)。
/// 同一角色的姿态/服装/表情变体共享键 → 引擎按**角色占一层**,
/// 变体切换 = 换图或换部件,而非叠新层(layout-fix-plan P0 勘误:
/// `L_NYA_1A0200` 实为 7KB 部件图,需叠加在基图上并随角色移动)。
fn tachie_char_key(name: &str) -> String {
    let mut it = name.split('_');
    match (it.next(), it.next()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => format!("{a}_{b}"),
        _ => {
            // 名字不带 `L_角色_…` 形态 ⇒ 退回整个名字当角色键:同角色的
            // 表情/姿态变体会被当成独立层叠加(实机表现为重影)。
            yuris_log::alert_once(
                module_path!(),
                format!("tachie-key:{name}"),
                format!(
                    "立绘名不符合 角色键 形态,按整名建层\n\
                     名字: {name}\n\
                     后果: 同角色的变体将各自成层(可能重影),而不是换图\n\
                     处置: 需要该作品的立绘命名表取证(期望 `<PREFIX>_<角色>_...`)"
                ),
            );
            name.to_string()
        }
    }
}

/// 按 id 取可改图层(走 [`yuris_scene::Scene::layer_mut`] 的 O(1) 索引;
/// 成果 124 之前是逐帧线性扫描)。
fn scene_mut_layer(scene: &mut yuris_scene::Scene, id: u64) -> Option<&mut Layer> {
    scene.layer_mut(id)
}

/// 背景显示变换(引擎实测模型;2026-09-12 原版截图对拍,corr 0.908):
/// - `scale = 1.25`(逻辑 px / 素材 px;2400×1200 背景实测值。可能是
///   `iw/1920` 或固定 1.25,样本全为 2400×1200 不可分,取 1.25);
/// - 可见素材区 = 1920/scale × 1080/scale;
/// - 区域中心 = 图中心 + `cam × 0.5`(实测偏移比 0.517/0.568,取 0.5),
///   并钳制在图内(不露黑边);
/// - 返回图层 `(x, y, scale)`(x/y = 可见区左上角的负逻辑坐标)。
/// 相机方向/系数等级 **Likely**(单点实测),实机截图复核后可微调。
fn bg_cover_transform(iw: u32, ih: u32, cam: (i64, i64, i64)) -> (f32, f32, f32) {
    let (iwf, ihf) = (iw.max(1) as f32, ih.max(1) as f32);
    // 引擎实测缩放;小图保底 cover(不露黑边)
    let s = 1.25f32.max(logical_w() / iwf).max(logical_h() / ihf);
    let (vw, vh) = (logical_w() / s, logical_h() / s);
    let (cx, cy, _cz) = cam;
    let mut x0 = (iwf - vw) / 2.0 + cx as f32 * 0.5;
    let mut y0 = (ihf - vh) / 2.0 + cy as f32 * 0.5;
    x0 = x0.clamp(0.0, (iwf - vw).max(0.0));
    y0 = y0.clamp(0.0, (ihf - vh).max(0.0));
    (-x0 * s, -y0 * s, s)
}

/// 三态按钮公共体(标题按钮与子画面按钮共用):按光标/按下帧计算每钮的
/// 目标态并切换层资源;返回 (层资源更新, 是否有悬停进入边沿)。
/// `_na` 灰化钮(active=false)不参与。
fn tri_state_pass(
    buttons: &mut [TitleButton],
    cursor: (i64, i64),
    clicked: bool,
) -> (Vec<(u64, ResourceId)>, bool) {
    let (cx, cy) = cursor;
    let mut updates: Vec<(u64, ResourceId)> = Vec::new();
    let mut hover_entered = false;
    for b in buttons {
        if !b.active {
            continue;
        }
        let [x, y, w, h] = b.rect;
        let hover = cx >= x as i64 && cx < (x + w) as i64 && cy >= y as i64 && cy < (y + h) as i64;
        if hover && !b.hovered {
            hover_entered = true;
        }
        b.hovered = hover;
        let want = if hover && clicked {
            TITLE_BTN_OVER
        } else if hover {
            TITLE_BTN_ON
        } else {
            TITLE_BTN_OFF
        };
        if want != b.shown {
            if let Some(rid) = b.rids[want] {
                b.shown = want;
                updates.push((b.id, rid));
            }
        }
    }
    (updates, hover_entered)
}

/// scenario 层资源/图层 id 基(与 SceneBridge 的 VM 层错开)。
pub const SC_BG: u64 = 0x5C_0000_0001;
/// 淡入淡出覆盖层 id。
pub const SC_FADE: u64 = 0x5C_0000_0002;
/// 台词窗文本层 id。
pub const SC_TEXT: u64 = 0x5C_0000_0003;
/// 台词窗框层 id。
pub const SC_WIN: u64 = 0x5C_0000_0004;
/// 崩坏级错误窗图层 id（最高 z；不参与 Stage 实体表）。
pub const SC_FATAL: u64 = 0x5C_0000_00FA;
/// 错误窗位图纹理 id（整张画布尺寸；seq 变化时覆写）。
pub const SC_FATAL_TEX: u64 = 0x5C_0000_00FB;

/// 淡入淡出覆盖层动画状态。
pub struct FadeState {
    /// 方向标志(由剧本 `FADEIN`/`FADEOUT` 语义给定;消费见 `PlayerCore::update_fade`)。
    pub out: bool,
    /// 起点(动画逻辑时钟口径,ms;见 `PlayerCore::anim_now`)。
    pub start: f64,
    /// 时长(ms)。
    pub ms: u64,
    /// 覆盖色 RGB。
    pub color: [u8; 3],
}

/// 立绘显示比例(× m_050 原生像素)。2026-09-12 原版引擎截图对拍实测:
/// 三角色(k_pen/l_nya/m_lop)一致 0.930/0.930/0.929(内容 16:9 标定,
/// corr 0.97~0.99)。layout-fix-plan P0。
pub const TACHIE_SCALE: f32 = 0.93;

/// 立绘循环上下浮动(实机:原版三玩偶持续上下浮动;仅 y, x 不动)。
/// 幅度/周期来自三帧原版静图估计(k_pen 顶缘摆幅 ≈39 逻辑 px、
/// l_nya ≈26,m_lop 样本不稳);周期无计时证据取 2.8s。
/// **UNVERIFIED(浮动参数)**:可用录屏计时后精确标定。
pub const TACHIE_FLOAT_AMP: f32 = 18.0;
/// 浮动周期(秒)。
pub const TACHIE_FLOAT_PERIOD: f32 = 2.8;

/// 浮动偏移(纯函数,可单测):正弦上下,相位逐角色不同。
fn tachie_float_dy(elapsed_s: f32, phase: f32) -> f32 {
    TACHIE_FLOAT_AMP * (elapsed_s * std::f32::consts::TAU / TACHIE_FLOAT_PERIOD + phase).sin()
}

/// 立绘浮动相位(按层 id 稳定派生;同角色每次运行一致)。
fn tachie_float_phase(id: u64) -> f32 {
    ((id % 997) as f32 / 997.0) * std::f32::consts::TAU
}

/// 标题按钮三态下标(素材后缀 `_off`/`_on`/`_over`;成果 78 图像实证)。
pub const TITLE_BTN_OFF: usize = 0;
/// 悬停/聚焦高亮态。
pub const TITLE_BTN_ON: usize = 1;
/// 按下态。
pub const TITLE_BTN_OVER: usize = 2;

/// 标题按钮悬停音(sysse/sse02)。es.BT.SE.SET 第一参数全语料 444 组恒定,
/// 时长 0.086s 显著短促(其余 0.30~0.39s);成果 80。
pub const TITLE_SE_HOVER: &str = "sse02";
/// 标题按钮决定音(sysse/sse03)。es.BT.SE.SET 第二参数(普通按钮 316 组);
/// BACK 型取消钮为 sse06(128 组)—— 参数 2 随按钮语境变化,标题六钮均普通型。
pub const TITLE_SE_DECIDE: &str = "sse03";
/// 取消/戻る音(sysse/sse06)。es.BT.SE.SET 第二参数的 BACK 型取值(成果 80)。
pub const TITLE_SE_CANCEL: &str = "sse06";

/// 子画面层 id 基(P3:LOAD / EXTRA / CG 鉴赏 / BGM 鉴赏 / END 确认;
/// 与标题 0x5C_7000_* 与台词窗 SC_* 错开;z 在 50..=61 段)。
pub const UI_LOAD_BASE: u64 = 0x5C_8000_0000;
/// EXTRA(鉴赏入口)子画面层 id 基。
pub const UI_EXTRA_BASE: u64 = 0x5C_9000_0000;
/// EXTRA CG 鉴赏子画面层 id 基。
pub const UI_CG_BASE: u64 = 0x5C_B000_0000;
/// BGM 鉴赏子画面层 id 基。
pub const UI_BGM_BASE: u64 = 0x5C_D000_0000;
/// END 确认对话框层 id 基。
pub const UI_CONFIRM_BASE: u64 = 0x5C_E000_0000;

/// 子画面路由钮 id(常量而非算术 —— match 模式需要)。
pub const UI_LOAD_BACK: u64 = UI_LOAD_BASE + 1;
/// CG 鉴赏:返回。
pub const UI_CG_BACK: u64 = UI_CG_BASE + 1;
/// CG 鉴赏:上一页。
pub const UI_CG_PREV: u64 = UI_CG_BASE + 0x90;
/// CG 鉴赏:下一页。
pub const UI_CG_NEXT: u64 = UI_CG_BASE + 0x91;
/// CG 鉴赏:切到 BGM 页签。
pub const UI_CG_TAB_BGM: u64 = UI_CG_BASE + 0xA1;
/// BGM 鉴赏:返回。
pub const UI_BGM_BACK: u64 = UI_BGM_BASE + 1;
/// BGM 鉴赏:上一页。
pub const UI_BGM_PREV: u64 = UI_BGM_BASE + 0x90;
/// BGM 鉴赏:下一页。
pub const UI_BGM_NEXT: u64 = UI_BGM_BASE + 0x91;
/// BGM 鉴赏:切到 CG 页签。
pub const UI_BGM_TAB_CG: u64 = UI_BGM_BASE + 0xA0;
/// END 确认:是。
pub const UI_CONFIRM_YES: u64 = UI_CONFIRM_BASE + 2;
/// END 确认:否。
pub const UI_CONFIRM_NO: u64 = UI_CONFIRM_BASE + 3;

/// 播放器子画面状态(P3)。均在 `Wait::TitleMenu` 等待下叠加显示,
/// 点击经 `poll_title_menu` 路由,scenario 播放器不感知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubUi {
    /// 无子画面(标题菜单本体)。
    None,
    /// LOAD 存档列表(back_load 整屏)。
    Load,
    /// EXTRA CG 鉴赏(EXTRA 直达,原生无落地菜单;cgmode/back 3×3 画格)。
    ExtraCg,
    /// EXTRA BGM 鉴赏(extra/back 双列列表 + 曲目点播)。
    ExtraBgm,
    /// END 确认对话框(dialog_end + yes/no)。
    ConfirmEnd,
}

/// 标题按钮:命中区 + 三态资源 + 当前显示态(成果 78;坐标/绑定成果 79)。
pub struct TitleButton {
    /// 命中区 rect = x,y,w,h(逻辑坐标,原生尺寸)。
    pub rect: [f32; 4],
    /// 按钮层 id(0x5C_7000_0005..)。
    pub id: u64,
    /// 三态资源 [off, on, over](title_screen 预载;加载失败 = None)。
    pub rids: [Option<ResourceId>; 3],
    /// 当前显示态(TITLE_BTN_*)。
    pub shown: usize,
    /// 可用性(false = `_na` 不可用态:单素材、无三态切换、点击无效 ——
    /// 原生 es.BT 把无存档 CONTINUE 注册为 BTN.LLOAD.NA 独立按钮,成果 79)。
    pub active: bool,
    /// 上一帧悬停态(进入命中区的边沿触发悬停音,成果 80)。
    pub hovered: bool,
}

/// 播放核心(实现 ScenarioHost;与 scenario 分裂借用)。
pub struct PlayerCore {
    /// 主封包索引(资源懒加载用)。
    pub index: Arc<PacFileIndex>,
    /// 组级 VM 实例(脚本执行主体)。
    pub vm: GroupVm,
    /// VM ↔ 场景投影桥(显隐/文本/层 z 同步)。
    pub bridge: SceneBridge,
    /// 渲染帧计数(`YURIS_LOG=1` 下的周期诊断用;与游戏逻辑无关)。
    pub debug_frames: u64,
    /// **渲染编排核心(M2,成果 155–158)**:全部 CG 族(含精灵族)与文本
    /// 事件经 Stage 指令翻译上屏;层状态/文本窗态唯一归属 = Stage 实体表。
    /// 规格 = `docs/engine/stage-core.md`。
    pub stage: yuris_stage::Stage,
    /// 整窗截图的每目标拍数(StageAssets::snapshot 的轮转槽序号)。
    pub snap_seq: HashMap<u64, u64>,
    /// **层 id → 最近一次已知 z**(跨 CGEND 存活)。
    ///
    /// 为什么需要独立于 VM 的 CG 注册表:引擎 CGEND 会销毁对象(注册表条目
    /// 随之消失),但**同名层之后再被 `CG[ID=… FILE=…]` 重建且不带 Z 槽**时,
    /// 引擎侧的 z 仍取该对象的既定层序 —— 它是层属性,不是一次性参数。
    /// 我们的 `vm.cg_z()` 在 CGEND 后查不到 ⇒ fallback 落 0 ⇒ 层沉底/错位
    /// (实测:开屏 環境設定 页 z998→0)。本表在投影时记录,永不因 CGEND 清除。
    pub layer_z_memo: HashMap<u64, i64>,
    /// 已上传过纹理的资源 id(避免重复上传)。
    pub loaded: HashSet<u64>,
    /// 渲染后端(`None` = 尚未建窗或初始化失败)。
    pub backend: Option<WgpuBackend>,
    /// 已消费的 vm.events() 游标。
    pub events_cursor: usize,
    /// **输入层（P9.2）**：引擎语义的键/鼠标/手柄状态机。
    ///
    /// 平台壳把原始事件翻译成 `yuris_input::PlatformEvent` 推进来；
    /// [`Player::tick`] 每帧 `tick()` 一次并生成 VM 快照。
    /// `cursor_logical` / `clicked` / `frame_clicked` 仍是**派生缓存**：
    /// 测试可直接写它们，输入层有事件时以输入层为准。
    pub input: yuris_input::InputState,
    /// 触摸适配器（Android/wasm；桌面壳也可用）。
    pub input_touch: yuris_input::TouchAdapter,
    /// 光标逻辑坐标(@133/@138 注入)。
    pub cursor_logical: (i64, i64),
    /// 按键脉冲:注入点击/按键置起,主循环按帧计数并回写 VM 按键态。
    pub key_pulse: bool,
    /// 脉冲已持续的帧数(超过 120 帧清键)。
    pub key_pulse_frames: u32,
    /// 系统 CJK 字体(台词与 UI 文本栅格化用)。
    pub font: fontdue::Font,
    /// 当前淡入淡出覆盖层(`None` = 无)。
    pub fade: Option<FadeState>,
    /// 本帧是否有点击(派生缓存;输入层有事件时以输入层为准)。
    pub clicked: bool,
    /// scenario 已生成的精灵层(id)。
    pub sprites: Vec<u64>,
    /// 精灵淡入/淡出动画:(id, 开始, ms, 淡出?)。
    pub sprite_fades: Vec<(u64, f64, u64, bool)>,
    /// 台词纹理槽池(成果 127):定长轮转;`None` = 空槽。
    ///
    /// 旧实现每句台词一个新 rid 的新纹理,`backend.images` **只增不减**
    /// —— 长跑几百句后 GPU 内存被死台词纹理吃满(实机「越玩越卡」候选根因),
    /// 且每帧重发的同一句台词会**重复栅格化+上传**。现在:64 槽轮转 +
    /// 同文哈希命中直接复用(零栅格化/零上传)。
    pub text_slots: Vec<Option<(u64, ResourceId)>>,
    /// 台词槽轮转游标。
    pub text_slot_seq: usize,
    /// 立绘位移动画(`\T` 8 参):(id, 开始, ms, 起点, 终点)。
    pub sprite_moves: Vec<(u64, f64, u64, (f32, f32), (f32, f32))>,
    /// 立绘循环浮动:id → (相位, 基准 x/y;浮动叠加上去)。
    pub tachie_float: std::collections::HashMap<u64, (f32, (f32, f32))>,
    /// 浮动动画时钟起点(进程内单调)。
    pub float_clock: Instant,
    /// 音频包索引 + 播放器(P9.1)。
    pub audio_packs: Arc<PacFileIndex>,
    /// 音频后端(SE/BGM;P9.1)。
    pub audio: crate::audio::Audio,
    /// 活动选择肢(文本 + 命中区)。
    pub choices: Option<Vec<(String, [f32; 4])>>,
    /// 最近 CursorMoved 原始像素(winit 0.30 无 cursor_position 轮询)。
    pub last_cursor_px: Option<(f64, f64)>,
    /// 本帧是否发生过点击(防同一次点击推进两帧)。
    pub frame_clicked: bool,
    /// 标题按钮(命中区 + 三态资源;成果 78)。
    pub title_buttons: Vec<TitleButton>,
    /// 标题 LOAD/LASTLOAD 请求(tick 内置,Player::tick 消费走快读)。
    pub request_title_load: bool,
    /// 标题 END 请求(主循环消费后退出)。
    pub request_quit: bool,
    /// 全局槽(\GO.G.IF 的 G=n;仿真内部存储 —— 引擎真值映射 Unknown/B5。
    /// 成果 78 勘误:原 `@50[n]` 映射被运行时证伪,@50 dims=[1],idx≥1 越界)。
    pub globals: HashMap<usize, i64>,
    /// 子画面状态(P3;close_subui 复位)。
    pub subui: SubUi,
    /// 子画面三态按钮(戻る/yes/no/标签页;悬停边沿播悬停音)。
    pub ui_buttons: Vec<TitleButton>,
    /// LOAD:存档条目(路径,显示标签;mtime 排序,新在前)。
    pub save_entries: Vec<(std::path::PathBuf, String)>,
    /// LOAD 选中条目(Player::tick 消费走 restore_from)。
    pub request_load_path: Option<std::path::PathBuf>,
    /// EXTRA CG:ev 清单(剥根规范名)与页码、正在查看的全图层。
    pub ev_list: Vec<String>,
    /// EXTRA CG 当前页码。
    pub cg_page: usize,
    /// 正在查看的全图层资源(`None` = 停在列表)。
    pub cg_view: Option<ResourceId>,
    /// EXTRA BGM:曲目名(去目录/扩展名)与列表页码。
    pub bgm_tracks: Vec<String>,
    /// EXTRA BGM 当前页码。
    pub bgm_page: usize,
    /// 已进入过标题画面(P8.2b 门控:厂商 CG 阶段不建 VM UI 层)。
    pub title_seen: bool,
    /// **`Unsupported` 事件的首次告警去重集**(文本 → 已报过)。
    ///
    /// VM 侧 `note_unsupported` 已经 warn 过 strict 挂起路径;这里兜底的是
    /// lenient 模式(记事件继续)与遗漏的直推点:事件流里的「未实现」不再
    /// 一片静默。集合设 512 封顶(超限不再加 = 宁可漏报不涨内存)。
    pub unsup_note_seen: std::collections::HashSet<String>,
    /// 背景相机位移(`\BG.CMXYZ(x,y,z)`;layout-fix-plan P1)。
    /// 应用模型:`bg.x = (1920-iw)/2 - cam.x`(Likely,实机对拍校准)。
    pub bg_cam: (i64, i64, i64),
    /// ADV 主按钮行(参考图标定;layout-fix-plan P2-2)。
    pub adv_buttons: Vec<TitleButton>,
    /// `\WINDOWMODE(n)` 最近值(语义 Unknown,仅记录 + 日志)。
    pub window_mode: u8,
    /// 上一帧 VM UI 门控值(false→true 沿触发注册表重放)。
    pub vm_ui_allowed_prev: bool,
    /// **动画逻辑时钟(成果 172)**:淡变/位移/浮动的基准。
    ///
    /// 与 VM 的逻辑时钟同源(每 tick +1 帧 → ms),使整个呈现层的时间轴
    /// **只依赖帧号**,不依赖渲染耗时。缺省启用;`YURIS_WALL_CLOCK=1` 回退。
    pub anim_epoch: std::time::Instant,
    /// 已推进的动画帧数(逻辑时钟口径)。
    pub anim_frames: u64,
    /// **动画时钟的量化余量(毫秒)**。
    ///
    /// `anim_now()` 的粒度为 1/60 秒,而真实帧距通常不是它的整数倍
    /// (实测 18.9ms)。余量累积使长期平均严格等于真实时间,不产生
    /// 「每帧少算一点 ⇒ 动画整体慢 12%」的系统性漂移。
    pub anim_debt_ms: f64,
    /// 单帧 VM 墙钟预算(缺省 None = 跑到自然让出,引擎真值;
    /// `YURIS_FRAME_BUDGET_MS=<毫秒>` 显式开启)。
    pub frame_budget: Option<std::time::Duration>,
    /// 崩坏级错误窗的当前排版状态（`None` = 无错误窗）。
    pub fatal_overlay: Option<FatalOverlayState>,
    /// **错误窗滚动量**（物理像素；由输入层修改，`update_fatal_overlay` 消费）。
    ///
    /// 单独存放的理由：滚动是输入事件驱动的，而重排（几 MB 的 RGBA + 纹理
    /// 上传）必须收敛到帧边界；放在这里就能「输入只改数字、帧内最多重排一次」。
    pub fatal_scroll: i32,
    /// 上次排版得到的可滚动上限（物理像素；0 = 一屏放得下）。
    pub fatal_max_scroll: i32,
    /// 游戏安装目录(封包与 `save/` 路径基准)。
    #[allow(dead_code)]
    pub game_dir: std::path::PathBuf,
}

/// 崩坏级错误窗的当前排版状态。
///
/// 记录**已应用**的参数（报告序号 / 输出缩放 / 滚动量）：任一变化才重排。
/// 滚动时位图必须整张重画（文字是烧进 RGBA 的），所以这里也兼任去重键。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FatalOverlayState {
    /// 报告序号（首因；后续致命记录只追加 tail，不改 seq）。
    pub seq: u64,
    /// 位图纹理 id。
    pub rid: ResourceId,
    /// 已应用滚动量（物理像素）。
    pub scroll: i32,
    /// 输出缩放（物理 / 逻辑）。
    pub scale: f32,
}

/// 台词纹理槽池大小(成果 127)。64 槽 × ~825×300×4B ≈ 60MB 上限;
/// 超过 64 句未复用的旧台词纹理被轮转覆写(滚回不渲染,无可见影响)。
pub const TEXT_SLOT_MAX: usize = 64;

/// 台词文本层的 z(定义在 `yuris-stage`;此处转出方便调用方引用)。
pub use yuris_stage::TEXT_Z;

/// **已退役**(2026-10-17,成果 168):引擎命令泵无条数预算
/// (`FUN_00404388` 泵到处理器让出为止,Confirmed);旧实现用 300,000 组
/// 单帧硬上限「保证本帧结束」,但在 boot 密集段会**人为切批** —— 语义偏离。
///
/// 现在帧结束由**脚本自己**决定:`run_frame_chain` 跑到任务链自然让出 +
/// 无 TASK 续帧。工控护栏只剩显式的 `YURIS_TASK_PUMP_CAP`(缺省关)。
/// 常量保留供既有报告引用,新代码不得再把它传给调度器。
#[deprecated(note = "命令泵无预算(官方 FUN_00404388);帧边界由 run_frame_chain 决定")]
pub const FRAME_GROUP_BUDGET: usize = 300_000;

/// 单帧 VM 墙钟预算(stag 架构勘误,2026-10-16):**缺省关闭**。
///
/// 引擎帧循环 = 「任务跑到自然让出(WAIT)→ 帧呈」;一段无 WAIT 的编排
/// (例:es.SS.SHOT 的建层→截屏→盖屏→揭幕宏序)在引擎里是**同帧原子**的。
/// 旧 8ms 墙钟切块把它摊到多帧,把中间态直接泄露给呈现 —— 实测(config
/// 注册表预热页抢屏、boot 白点、揭幕出黑)全源于此。缺省 = 无墙钟上限,
/// 组数上限 [`FRAME_GROUP_BUDGET`] 独挑失控保护;需要做性能对拍时可用
/// `YURIS_FRAME_BUDGET_MS=<毫秒>` 显式打开。
pub fn frame_budget_from_env() -> Option<std::time::Duration> {
    std::env::var("YURIS_FRAME_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(std::time::Duration::from_millis)
}

// ---------------------------------------------------------------------------
// M2 Stage 翻译层(成果 155–158;规格 docs/engine/stage-core.md)
// **全部** CG 族事件(含精灵族 SPL/DXBUF/CP*)-> yuris_stage::Command;
// 批 = VM 自然让出段,呈现 = 批提交后。旧手工 CG 消费段已整体退役
// (2026-10-16,M2):层状态唯一归属 = Stage 实体表。
// ---------------------------------------------------------------------------

/// 消息窗载体判定(成果 158,语料实证;Likely):本作消息窗 = 游戏自己的图层族
/// —— 名含 `MESW`(`BT.*ES.GAMEMAIN.TIP.MESW.{TX,A}`)或 FILE 含
/// `meswindow`(`cgsys/main/tip_meswindow_{txspace,back}`,1350×200 文本区)。
/// 其 E 槽翻转即窗口开合;`WORD`/`TEXT` 文本只在窗开时进景(引擎侧文字画在
/// 窗口子系统里,窗口不可见时整族不可见 ⇒ 原版 boot 无白点)。
fn is_message_window_carrier(name: &str, file: Option<&[u8]>) -> bool {
    if name.to_ascii_uppercase().contains("MESW") {
        return true;
    }
    file.map(|f| {
        String::from_utf8_lossy(f)
            .to_ascii_lowercase()
            .contains("meswindow")
    })
    .unwrap_or(false)
}

/// CG 族事件流水(`YURIS_TRACE_CG=<子串>` 门控;`1` = 全量)。诊断工具,
/// 不携带任何行为:打印事件名/FILE/E/Z/A 原值,供帧序对拍。
/// 批序号 = consume_events 的调用次(≈引擎呈现帧,VM 让出段)。
static CG_TRACE_FILTER: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
static CG_TRACE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn cg_trace_frame() -> u64 {
    CG_TRACE_SEQ.load(std::sync::atomic::Ordering::Relaxed)
}

fn cg_trace_enabled(name: &str) -> bool {
    CG_TRACE_FILTER
        .get_or_init(|| {
            std::env::var("YURIS_TRACE_CG")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|p| {
                    if p == "1" {
                        // 全量 = 空表(恒中)
                        Vec::new()
                    } else {
                        // 逗号分隔子串族,任一命中即显示(分层取证用)
                        p.split(',')
                            .map(|x| x.trim().to_ascii_lowercase())
                            .collect()
                    }
                })
        })
        .as_ref()
        .map(|pats| {
            pats.is_empty() || pats.iter().any(|p| name.to_ascii_lowercase().contains(p))
        })
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn stage_cg_trace(
    kind: &str,
    name: &str,
    file: &Option<Vec<u8>>,
    e: Option<i64>,
    z: Option<i64>,
    a: Option<i64>,
    sx: Option<i64>,
    sy: Option<i64>,
    x: Option<i64>,
    y: Option<i64>,
    pc: usize,
    script_id: u16,
) {
    if !cg_trace_enabled(name) {
        return;
    }
    let file_txt = file
        .as_ref()
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .unwrap_or_default();
    yuris_log::trace_forced!(
        "[cg-trace f{}] {kind} s{script_id} pc{pc} {name} file=\"{file_txt}\" E={:?} Z={:?} A={:?} SX={:?} SY={:?} X={:?} Y={:?}",
        cg_trace_frame(),
        e,
        z,
        a,
        sx,
        sy,
        x,
        y
    );
}

fn stage_cgact_trace(name: &str, effects: &[(u8, String, yuris_vm::EffectValue)]) {
    if !cg_trace_enabled(name) {
        return;
    }
    let parts: Vec<String> = effects
        .iter()
        .map(|(n, _, v)| format!("{n}={:?}", v))
        .collect();
    yuris_log::trace_forced!("[cg-trace f{}] CGACT {name} {}", cg_trace_frame(), parts.join(" "));
}

/// 标题按钮层名 → `@1035` 的选择值。
///
/// 映射依据 = `s114` 的 `es.BT.SET("BTN.<名>", n)` 第三参(脚本侧真值)。
/// 本内核只需「**不是 -1**」即可让 `s58 g31` 成立并进入游戏,
/// 故非 START 族统一给 2;精确路由待 es.BT 出队链取证。
fn title_button_slot(layer_name: &str) -> Option<i64> {
    let up = layer_name.to_ascii_uppercase();
    if up.contains("BTN.START") {
        Some(1)
    } else if up.contains("BTN.LOAD")
        || up.contains("BTN.CGMODE")
        || up.contains("BTN.RPMODE")
        || up.contains("BTN.CONFIG")
        || up.contains("BTN.END")
    {
        Some(2)
    } else {
        // **未知按钮名**:点击会被吞掉(@1035 保持 -1,脚本门控永不成立)。
        // 以前完全静默;现在按按钮名去重打横幅。
        yuris_log::alert_once(
            module_path!(),
            format!("title-button:{up}"),
            format!(
                "标题/系统栏按钮未映射\n\
                 按钮层: {layer_name}\n\
                 后果: 该点击被丢弃(@1035 不会更新,对应脚本分支不触发)\n\
                 处置: 在 title_button_slot 白名单补映射(需要脚本侧 es.BT 第三认真值)"
            ),
        );
        None
    }
}

/// 文本字号的**缺省值**与取值规则。
///
/// **取证(2026-10-17,成果 175)**:字号真值 = 游戏自身 `FONT` 命令的
/// `SX/SY`(YSCM 12 参,语料实证 12/12、14/15);脚本没设过时才用缺省。
/// `YURIS_TEXT_PX` 保留为**诊断覆盖**(逐作品标定/对拍用)。
pub(crate) fn text_px_or_default(px: Option<(i64, i64)>) -> f32 {
    if let Some(v) = std::env::var("YURIS_TEXT_PX")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| *v >= 4.0)
    {
        return v;
    }
    match px {
        Some((sx, _sy)) if sx > 0 => sx as f32,
        // 与 VM 侧 `FONTINFO SX/SY` 查询共用同一常量:脚本的
        // 「暂存字号 → 改 → 还原」惯用法依赖查询值 = 渲染生效值。
        _ => yuris_vm::DEFAULT_FONT_PX as f32,
    }
}

/// Stage 的像素服务宿主(老纹理路径/缓存全部复用;Stage 只裁决归属与时序)。
struct StageAssets<'a> {
    index: &'a PacFileIndex,
    backend: Option<&'a mut WgpuBackend>,
    loaded: &'a mut HashSet<u64>,
    font: &'a fontdue::Font,
    /// 当前字号(`FONT` SX;`None` = 脚本未设置 → 内核缺省)。
    text_px: Option<(i64, i64)>,
    text_slots: &'a mut Vec<Option<(u64, ResourceId)>>,
    text_seq: &'a mut usize,
    /// 整窗截图**每拍必须取新内容** —— 产物 rid 按"该目标第几次拍"轮转
    /// (8 槽),否则第二次以后的 COPY2 全拿到第一张旧图(实测后果:
    /// boot 黑幕采到空场景 ⇒ 透明,底下的 config 页穿帮)。
    snap_seq: &'a mut HashMap<u64, u64>,
    /// 填色产物纹理 id 的轮转序号(见 [`StageAssets::upload_rgba`])。
    paint_seq: u64,
}

impl<'a> yuris_stage::Assets for StageAssets<'a> {
    fn resolve(&mut self, file: &[u8], owner: &[u8]) -> Option<TexId> {
        let rid = fnv1a(owner);
        if self.loaded.contains(&rid) {
            return Some(rid);
        }
        let Some(data) = self.index.read_image_bytes_raw(file) else {
            return None;
        };
        let Some(b) = self.backend.as_deref_mut() else {
            return None;
        };
        match b.load_image(ResourceId(rid), &data) {
            Ok(()) => {
                self.loaded.insert(rid);
                Some(rid)
            }
            Err(e) => {
                yuris_log::warn!("[stage] 纹理上传失败 rid={rid:#x}: {e}");
                None
            }
        }
    }

    /// 纹理解析失败原因(转 [`PacFileIndex::explain_image_miss`])。
    fn explain_miss(&self, file: &[u8]) -> Option<String> {
        Some(self.index.explain_image_miss(file).describe())
    }

    fn solid(&mut self) -> TexId {
        // 1×1 黑(FILE="";手册示例[3])。稳定 rid 与文本产物同族前缀隔开。
        let rid = ResourceId(0x5C_4_0000_0000);
        if !self.loaded.contains(&rid.0) {
            if let Some(b) = self.backend.as_deref_mut() {
                if let Err(e) = b.load_image_rgba(rid, &[0, 0, 0, 255], 1, 1) {
                    yuris_log::warn!("[stage] 1×1 黑上传失败 rid={:#x}: {e}", rid.0);
                    return 0;
                }
            } else {
                return 0;
            }
            self.loaded.insert(rid.0);
        }
        rid.0
    }

    fn blit(
        &mut self,
        src_file: &[u8],
        src: LayerId,
        params: [i64; 6],
        gen: u64,
        target: LayerId,
    ) -> Option<TexId> {
        // 产物 rid 与旧 CGACT[SIZE] 同式稳定:同参数重复帧只上传一次。
        let rid = cgact_size_rid(target, src, gen, params);
        if self.loaded.contains(&rid) {
            return Some(rid);
        }
        let [x1, y1, x2m, y2m, sxm, sym] = params;
        let Some(raw) = self.index.read_image_bytes_raw(src_file) else {
            yuris_log::trace_forced!(
                "[blit] read_image FAILED file={:?}",
                String::from_utf8_lossy(src_file)
            );
            return None;
        };
        let Ok(img) = image::load_from_memory(&raw) else {
            yuris_log::trace_forced!(
                "[blit] decode FAILED file={:?} bytes={}",
                String::from_utf8_lossy(src_file),
                raw.len()
            );
            return None;
        };
        let img = img.to_rgba8();
        let (sw, sh) = (img.width() as i64, img.height() as i64);
        let x2 = if x2m != i64::MIN { x2m } else { sw - 1 };
        let y2c = if y2m != i64::MIN { y2m } else { sh - 1 };
        let sxc = if sxm != i64::MIN { sxm } else { x2 - x1 + 1 };
        let syc = if sym != i64::MIN { sym } else { y2c - y1 + 1 };
        if x2 < x1 || y2c < y1 || sxc <= 0 || syc <= 0 {
            return None;
        }
        let Some((rgba, w, h)) = crop_scale_rgba(
            img.as_raw(),
            img.width(),
            img.height(),
            x1,
            y1,
            x2,
            y2c,
            sxc,
            syc,
        ) else {
            return None;
        };
        let Some(b) = self.backend.as_deref_mut() else {
            return None;
        };
        match b.load_image_rgba(ResourceId(rid), &rgba, w, h) {
            Ok(()) => {
                self.loaded.insert(rid);
                Some(rid)
            }
            Err(e) => {
                yuris_log::warn!("[stage] BLIT 纹理上传失败 rid={rid:#x}: {e}");
                None
            }
        }
    }

    /// 整窗截图(**带字节**):`CGACT[RECTPAINT*]` 要改写位图,必须能拿到
    /// 像素。这里与 `snapshot` 同源(每次重新合成 + 上传),额外把字节返回。
    fn snapshot_ex(
        &mut self,
        view: &Scene,
        target: LayerId,
    ) -> Option<(TexId, Vec<u8>, u32, u32)> {
        let seq = {
            let e = self.snap_seq.entry(target).or_insert(0);
            *e = e.wrapping_add(1);
            *e
        };
        let rid = 0x5C53_4E41_5000_0000
            | ((seq & 0x7) << 40)
            | (target & 0xFFFF_FFFF);
        let b = self.backend.as_deref_mut()?;
        let (rgba, w, h) = b.render_scene_to_rgba(view).ok()?;
        match b.load_image_rgba(ResourceId(rid), &rgba, w, h) {
            Ok(()) => {
                self.loaded.insert(rid);
                Some((rid, rgba, w, h))
            }
            Err(e) => {
                yuris_log::warn!("[stage] 截图上传失败 {target:#x}: {e}");
                None
            }
        }
    }

    /// 解码 FILE 图像为 RGBA(填色改写位图用)。按文件字节寻址,不碰纹理。
    fn decode_rgba(&mut self, file: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
        let raw = self.index.read_image_bytes_raw(file)?;
        let img = image::load_from_memory(&raw).ok()?.to_rgba8();
        let (w, h) = (img.width(), img.height());
        Some((img.into_raw(), w, h))
    }

    /// 上传改写后的像素(填色产物)。
    ///
    /// **每次都必须真上传**:内容是"上一次截屏 + 通道覆盖",逐帧都不同;
    /// 若按内容长度/尺寸派生成 rid 并命中缓存,就会永远返回第一张
    /// (实机后果:全屏缓冲停在黑帧,标题背景全黑且动画消失)。
    /// 因此按单调序号轮转 rid(64 槽,远超同时在场的填色层数)。
    fn upload_rgba(&mut self, rgba: &[u8], w: u32, h: u32) -> Option<TexId> {
        let seq = {
            self.paint_seq = self.paint_seq.wrapping_add(1);
            self.paint_seq
        };
        let rid = 0x5041_494E_5400_0000 | ((seq & 0x3F) << 40);
        let b = self.backend.as_deref_mut()?;
        match b.load_image_rgba(ResourceId(rid), rgba, w, h) {
            Ok(()) => {
                self.loaded.insert(rid);
                Some(rid)
            }
            Err(e) => {
                yuris_log::warn!("[stage] 填色纹理上传失败 {rid:#x}: {e}");
                None
            }
        }
    }

    fn snapshot(&mut self, view: &Scene, target: LayerId) -> Option<TexId> {
        // 第几次拍 → 轮转槽(8 槽;槽数只是内存上界)。每次拍都重新合成 +
        // 上传(= 引擎"当前画面"语义),绝不返回旧图。
        let seq = {
            let e = self.snap_seq.entry(target).or_insert(0);
            *e = e.wrapping_add(1);
            *e
        };
        let rid = 0x5C53_4E41_5000_0000
            | ((seq & 0x7) << 40)
            | (target & 0xFFFF_FFFF);
        let Some(b) = self.backend.as_deref_mut() else {
            return None;
        };
        let dumped = match b.render_scene_to_rgba(view) {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        };
        // 截屏取证(`YURIS_DUMP_SNAP=1`):把每次 COPY2 取到的像素落盘。
        // 「logo 淡出到黑」的直接判据 = 这个文件是黑还是含 logo。
        if std::env::var_os("YURIS_DUMP_SNAP").is_some() {
            if let Ok((rgba, w, h)) = &dumped {
                let dir = std::path::PathBuf::from(
                    std::env::var("TEMP").unwrap_or_else(|_| ".".into()),
                )
                .join("snap");
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join(format!("snap_{seq:03}_{target:#x}.png"));
                if let Some(img) = image::RgbaImage::from_raw(*w, *h, rgba.clone()) {
                    let _ = img.save(&path);
                    yuris_log::trace_forced!("[snap] {path:?} ({w}x{h})");
                }
            }
        }
        match dumped {
            Ok((rgba, w, h)) => match b.load_image_rgba(ResourceId(rid), &rgba, w, h) {
                Ok(()) => {
                    self.loaded.insert(rid);
                    Some(rid)
                }
                Err(e) => {
                    yuris_log::warn!("[stage] 截图上传失败 {target:#x}: {e}");
                    None
                }
            },
            Err(e) => {
                yuris_log::warn!("[stage] 整窗截图合成失败: {e}");
                None
            }
        }
    }

    fn text(&mut self, s: &str) -> Option<(TexId, u32, u32)> {
        let rid = rasterize_text(
            self.font,
            self.text_slots,
            self.text_seq,
            self.backend.as_deref_mut(),
            s,
            text_px_or_default(self.text_px),
        )?;
        let (w, h) = self.backend.as_deref().and_then(|b| b.image_size(rid.0))?;
        Some((rid.0, w, h))
    }
}

impl PlayerCore {
    /// 动画层的「现在」(毫秒)。
    ///
    /// **= 真实经过时间(墙钟)**。淡变/立绘位移/浮动/过场全部读它,
    /// 因此 `FADE MS=500` 就是 500ms 真实时间,与帧率完全解耦:
    /// 30fps 掉帧时动画照样在同一时刻走完,不会「整体变慢」。
    ///
    /// **勘误(2026-10-17,成果 184)**:一度改成 `anim_frames × 1000/60`
    /// (帧量化),理由是「确定性」。但帧量化在帧率 ≠ 目标帧率时会产生
    /// **系统性时间漂移**:实测帧距 18.9ms 而一格算 16.67ms ⇒ 动画按
    /// 53 帧/秒推进,整体慢 12%,观感「卡顿/不顺」。真实时间才是引擎
    /// 语义(`timeGetTime` 派生),帧量化只是取证期的权宜。
    ///
    /// 需要**逐帧可复现**的取证场景(`--shot` 三跑同 SHA)用
    /// `YURIS_FRAME_ANIM=1` 切回帧量化。
    fn anim_now(&self) -> f64 {
        if std::env::var_os("YURIS_FRAME_ANIM").is_some() {
            return self.anim_frames as f64 * 1000.0 / 60.0;
        }
        self.anim_epoch.elapsed().as_secs_f64() * 1000.0
    }

    /// 推进动画时钟(每 tick 一次,先于更新)。
    pub fn advance_anim_clock(&mut self) {
        self.anim_frames = self.anim_frames.wrapping_add(1);
    }

    /// 按真实经过时间推进动画时钟(步数;与 `advance_frame_clock_by` 同源)。
    pub fn advance_anim_clock_by(&mut self, steps: u32) {
        self.anim_frames = self.anim_frames.wrapping_add(steps.max(1) as u64);
    }

    /// 消费 VM 事件流 → 场景;装载新 CG 图像。
    /// P8.2b(成果 82):VM CG 通道接入场景 —— 游戏内 UI(es.BT.* 部件)
    /// 经此上屏;debug 覆盖层与标题等待期过滤(详见 docs/in-game-ui-plan.md)。

    /// **M2 事件翻译(成果 158)**:**全部** CG 族事件 → Stage 指令(精灵族
    /// 不分家,2026-10-16 收敛)。政策门控(`:/W1-9/` 辅助窗、`cgsys2` 调试
    /// 素材、debug 覆盖层、`vm_ui_layer_allowed` 标题期/denylist)在此单点
    /// 执行;Stage 归属:CG 建/patch、CGEND=Kill、SIZE 搬运、:/W0/ 截屏、
    /// COPY 别名、**消息窗开合**。翻译产出**无 legacy 集**——旧手工 CG 消费段
    /// 已整体退役。
    fn translate_stage_frame(
        &mut self,
        fresh: &[yuris_vm::VmEvent],
        allow_vm_ui: bool,
        builtin_title: bool,
        cmds: &mut Vec<yuris_stage::Command>,
    ) {
        let dbg_visuals = std::env::var("YURIS_DEBUG_VISUALS").is_ok();
        for ev in fresh.iter() {
            match ev {
                yuris_vm::VmEvent::Cg {
                    id,
                    file,
                    e,
                    x,
                    y,
                    z,
                    a,
                    mode,
                    t,
                    tid,
                    ta,
                    mid,
                    sx,
                    sy,
                    pc,
                    script_id,
                    ..
                } => {
                    let Some(name) = id else { continue };
                    stage_cg_trace(
                        "CG",
                        name,
                        file,
                        *e,
                        *z,
                        *a,
                        *sx,
                        *sy,
                        *x,
                        *y,
                        *pc,
                        *script_id,
                    );
                    let win_aux = {
                        let b = name.as_bytes();
                        b.len() >= 4
                            && b[0] == b':'
                            && b[1] == b'/'
                            && b[2] == b'W'
                            && (b'1'..=b'9').contains(&b[3])
                    };
                    if !dbg_visuals && win_aux {
                        continue;
                    }
                    let lower = name.to_ascii_lowercase();
                    if lower.contains("cgsys2") && !dbg_visuals {
                        continue;
                    }
                    if let Some(f) = file {
                        if !f.is_empty() {
                            let fpath = String::from_utf8_lossy(f).to_ascii_lowercase();
                            if fpath.contains("debug") {
                                continue;
                            }
                        }
                    }
                    let (base, _) = vm_ui_base_prio(name.as_bytes());
                    let lid = fnv1a(&base);
                    // **cgsys 判定看 FILE 路径,不是层名(勘误 2026-10-17)**。
                    // 旧实现用 `name`(CG 的 ID)判:`BT.OFFES.CONFIG.TIP.BG=1=1`
                    // 里没有 "cgsys" 字样 ⇒ `is_cgsys=false` ⇒ `fallback_z=0`。
                    // 后果:任何 cgsys 界面层在 CGEND 后以「无 Z 槽」的 CG 重建时
                    // 掉到 z=0,沉到所有层底下/或被错误地暴露 ——
                    // 实测开屏 環境設定 页(z998→0)上屏即由此而来。
                    let path_for_kind = file
                        .as_ref()
                        .map(|f| String::from_utf8_lossy(f).to_ascii_lowercase())
                        .unwrap_or_default();
                    // **UI 判定补强(2026-10-17,成果 174)**:
                    // 光看路径不够 —— boot 期大量 VM UI 层(`BT.*`)的**首次**
                    // 事件就是纯属性刷新(`file=""`,如
                    // `BT.OFFES.GAMEMAIN.TIP.CLICK=0=1 E=1`),此时层名与路径
                    // 都没有 `cgsys`,于是被判「非 UI」→ `fallback_z = 0`
                    // → 沉到游戏背景之下(实测首屏只剩 4 层、3 个 TIP 图标
                    // 全 z=0)。
                    //
                    // `BT.` 前缀 = 按钮模板族(引擎 `es.BT.*` 宏管理,
                    // 注册表名如 `BT.OFFES.GAMEMAIN.TIP.CLICK`;成果 159 的
                    // f190 trace 明确该族 Z 档 2010)。据此归 UI,**只影响
                    // z 兜底档位**,不改变显隐(I3 仍按 E 槽)。
                    let is_bt = lower.starts_with("bt.") || lower.contains(".bt.");
                    let is_cgsys = lower.contains("cgsys")
                        || path_for_kind.contains("cgsys")
                        || is_bt;
                    // **判定字段勘误(2026-10-17,成果 174)**:
                    // 旧实现把**层名**当路径传进来判 —— 而层名是
                    // `bt.offes.gamemain.tip.mesw.tx=0=1` 这种,里面**没有**
                    // `cgsys\`,于是 `norm.contains("cgsys\\")` 恒 false ⇒
                    // 所有 cgsys 界面件被判「不是 cgsys」⇒ 走 else 分支
                    // 落到 `fallback_z = 0`。实测后果:消息窗/系统栏
                    // 全部沉到 z=0 被游戏背景盖住,首屏只剩 4 层。
                    // 正确输入 = **FILE 路径**(`cgsys/main/tip_meswindow_…`);
                    // 无 FILE 的属性刷新沿用层名(此时该层已注册过,
                    // gate 只做 deny 判定,不参与 z)。
                    let gate_path = if !path_for_kind.is_empty() {
                        path_for_kind.clone()
                    } else {
                        lower.clone()
                    };
                    if is_cgsys && !vm_ui_layer_allowed(&gate_path, allow_vm_ui, builtin_title) {
                        continue;
                    }
                    // 消息窗状态机(成果 158;勘误 2026-10-17)。
                    //
                    // **旧实现漏了「创建即显示」这一态**:它只在 `E=Some(非零)`
                    // 时开窗,而语料里载体的**创建命令不带 E 槽**(`E=None`)——
                    // `CG` 对象新建时 E 默认 1(CgState 文档),即「显示」。
                    // 后果:窗永不开 ⇒ `Stage::apply_text` 整段丢弃 ⇒
                    // **全部台词不渲染**(实测一帧到 74 行 TextLine 却
                    // `window_open=false`)。
                    //
                    // 现按 CG 对象语义判定:
                    // - `E=Some(0)` → 关(且只有已开过才发关,避免注册期抖动);
                    // - `E=Some(非零)` → 开;
                    // - `E=None` + **带 FILE**(= 新建/换图,默认 E=1)→ 开;
                    // - `E=None` + 无 FILE(纯属性刷新,不改 E)→ 不动。
                    if is_message_window_carrier(name, file.as_deref()) {
                        let has_file = file.as_deref().is_some_and(|f| !f.is_empty());
                        match *e {
                            Some(0) => {
                                if self.stage.window_open() && self.stage.has_entity(lid) {
                                    cmds.push(yuris_stage::Command::MessageWindow(false));
                                }
                            }
                            Some(_) => cmds.push(yuris_stage::Command::MessageWindow(true)),
                            None if has_file => {
                                cmds.push(yuris_stage::Command::MessageWindow(true))
                            }
                            None => {}
                        }
                    }
                    let fallback_z = if is_cgsys {
                        vm_ui_z_for_path(&gate_path)
                    } else {
                        0
                    };
                    // **z 属性继承(成果 161,Confirmed)**：CG 对象按名持有
                    // 属性;CGEND 销毁后同名再建时,命令不带 Z 槽只表示
                    // 「本命令不改 z」,不等于 z=0。引擎 `<game-b>_win_trace.log`
                    // 同名重建行全 `z=None`,画面该层却始终在 998 档。
                    // 取注册表值补齐;注册表也无记录才用 fallback。
                    let z_from_evt = *z;
                    let z_from_reg = self.vm.cg_z(name);
                    let z_from_memo = self.layer_z_memo.get(&lid).copied();
                    let z_slot = z_from_evt
                        .or(z_from_reg)
                        .or(z_from_memo);
                    if let Some(v) = z_slot {
                        self.layer_z_memo.insert(lid, v);
                    }
                    // z 溯源(`YURIS_TRACE_Z=1`):每次 cgsys 层落 z 时打出
                    // 三个来源,直接判定「z 是脚本给的 / 注册表继承 / 兜底」。
                    if std::env::var_os("YURIS_TRACE_Z").is_some() {
                        if let Some(f) = file.as_ref() {
                            let fp = String::from_utf8_lossy(f).to_ascii_lowercase();
                            if fp.contains("cgsys") {
                                yuris_log::trace_forced!(
                                    "[z] {:?} id={lid:#x} evt={z_from_evt:?} reg={z_from_reg:?} memo={z_from_memo:?} → {z_slot:?} fallback={fallback_z}",
                                    name
                                );
                            }
                        }
                    }
                    let slots = yuris_stage::Slots {
                        e: *e,
                        x: *x,
                        y: *y,
                        z: z_slot,
                        a: *a,
                        mode: *mode,
                        tid: tid.clone(),
                        t: *t,
                        ta: *ta,
                        mid: mid.clone(),
                        sx: *sx,
                        sy: *sy,
                    };
                    cmds.push(yuris_stage::Command::Layer {
                        id: lid,
                        name: base,
                        file: file.clone(),
                        slots,
                        fallback_z,
                    });
                }
                yuris_vm::VmEvent::CgAct { id, effects, .. } => {
                    let Some(name) = id else { continue };
                    stage_cgact_trace(name, effects);
                    let has_copy = effects.iter().any(|(_, n, v)| {
                        (n.eq_ignore_ascii_case("COPY") || n.eq_ignore_ascii_case("COPY2"))
                            && match v {
                                yuris_vm::EffectValue::Int(x) => *x != 0,
                                yuris_vm::EffectValue::Float(f) => *f != 0.0,
                                yuris_vm::EffectValue::Str(s) => !s.is_empty(),
                            }
                    });
                    let has_size = effects.iter().any(|(_, n, _)| {
                        n.eq_ignore_ascii_case("SIZE") || n.eq_ignore_ascii_case("SIZE2")
                    });
                    // **数值槽取值:Int 与 Float 都要接受(勘误 2026-10-17)**。
                    // `CGACT[SPLBUF→SPL SIZE]` 的裁剪矩形实测以 **Float** 下发
                    // (`14=Float(1100.0) 15=Float(824.5) 16=Float(1101.0) …`),
                    // 旧实现只认 `EffectValue::Int` ⇒ X2/Y2/SX/SY 全是 None ⇒
                    // params 退化成 `[0,0,MIN,MIN,MIN,MIN]` ⇒ `blit` 的裁剪区
                    // 判成空 ⇒ **整层拿不到像素** ⇒ 截屏全黑(logo 淡出到黑屏)。
                    let val_of = |nm: &str| -> Option<i64> {
                        effects
                            .iter()
                            .find(|(_, n, _)| n.eq_ignore_ascii_case(nm))
                            .and_then(|(_, _, v)| match v {
                                yuris_vm::EffectValue::Int(x) => Some(*x),
                                yuris_vm::EffectValue::Float(f) => Some(*f as i64),
                                _ => None,
                            })
                    };
                    let str_of = |nm: &str| -> Option<String> {
                        effects
                            .iter()
                            .find(|(_, n, _)| n.eq_ignore_ascii_case(nm))
                            .and_then(|(_, _, v)| match v {
                                yuris_vm::EffectValue::Str(s) => Some(s.clone()),
                                _ => None,
                            })
                    };
                    let id2 = str_of("ID2");
                    // CGACT 分派取证(`YURIS_TRACE_CGACT=1`)。
                    if std::env::var_os("YURIS_TRACE_CGACT").is_some() {
                        let names: Vec<&str> =
                            effects.iter().map(|(_, n, _)| n.as_str()).collect();
                        yuris_log::trace_forced!(
                            "[cgact] id={:?} id2={:?} copy={has_copy} size={has_size} eff={names:?}",
                            name, id2
                        );
                    }
                    if has_copy && !has_size && name.as_str() != ":/W0/" {
                        // 模板层 → 目标层别名(引擎方向:ID=源、ID2=目标;成果 134)
                        if let Some(dst_name) = id2.as_ref() {
                            let (src_base, _) = vm_ui_base_prio(name.as_bytes());
                            let (dst_base, _) = vm_ui_base_prio(dst_name.as_bytes());
                            cmds.push(yuris_stage::Command::CopyAlias {
                                dst: fnv1a(&dst_base),
                                src: fnv1a(&src_base),
                            });
                        }
                        continue;
                    }
                    if has_copy && name.as_str() == ":/W0/" {
                        if let Some(dst_name) = id2 {
                            let (dst_base, _) = vm_ui_base_prio(dst_name.as_bytes());
                            cmds.push(yuris_stage::Command::SnapshotTo(fnv1a(&dst_base)));
                        }
                        continue;
                    }
                    if has_size {
                        let Some(dst_name) = id2.clone() else {
                            if std::env::var_os("YURIS_TRACE_CGACT").is_some() {
                                yuris_log::trace_forced!("[cgact] size but NO ID2");
                            }
                            continue;
                        };                        let fixed = effects
                            .iter()
                            .any(|(_, n, _)| n.eq_ignore_ascii_case("FSX"));
                        let fix = |v: Option<i64>| -> Option<i64> {
                            v.map(|x| {
                                if fixed {
                                    (x as f64 / 100.0).floor() as i64
                                } else {
                                    x
                                }
                            })
                        };
                        let x1 = fix(val_of("X").or_else(|| val_of("FX"))).unwrap_or(0);
                        let y1 = fix(val_of("Y").or_else(|| val_of("FY"))).unwrap_or(0);
                        let raw2 = fix(val_of("X2").or_else(|| val_of("FX2")));
                        let rawy2 = fix(val_of("Y2").or_else(|| val_of("FY2")));
                        let rawsx = fix(val_of("SX").or_else(|| val_of("FSX")));
                        let rawsy = fix(val_of("SY").or_else(|| val_of("FSY")));
                        let params = [
                            x1,
                            y1,
                            raw2.unwrap_or(i64::MIN),
                            rawy2.unwrap_or(i64::MIN),
                            rawsx.unwrap_or(i64::MIN),
                            rawsy.unwrap_or(i64::MIN),
                        ];
                        let (src_base, _) = vm_ui_base_prio(name.as_bytes());
                        let (dst_base, _) = vm_ui_base_prio(dst_name.as_bytes());
                        if std::env::var_os("YURIS_TRACE_CGACT").is_some() {
                            yuris_log::trace_forced!(
                                "[cgact] BlitSize push src={:?} dst={:?} params={params:?}",
                                String::from_utf8_lossy(&src_base),
                                String::from_utf8_lossy(&dst_base)
                            );
                        }
                        cmds.push(yuris_stage::Command::BlitSize {
                            src_name: src_base.clone(),
                            src: fnv1a(&src_base),
                            dst: fnv1a(&dst_base),
                            params,
                        });
                        continue;
                    }
                    // **`RECTPAINT` 族(2026-10-18 实装)**
                    //
                    // 手册 `cg/cgact.html`[18]-[20]:`RECTPAINT` 填 RGB、
                    // `RECTPAINT2` 填 α、`RECTPAINT3` 填 `SET=0xAARRGGBB`。
                    // 引擎逐像素改写该层位图;本内核在 Stage 侧**烘焙**
                    // (渲染该层像素 → 覆盖通道 → 作为新像素替换),语义一致:
                    // 一次性生效、不可逆、换图后失效。目标 = `ID`(原地加工)。
                    let paint = effects
                        .iter()
                        .find(|(_, en, v)| {
                            ["RECTPAINT", "RECTPAINT2", "RECTPAINT3"]
                                .iter()
                                .any(|n| en.eq_ignore_ascii_case(n))
                                && match v {
                                    yuris_vm::EffectValue::Int(x) => *x != 0,
                                    yuris_vm::EffectValue::Float(f) => *f != 0.0,
                                    yuris_vm::EffectValue::Str(s) => !s.is_empty(),
                                }
                        })
                        .map(|(_, en, _)| en.clone());
                    if let Some(which) = paint {
                        let kind = if which.eq_ignore_ascii_case("RECTPAINT2") {
                            yuris_stage::PaintKind::Alpha
                        } else if which.eq_ignore_ascii_case("RECTPAINT3") {
                            yuris_stage::PaintKind::Both
                        } else {
                            yuris_stage::PaintKind::Rgb
                        };
                        let Some(color) = val_of("SET") else {
                            yuris_log::alert_once(
                                module_path!(),
                                format!("cgact-paint-no-set:{which}"),
                                format!(
                                    "CGACT {which} 缺少 SET 颜色
                                     层: {name}
                                     后果: 本次填色不执行(引擎必填 SET)"
                                ),
                            );
                            continue;
                        };
                        let (base, _) = vm_ui_base_prio(name.as_bytes());
                        cmds.push(yuris_stage::Command::Paint {
                            dst: fnv1a(&base),
                            kind,
                            color,
                        });
                        continue;
                    }
                    // **未识别的 CGACT 效果槽(2026-10-17 用户要求:必须大告警)**。
                    //
                    // 本函数只消费 COPY/COPY2/SIZE 三族;CGACT 其余
                    // 效果槽(旋转/色调/滤镜/模糊/扩散/光栅/文字…)在 VM 侧
                    // 已如实带出,但**下游没有任何效果实现** —— 以前这里一行
                    // 注释就静默结束。现在按「效果名集合」去重打一条横幅,
                    // 并把本次 CGACT 的层名带上,缺什么直接可查。
                    let unhandled: Vec<&str> = effects
                        .iter()
                        .map(|(_, n, _)| n.as_str())
                        .filter(|n| {
                            !n.eq_ignore_ascii_case("COPY")
                                && !n.eq_ignore_ascii_case("COPY2")
                                && !n.eq_ignore_ascii_case("SIZE")
                                && !n.eq_ignore_ascii_case("SIZE2")
                                && !n.eq_ignore_ascii_case("ID")
                                && !n.eq_ignore_ascii_case("ID2")
                                // RECTPAINT 族已在上面单独消费;`SET` 是它的
                                // 颜色实参,不单列。
                                && !n.eq_ignore_ascii_case("RECTPAINT")
                                && !n.eq_ignore_ascii_case("RECTPAINT2")
                                && !n.eq_ignore_ascii_case("RECTPAINT3")
                                && !n.eq_ignore_ascii_case("SET")
                        })
                        .collect();
                    if !unhandled.is_empty() {
                        let mut key = unhandled.clone();
                        key.sort_unstable();
                        key.dedup();
                        yuris_log::alert_once(
                            module_path!(),
                            format!("cgact-unhandled:{key:?}"),
                            format!(
                                "CGACT 效果槽未实现(已记录未执行)\n\
                                 层: {name}\n\
                                 未消费槽: {key:?}\n\
                                 处置: VM 侧参数已带出,渲染/合成管线**未实现**该效果\n\
                                 —— 画面不会发生对应变化(不是静默:本条即证据)"
                            ),
                        );
                    }
                }
                yuris_vm::VmEvent::CgEnd { id, pc, .. } => {
                    let Some(name) = id else { continue };
                    stage_cg_trace(
                        "CGEND",
                        name,
                        &None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        *pc,
                        self.vm.script_id(),
                    );
                    let (base, _) = vm_ui_base_prio(name.as_bytes());
                    cmds.push(yuris_stage::Command::Kill(fnv1a(&base)));
                }
                _ => {}
            }
        }
    }

    fn consume_events(&mut self, allow_vm_ui: bool, builtin_title: bool) {
        // 取出为本段事件(独立 Copy;translate 需要 &mut self,vm 的
        // events() 借用不能跨)。
        CG_TRACE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // **零克隆消费(成果 171)**:旧实现把 `events()[cursor..]` 整份
        // `to_vec()` —— boot 单帧 85 万组时那是 85 万次深拷贝。现在直接
        // 搬走 VM 的事件缓冲(`take_events`),消费完自然清空。
        let (fresh_owned, domains_owned) = self.vm.take_events();
        let _ = domains_owned;
        // 域分布诊断(P10,成果 161):`YURIS_TRACE_DOMAINS=1` 时按帧打印
        // 事件数 × 域标(0=主线,1..=任务创建序)。定位 boot 作业生产速率的
        // 来源域(主线 run-to-yield vs 任务泵)。
        if std::env::var_os("YURIS_TRACE_DOMAINS").is_some() {
            let domains = &domains_owned;
            let from = 0usize.min(domains.len());
            let mut by_domain: std::collections::BTreeMap<u8, usize> =
                std::collections::BTreeMap::new();
            for d in &domains[from..] {
                *by_domain.entry(*d).or_insert(0) += 1;
            }
            yuris_log::trace_forced!("[domains] fresh={} {by_domain:?}", fresh_owned.len());
        }
        self.events_cursor = 0;
        if fresh_owned.is_empty() {
            return;
        }
        // **Unsupported 首次告警兜底(2026-10-17)**:lenient 模式(记事件
        // 继续)与不走 `note_unsupported` 的直推点产生的「未实现」事件,
        // 旧实现一片静默 —— 实机「没反应、日志还什么都看不到」的观感来源
        // 之一。同因只报一次,集合 512 封顶(宁漏报不涨内存)。
        self.note_unsupported_events(&fresh_owned);
        // ===== 成果 162：音频事件直接驱动声层 =====
        // 脚本 `SOUND`/`SOUNDEND` 的完整参数在 VM 侧已保留（含 FADE/LOOP/
        // VOLUME/PAN/PITCH/START/RESTART/END），这里原样映射到 `yuris-audio`。
        for ev in fresh_owned.iter() {
            self.apply_sound_event(ev);
        }
        // ===== FILEACT:文件操作意图 → 平台文件系统(唯一 I/O 边界在此)=====
        for ev in fresh_owned.iter() {
            self.apply_fileact_event(ev);
        }
        // ===== M2 Stage 全量翻译(含精灵族;成果 158)=====
        let mut stage_cmds: Vec<yuris_stage::Command> = Vec::new();
        self.translate_stage_frame(&fresh_owned, allow_vm_ui, builtin_title, &mut stage_cmds);
        // **统一文本通道**(成果 168):v49x 编译内联 `WORD`/`TEXT` 与 v555
        // 明文剧本共用 `yuris_vm::scenario::TextChannel`,不再按 `VmEvent::Text`
        // 反扫原始字节(旧路径把编码判定散在消费侧,且同一帧多行只留最后一行)。
        // 逐行入 Stage —— 同一帧多条文本按 `seq` 顺序提交,呈现 = 末条,
        // 与引擎「文本子系统同帧内后写覆盖」一致。
        let text_lines = self.vm.drain_text_lines();
        if std::env::var_os("YURIS_TRACE_TEXT").is_some() && !text_lines.is_empty() {
            yuris_log::trace_forced!(
                "[text] lines={} window_open={}",
                text_lines.len(),
                self.stage.window_open()
            );
            // 逐条打出来源 + 内容(前 20 条):按钮文本/台词的生成点定位。
            for l in text_lines.iter().take(20) {
                let (sid, pc) = match l.source {
                    yuris_vm::scenario::TextSource::Inline { script_id, pc } => (script_id, pc),
                    yuris_vm::scenario::TextSource::Scenario => (0, 0),
                };
                yuris_log::trace_forced!(
                    "[text-src] s{sid} pc{pc} len={} {:?}",
                    l.text.chars().count(),
                    l.text.chars().take(40).collect::<String>()
                );
            }
        }
        for line in text_lines {
            if line.text.is_empty() {
                yuris_log::warn!(
                    "[player] 台词文本编码判定失败(s{};SJIS/Big5 均有非法序列,不猜)",
                    match line.source {
                        yuris_vm::scenario::TextSource::Inline { script_id, .. } => script_id,
                        yuris_vm::scenario::TextSource::Scenario => 0,
                    }
                );
                continue;
            }
            let content = fast2s::convert(&line.text);
            stage_cmds.push(yuris_stage::Command::Text {
                layer: SC_TEXT,
                content,
            });
        }
        if !stage_cmds.is_empty() {
            // 截屏取样的"上一呈现帧" = 本批开始前的已提交景(Stage I4/I6)。
            let (cw, ch) = (logical_w(), logical_h());
            self.stage.set_canvas(cw, ch);
            let scene = self.bridge.scene_mut();
            let mut host = StageAssets {
                index: &self.index,
                backend: self.backend.as_mut(),
                loaded: &mut self.loaded,
                font: &self.font,
                text_px: self.vm.font_size(),
                text_slots: &mut self.text_slots,
                text_seq: &mut self.text_slot_seq,
                snap_seq: &mut self.snap_seq,
                paint_seq: 0,
            };
            self.stage.apply(&stage_cmds, scene, &mut host);
        }
        // **封顶事件缓冲**(内存泄漏修复):VM 的 `events()` 是只增不减的
        // `Vec`,本节已把 `[events_cursor..]` 全部消费完 ⇒ 就地丢弃,游标归零。
        // 不丢的话长跑必爆(实测数十帧即 1.16e6 条事件 ≈ 数十 MB;
        // 开启任务调度后事件量还会再涨)。
        // 事件缓冲已由 `take_events` 搬空;游标恒 0。
        self.events_cursor = 0;
    }

    /// **Unsupported 事件的首次告警**(消费侧兜底;见字段
    /// [`Self::unsup_note_seen`])。
    fn note_unsupported_events(&mut self, events: &[yuris_vm::VmEvent]) {
        for ev in events {
            if let yuris_vm::VmEvent::Unsupported { pc, command, reason } = ev {
                if self.unsup_note_seen.len() >= 512 {
                    return;
                }
                let note = format!("cmd idx{command} pc{pc} :: {reason}");
                if self.unsup_note_seen.insert(note.clone()) {
                    // 与 VM 侧同级的**醒目横幅**(VM 的 strict 告警可能被
                    // YURIS_QUIET_UNSUP 压掉;消费侧这条永远打)。
                    yuris_log::alert!(
                        "消费侧收到未实现事件\n\
                         命令下标: {command}  组: {pc}\n\
                         原因: {reason}\n\
                         处置: strict 下该 VM 已挂起;若仍看到游戏推进,\
                         说明走了 lenient 或某个特判继续路径"
                    );
                }
            }
        }
    }

    /// 注册表重放(M3 迁入 Stage,2026-10-16;注册链来源 = P8.2b):进入游戏时
    /// 按 VM `cg_registry` 把游戏已登记的界面件恢复上屏 —— 每件 = 一条
    /// [`yuris_stage::Command::Layer`](FILE 族创建 + `E=1` 显式点亮,坐标取自
    /// 注册表)。重放 = 一批原子(同帧完成,无中间态)。诊断清单逐条打印。
    ///
    /// 与旧实现的差异:①层状态落在 Stage 实体表(不再直写场景);
    /// ②无纹理的注册件由 Stage 留作空实体不上屏(旧实现跳过 —— 结果同);
    /// ③同名单件重复注册时后写覆盖(旧实现按 prio 择优,prio 已恒等,无差)。
    fn rebuild_vm_ui(&mut self) {
        let mut cmds: Vec<yuris_stage::Command> = Vec::new();
        let mut skipped = 0usize;
        let mut inv: Vec<String> = Vec::new();
        for (name, x, y, z, e, file) in self.vm.cg_registry_snapshot_full() {
            let Some(f) = file else {
                skipped += 1;
                continue; // 注册件无 FILE(纹理经他路):不进层
            };
            if f.is_empty() {
                skipped += 1;
                continue;
            }
            let path = String::from_utf8_lossy(&f).into_owned();
            let lower = path.to_ascii_lowercase().replace('/', "\\");
            // 本函数只在「播放器自己接管画面」时被调用(见 `tick` 的 builtin_title 门),
            // 故此处按内置标题在场处理。
            if !vm_ui_layer_allowed(&lower, true, true) {
                continue;
            }
            let (base, _) = vm_ui_base_prio(&name);
            // **保真重放(勘误 2026-10-17)**:注册表里的 `E` 是引擎对象
            // 的真实显隐位,不能一律点亮 —— config 页注册时 `E=0`,
            // 强制 `E=1` 会让它在开屏就上屏(实机症状:一启动就是環境設定)。
            //
            // `z` 只在脚本**显式给过**时才恢复(`Some`);从未给过 Z 的对象
            // 传 `None` 落到 `fallback_z` 档位。旧实现无条件 `Some(z)`,
            // 而 `z` 的缺省 0 被当成"真实层序"写回 ⇒ 标题六钮/系统栏
            // 全部 z=0 沉到背景之下(2026-10-17 实机 + `YURIS_TRACE_Z` 复现)。
            let slots = yuris_stage::Slots {
                e: Some(e),
                x: Some(x),
                y: Some(y),
                z,
                ..Default::default()
            };
            let dir = lower
                .trim_start_matches(r"cgsys\")
                .split('\\')
                .next()
                .unwrap_or("?");
            let nm = String::from_utf8_lossy(&base);
            if inv.len() < 60 {
                inv.push(format!("  [{dir}] {nm} @({x:.0},{y:.0})"));
            }
            cmds.push(yuris_stage::Command::Layer {
                id: fnv1a(&base),
                name: base,
                file: Some(f.clone()),
                slots,
                fallback_z: vm_ui_z_for_path(&lower),
            });
        }
        let n = cmds.len();
        if !cmds.is_empty() {
            self.stage.set_canvas(logical_w(), logical_h());
            let scene = self.bridge.scene_mut();
            let mut host = StageAssets {
                index: &self.index,
                backend: self.backend.as_mut(),
                loaded: &mut self.loaded,
                font: &self.font,
                text_px: self.vm.font_size(),
                text_slots: &mut self.text_slots,
                text_seq: &mut self.text_slot_seq,
                snap_seq: &mut self.snap_seq,
                paint_seq: 0,
            };
            self.stage.apply(&cmds, scene, &mut host);
            // VM 消息窗部件已上屏 → 撤掉 scenario 近似底框(双重叠层修复)
            self.bridge.scene_mut().hide_layer(SC_WIN);
        }
        yuris_log::debug!(
            "[player] VM UI 重放 {n} 层(跳 {skipped};注册 {} 条)",
            self.vm.cg_registry_len()
        );
        for l in &inv {
            yuris_log::trace!("[vm-ui]{l}");
        }
        // ADV 主按钮行(vm_ui 之外独立管理;参考图标定)
        self.build_adv_row();
    }

    /// 读 scenario 资源并上传;返回 ResourceId。
    fn load_scenario_image(&mut self, path_or_name: &str) -> Option<ResourceId> {
        let rid = ResourceId(fnv1a(path_or_name.as_bytes()));
        if self.loaded.contains(&rid.0) {
            return Some(rid);
        }
        let data = match self.index.read_cg_bytes(path_or_name) {
            Some(d) => d,
            None => {
                // **子画面/标题素材缺失**:命中区仍在,但按钮/底图不可见。
                // 旧实现 `?` 静默返回 None —— 实机表现为「按钮看不见还能点」。
                yuris_log::alert_once(
                    module_path!(),
                    format!("scenario-img-miss:{path_or_name}"),
                    format!(
                        "子画面素材未找到\n\
                         路径: {path_or_name}\n\
                         后果: 该素材不上屏(命中区/流程仍存在)\n\
                         处置: 检查 cg 包条目名与扩展名候选"
                    ),
                );
                return None;
            }
        };
        let Some(b) = self.backend.as_mut() else {
            yuris_log::alert_once(
                module_path!(),
                "scenario-img-no-backend",
                "子画面素材无法装载:图形后端尚未注入\n\
                 处置: 播放器须在 tick 前注入 backend"
                    .to_string(),
            );
            return None;
        };
        if let Err(e) = b.load_image(rid, &data) {
            yuris_log::alert_once(
                module_path!(),
                format!("scenario-img-decode:{path_or_name}"),
                format!(
                    "子画面素材解码/上传失败\n\
                     路径: {path_or_name}\n\
                     错误: {e}\n\
                     后果: 该素材不上屏"
                ),
            );
            return None;
        }
        self.loaded.insert(rid.0);
        Some(rid)
    }

    /// 标题按钮三态切换(每帧;悬停 `_on`、按下帧 `_over`,其余 `_off`;成果 78)。
    /// 悬停**进入**命中区的边沿播悬停音(P2-2,成果 80)。子画面打开时让位。
    fn update_title_buttons(&mut self) {
        if self.subui != SubUi::None || self.title_buttons.is_empty() {
            return;
        }
        let (updates, hover_entered) = tri_state_pass(
            &mut self.title_buttons,
            self.cursor_logical,
            self.frame_clicked,
        );
        if hover_entered {
            self.play_sysse(TITLE_SE_HOVER);
        }
        if updates.is_empty() {
            return;
        }
        let scene = self.bridge.scene_mut();
        for (id, rid) in updates {
            if let Some(layer) = scene_mut_layer(scene, id) {
                layer.resource = Some(rid);
            }
        }
    }

    /// 子画面按钮三态切换(P3;机制与标题按钮同,悬停进入边沿播悬停音)。
    fn update_ui_buttons(&mut self) {
        if self.ui_buttons.is_empty() {
            return;
        }
        let (updates, hover_entered) = tri_state_pass(
            &mut self.ui_buttons,
            self.cursor_logical,
            self.frame_clicked,
        );
        if hover_entered {
            self.play_sysse(TITLE_SE_HOVER);
        }
        if updates.is_empty() {
            return;
        }
        let scene = self.bridge.scene_mut();
        for (id, rid) in updates {
            if let Some(layer) = scene_mut_layer(scene, id) {
                layer.resource = Some(rid);
            }
        }
    }

    /// 构建 ADV 主按钮行(layout-fix-plan P2-2;参考图坐标 + 图集三态)。
    /// 幂等:已有行不重建;素材缺失只记日志不占位(视觉正确优先)。
    fn build_adv_row(&mut self) {
        if !self.adv_buttons.is_empty() {
            return;
        }
        for &(id, x, y, w, h, path, segs) in ADV_BUTTONS {
            let off = self.load_atlas_segment(path, 0, segs);
            let on = self.load_atlas_segment(path, 1, segs);
            let over = self.load_atlas_segment(path, 2, segs);
            if off.is_none() {
                yuris_log::warn!("[adv] 主行素材未命中: {path}");
                continue;
            }
            self.adv_buttons.push(TitleButton {
                rect: [x, y, w as f32, h as f32],
                id,
                rids: [off, on, over],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
            let layer = Layer {
                id,
                z: 93, // 立绘(10)/场景层之上、VM 按钮带(95)之下
                visible: true,
                x,
                y,
                scale_x: 1.0,
                scale_y: 1.0,
                alpha: 1.0,
                rotation: 0.0,
                resource: off,
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(layer);
        }
        yuris_log::debug!(
            "[adv] 主按钮行 {} 钮(layout-fix-plan P2-2)",
            self.adv_buttons.len()
        );
    }

    /// ADV 主行三态(悬停/按下;动作路由 = P9.2 待实装)。
    fn update_adv_buttons(&mut self) {
        if self.adv_buttons.is_empty() {
            return;
        }
        let (updates, hover_entered) = tri_state_pass(
            &mut self.adv_buttons,
            self.cursor_logical,
            self.frame_clicked,
        );
        if hover_entered {
            self.play_sysse(TITLE_SE_HOVER);
        }
        if updates.is_empty() {
            return;
        }
        let scene = self.bridge.scene_mut();
        for (id, rid) in updates {
            if let Some(layer) = scene_mut_layer(scene, id) {
                layer.resource = Some(rid);
            }
        }
    }

    /// 光标是否命中 ADV 主行(点击消费用;返回按钮下标)。
    fn adv_button_hit(&self) -> Option<usize> {
        let (cx, cy) = self.cursor_logical;
        self.adv_buttons.iter().position(|b| {
            let [x, y, w, h] = b.rect;
            b.active
                && cx >= x as i64
                && cx < (x + w) as i64
                && cy >= y as i64
                && cy < (y + h) as i64
        })
    }

    /// 渲染台词/选择肢到 RGBA 白字(转调 [`rasterize_text`];选择肢路径与
    /// Stage 文本共用同一字型/槽池)。
    fn render_text_layer(&mut self, text: &str) -> Option<ResourceId> {
        let px = text_px_or_default(self.vm.font_size());
        rasterize_text(
            &self.font,
            &mut self.text_slots,
            &mut self.text_slot_seq,
            self.backend.as_mut(),
            text,
            px,
        )
    }

    /// 精灵淡入/淡出动画推进(\S/\T 的 fade_ms 与 \S.D 淡出)。
    /// 立绘位移动画步进(每帧;完成后钉在终点)。
    fn update_sprite_moves(&mut self) {
        if self.sprite_moves.is_empty() && self.tachie_float.is_empty() {
            return;
        }
        let now = self.anim_now();
        let mut done: Vec<u64> = Vec::new();
        for (id, start, ms, from, to) in &self.sprite_moves {
            let t = ((now - *start) / (*ms.max(&1) as f64)) as f32;
            let t = t.clamp(0.0, 1.0);
            let x = from.0 + (to.0 - from.0) * t;
            let y = from.1 + (to.1 - from.1) * t;
            if let Some(layer) = scene_mut_layer(self.bridge.scene_mut(), *id) {
                layer.x = x;
                layer.y = y;
            }
            if let Some(entry) = self.tachie_float.get_mut(id) {
                entry.1 = (x, y);
            }
            if t >= 1.0 {
                done.push(*id);
            }
        }
        self.sprite_moves.retain(|m| !done.contains(&m.0));
        // 循环上下浮动(基准 + 正弦偏移;P0 实测)
        let elapsed = (self.anim_now() / 1000.0) as f32;
        for (id, (phase, (bx, by))) in &self.tachie_float {
            if let Some(layer) = scene_mut_layer(self.bridge.scene_mut(), *id) {
                layer.x = *bx;
                layer.y = *by + tachie_float_dy(elapsed, *phase);
            }
        }
    }

    /// 按层 id 隐藏精灵(淡出动画完成由 tick 收尾隐藏)。先撤销同 id
    /// 未完成的淡入条目(否则 in/out 双条目同帧互写 alpha)。
    fn hide_sprite_id(&mut self, id: u64, fade_ms: u64) {
        self.tachie_float.remove(&id);
        if fade_ms > 0 {
            self.sprite_fades.retain(|(fid, _, _, _)| fid != &id);
            self.sprite_fades.push((id, self.anim_now(), fade_ms, true));
            return;
        }
        self.bridge.scene_mut().hide_layer(id);
    }

    fn update_sprite_fades(&mut self) {
        if self.sprite_fades.is_empty() {
            return;
        }
        let mut done: Vec<(u64, bool)> = Vec::new();
        for (id, start, ms, out) in &self.sprite_fades {
            let t = ((self.anim_now() - *start) / (*ms).max(1) as f64) as f32;
            let a = if *out { 1.0 - t.min(1.0) } else { t.min(1.0) };
            let scene = self.bridge.scene_mut();
            if let Some(l) = scene.layers.iter_mut().find(|l| l.id == *id) {
                l.alpha = a;
                l.visible = a > 0.001;
            }
            if t >= 1.0 {
                done.push((*id, *out));
            }
        }
        for (id, out) in done {
            if out {
                self.bridge.scene_mut().hide_layer(id);
            }
            self.sprite_fades.retain(|(fid, _, _, _)| fid != &id);
        }
    }

    /// fade 覆盖层动画推进。
    fn update_fade(&mut self) {
        let Some(fs) = &self.fade else { return };
        let t = ((self.anim_now() - fs.start) / fs.ms.max(1) as f64) as f32;
        let a = if fs.out { t.min(1.0) } else { 1.0 - t.min(1.0) };
        let [r, g, bcol] = fs.color;
        let rid = ResourceId(0x5C_2000_0000 | ((r as u64) << 16) | ((g as u64) << 8) | bcol as u64);
        if !self.loaded.contains(&rid.0) {
            let px = vec![r, g, bcol, 255];
            if let Some(b) = self.backend.as_mut() {
                let _ = b.load_image(rid, &px);
                self.loaded.insert(rid.0);
            }
        }
        let layer = Layer {
            id: SC_FADE,
            z: 100,
            visible: a > 0.001,
            x: 0.0,
            y: 0.0,
            scale_x: logical_w(),
            scale_y: logical_h(),
            alpha: a,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
        if t >= 1.0 {
            yuris_log::trace!("[scenario] fade 完成(out={})", fs.out);
            self.fade = None;
        }
    }

    /// 音频包内解析音频条目(voice/bgm/se 命名族)。
    ///
    /// **成果 136**:遍历的是 **VFS 归一化键**(CP932 + 折叠大小写 +
    /// 统一分隔符),比较与读取在同一键空间 —— 旧实现把键 lossy 成
    /// UTF-8 再 `to_ascii_lowercase`,对日文名(如 `se\自動ドアが開く.ogg`)
    /// 必然漏检;且用 `&k[1..]` 硬切 1 字节做"剥根"(假设根恒 1 字节)。
    fn resolve_audio(&self, pack: &PacFileIndex, prefix: &str, name: &str) -> Option<Vec<u8>> {
        let want = yuris_vm::vfs::normalize_request(&format!("{prefix}{name}.ogg"));
        let wb = want.as_bytes();
        for k in pack.keys() {
            let kb = k.as_bytes();
            let hit = kb == wb || (kb.len() > wb.len() && kb.ends_with(wb));
            if hit {
                if let Some(d) = pack.read_key_bytes(k) {
                    return Some(d);
                }
            }
        }
        None
    }

    /// 解析脚本 `SOUND[FILE=…]` 的逻辑名（成果 162）。
    ///
    /// 入参是 **CP932 原始字节**（与 `CG.FILE` 同约定）——语料实测声音名含
    /// 日文（`voice/琴音…`），先转 UTF-8 会毁掉名字。
    ///
    /// 形态不固定（语料实测 `bgm/xxx`、`voice/xxx`，均无扩展名）：
    /// 1. 去掉引擎的 `:/<段>/` 层组前缀（如 `:/S0/`）；
    /// 2. 路径形（含分隔符）直接用；裸名依次尝试
    ///    `""/bgm/se/voice/sysse/sysvo` 六个根；
    /// 3. 每个候选按 `原名 → .ogg → .wav` 顺序查 VFS 归一化键。
    fn resolve_audio_any(&self, name: &[u8]) -> Option<Vec<u8>> {
        for cand in audio_name_candidates(name) {
            let want = yuris_vm::vfs::normalize_request_bytes(&cand);
            let wb = want.as_bytes();
            for k in self.audio_packs.keys() {
                let kb = k.as_bytes();
                let hit = kb == wb || (kb.len() > wb.len() && kb.ends_with(wb));
                if hit {
                    if let Some(d) = self.audio_packs.read_key_bytes(k) {
                        return Some(d);
                    }
                }
            }
        }
        None
    }

    /// 应用 VM 的 `SOUND` / `SOUNDEND` 事件到音频层（成果 162）。
    ///
    /// 直接把事件里的完整参数映射成 [`yuris_audio::SoundCommand`]；
    /// 声层键用脚本给的 `ID` 原样（`SOUNDEND` 用同一键）。
    fn apply_sound_event(&mut self, ev: &yuris_vm::VmEvent) {
        use yuris_vm::VmEvent;
        let trace = std::env::var_os("YURIS_TRACE_SOUND").is_some();
        match ev {
            VmEvent::Sound { .. } => {
                let Some(cmd) = sound_command_from_event(ev) else {
                    return;
                };
                let id = cmd.id.clone();
                if trace {
                    yuris_log::trace_forced!(
                        "[trace-sound] id={:?} file={:?} e={:?} play={:?} pause={:?} fade={:?} start={:?} restart={:?} end={:?} loop={:?} vol={:?} pan={:?} pitch={:?}",
                        cmd.id,
                        sound_event_file(ev).map(|f| String::from_utf8_lossy(f).into_owned()),
                        cmd.enabled,
                        cmd.play,
                        cmd.pause,
                        cmd.fade_ms,
                        cmd.start_ms,
                        cmd.restart_ms,
                        cmd.end_ms,
                        cmd.loop_count,
                        cmd.volume,
                        cmd.pan,
                        cmd.pitch,
                    );
                }

                if let Some(file) = sound_event_file(ev).filter(|f| !f.is_empty()) {
                    let display = String::from_utf8_lossy(file).into_owned();
                    let Some(data) = self.resolve_audio_any(file) else {
                        // **资源缺失 = 运行时功能缺失**:以前一行 warn,
                        // 默认等级下一闪而过。现在按文件名去重打横幅。
                        yuris_log::alert_once(
                            module_path!(),
                            format!("sound-miss:{display}"),
                            format!(
                                "SOUND 资源未找到\n\
                                 id={id} file={display}\n\
                                 后果: 该声层无音源(参数仍应用,但不会出声)\n\
                                 处置: 检查 audio 封包/候选根(bgm/se/voice/sysse/sysvo)"
                            ),
                        );
                        // 资源缺失仍然应用参数（声层不存在时为空操作）。
                        let _ = self.audio.sound(&cmd);
                        return;
                    };
                    // 直接装入已解析字节；**不要**再设 cmd.file，否则
                    // `AudioSystem::sound` 会按名字二次查资源（必然失败）。
                    // `load_bytes` 已把 `SOUNDINFO FILE=` 需要的原始名记进声层。
                    match self.audio.system_mut().load_bytes(&id, &display, &data) {
                        Ok(_) => {}
                        Err(e) => {
                            yuris_log::alert_once(
                                module_path!(),
                                format!("sound-decode:{display}"),
                                format!(
                                    "SOUND 解码失败,该声层不会出声\n\
                                     id={id} file={display}\n\
                                     错误: {e}\n\
                                     处置: 编码不在支持面(PCM WAVE / OGG)或数据损坏"
                                ),
                            );
                            return;
                        }
                    }
                }
                if let Err(e) = self.audio.sound(&cmd) {
                    yuris_log::error!("[audio] SOUND 参数错误: id={id}: {e}");
                }
            }
            VmEvent::SoundEnd { id, .. } => {
                if let Some(id) = id.as_deref().filter(|s| !s.is_empty()) {
                    if trace {
                        yuris_log::trace_forced!("[trace-sound] SOUNDEND id={id:?}");
                    }
                    let _ = self.audio.system().sound_end(id);
                }
            }
            _ => {}
        }
    }

    /// 应用 VM 的 `FILEACT` 意图到平台文件系统(手册 `file/fileact.html`)。
    ///
    /// **分层的理由**:VM 侧只解析参数（`VmEvent::FileAct`），真实 I/O 在
    /// 播放器 —— Kernel 与平台之间只经 `RuntimeApi` 的约束不能被脚本命令
    /// 绕过；同时路径根、沙箱、错误呈现都是平台属性。
    ///
    /// 引擎处理体语义（v494 `0x441234` 逐槽分支实测）：
    /// - `CREATE` → 按 `\`/`/` 分段**逐级**建目录（`0x4426cc`）；
    /// - `COPY` → `CopyFileA(FILE, FILE2, 0)`；
    /// - `DELETE` → `DeleteFileA(FILE)` + `RemoveDirectoryA(FOLDER)`；
    /// - `EXEC`/`EXEC2` → `ShellExecuteExA`（`PARAM` 作参数）；
    /// - `SET`/`BMP`/`PNG`/`JPG`/`ID` → 图层导出（本内核无导出面）；
    /// - `CGCASH`/`SOUNDCASH` → 缓存登记（本内核按需装载，无缓存表可登记）。
    ///
    /// 未实装的分支**不静默**：按分支名去重告警（`alert_once`）。
    fn apply_fileact_event(&mut self, ev: &yuris_vm::VmEvent) {
        let yuris_vm::VmEvent::FileAct {
            file,
            file2,
            folder,
            create,
            copy,
            delete,
            exec,
            exec2,
            param,
            set,
            id,
            bmp,
            png,
            jpg,
            cg_cache,
            sound_cache,
            ..
        } = ev
        else {
            return;
        };
        let truthy = |v: &Option<i64>| v.map(|n| n != 0).unwrap_or(false);
        let trace = std::env::var_os("YURIS_TRACE_FILEACT").is_some();
        if trace {
            yuris_log::trace_forced!(
                "[fileact] file={file:?} file2={file2:?} folder={folder:?} create={create:?} copy={copy:?} delete={delete:?} exec={exec:?} exec2={exec2:?} param={param:?} set={set:?} id={id:?} bmp={bmp:?} png={png:?} jpg={jpg:?}"
            );
        }
        // `CREATE`：逐级建目录（引擎按分隔符切段，逐段 CreateDirectory）。
        if truthy(create) {
            if let Some(dir) = folder.as_deref().filter(|s| !s.is_empty()).or(file.as_deref()) {
                let path = self.game_dir.join(normalize_script_path(dir));
                if let Err(e) = std::fs::create_dir_all(&path) {
                    yuris_log::alert_once(
                        module_path!(),
                        format!("fileact-create:{dir}"),
                        format!(
                            "FILEACT CREATE 失败\n目录: {}\n错误: {e}\n处置: 检查游戏目录写权限",
                            path.display()
                        ),
                    );
                }
            }
        }
        // `COPY`：文件复制（目标 = FILE2）。
        if truthy(copy) {
            if let (Some(src), Some(dst)) = (
                file.as_deref().filter(|s| !s.is_empty()),
                file2.as_deref().filter(|s| !s.is_empty()),
            ) {
                let src_p = self.game_dir.join(normalize_script_path(src));
                let dst_p = self.game_dir.join(normalize_script_path(dst));
                if let Some(parent) = dst_p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::copy(&src_p, &dst_p) {
                    yuris_log::alert_once(
                        module_path!(),
                        format!("fileact-copy:{src}:{dst}"),
                        format!(
                            "FILEACT COPY 失败\n源: {}\n目标: {}\n错误: {e}",
                            src_p.display(),
                            dst_p.display()
                        ),
                    );
                }
            }
        }
        // `DELETE`：存档文件删除 / 空目录删除。
        if truthy(delete) {
            if let Some(f) = file.as_deref().filter(|s| !s.is_empty()) {
                let p = self.game_dir.join(save_file_name(f));
                match std::fs::remove_file(&p) {
                    Ok(()) => yuris_log::debug!("[fileact] 删除存档 {}", p.display()),
                    // 不存在 = 引擎同款静默（DeleteFile 失败但脚本不中断）。
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => yuris_log::alert_once(
                        module_path!(),
                        format!("fileact-del:{f}"),
                        format!("FILEACT DELETE 失败\n文件: {}\n错误: {e}", p.display()),
                    ),
                }
            }
            if let Some(d) = folder.as_deref().filter(|s| !s.is_empty()) {
                let p = self.game_dir.join(normalize_script_path(d));
                // 只删空目录（手册明示「中身のあるフォルダは削除できません」）。
                match std::fs::remove_dir(&p) {
                    Ok(()) => yuris_log::debug!("[fileact] 删除空目录 {}", p.display()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => yuris_log::alert_once(
                        module_path!(),
                        format!("fileact-rmdir:{d}"),
                        format!(
                            "FILEACT DELETE 目录失败(引擎只允许删空目录)\n目录: {}\n错误: {e}",
                            p.display()
                        ),
                    ),
                }
            }
        }
        // `EXEC`/`EXEC2`：执行外部程序。
        //
        // **安全边界**:本内核不执行脚本指定的任意可执行文件 —— 那是一个
        // 远端可写脚本就能触发的任意命令执行面,且与"兼容运行内核"的目标
        // 无关。显式告警而不是静默。
        if truthy(exec) || truthy(exec2) {
            yuris_log::alert_once(
                module_path!(),
                format!("fileact-exec:{}", file.as_deref().unwrap_or("")),
                format!(
                    "FILEACT EXEC 未执行(本内核拒绝执行脚本指定的外部程序)\n\
                     文件: {}\n参数: {}\n\
                     处置: 引擎侧走 ShellExecuteEx;此处只记录意图,不启动进程",
                    file.as_deref().unwrap_or("<无>"),
                    param.as_deref().unwrap_or("<无>")
                ),
            );
        }
        // 图层导出（BMP/PNG/JPG + ID + SET）。
        if truthy(bmp) || truthy(png) || truthy(jpg) {
            yuris_log::alert_once(
                module_path!(),
                format!("fileact-export:{}", id.as_deref().unwrap_or("")),
                format!(
                    "FILEACT 图层导出未实现\n图层: {}\n目标: {}\n格式: BMP={bmp:?} PNG={png:?} JPG={jpg:?} 级别={set:?}\n\
                     处置: 本内核无图层导出面;引擎侧写 BMP/PNG/JPG 文件",
                    id.as_deref().unwrap_or("<无>"),
                    file.as_deref().unwrap_or("<无>")
                ),
            );
        }
        // 缓存登记：本内核按需装载，没有跨存档的 CG/音源缓存表。
        if truthy(cg_cache) || truthy(sound_cache) {
            yuris_log::debug!(
                "[fileact] 缓存登记 CGCASH={cg_cache:?} SOUNDCASH={sound_cache:?}(本内核按需装载,无需登记)"
            );
        }
    }

    /// 播系统 UI 音(sysse 包;标题按钮悬停/决定音,P2-2 成果 80)。
    /// 未命中(包缺/条目缺)记录决策不出声,不阻断流程。
    fn play_sysse(&mut self, name: &str) {
        match self.resolve_audio(&self.audio_packs, "", name) {
            Some(data) => self.audio.play_se(name, data),
            None => {
                self.audio.play_se(name, Vec::new());
                yuris_log::alert_once(
                    module_path!(),
                    format!("sysse-miss:{name}"),
                    format!(
                        "系统 UI 音未找到(按钮仍可用,但无音效)\n\
                         名称: {name}\n\
                         处置: 检查 sysse 包条目;这不是静默跳过"
                    ),
                );
            }
        }
    }

    // ================= P3 子画面(LOAD / EXTRA / END 确认;成果 81) =================

    /// 关闭子画面:隐藏全部 UI_* 层、清按钮与查看态,回到标题菜单。
    fn close_subui(&mut self) {
        let scene = self.bridge.scene_mut();
        for base in [
            UI_LOAD_BASE,
            UI_EXTRA_BASE,
            UI_CG_BASE,
            UI_BGM_BASE,
            UI_CONFIRM_BASE,
        ] {
            for i in 0..0x100u64 {
                scene.hide_layer(base + i);
            }
        }
        self.ui_buttons.clear();
        self.cg_view = None;
        self.subui = SubUi::None;
    }

    /// 子画面三态图按钮(states = 素材后缀组;命中区 rect 用素材实际尺寸,
    /// 素材未命中时保留 rect 兜底命中区 —— 无图可点,不静默失效)。
    fn ui_button(&mut self, id: u64, rect: [f32; 4], base_path: &str, states: [&str; 3], z: i64) {
        let mut rids: [Option<ResourceId>; 3] = [None, None, None];
        for (i, sfx) in states.iter().enumerate() {
            rids[i] = self.load_scenario_image(&format!("{base_path}{sfx}"));
        }
        self.ui_button_rids(id, rect, rids, z, true);
    }

    /// 子画面按钮(已备三态 rids;命中区 rect 用素材实际尺寸)。
    fn ui_button_rids(
        &mut self,
        id: u64,
        rect: [f32; 4],
        rids: [Option<ResourceId>; 3],
        z: i64,
        active: bool,
    ) {
        let (iw, ih) = rids
            .iter()
            .flatten()
            .next()
            .copied()
            .and_then(|rid0| self.backend.as_ref().and_then(|b| b.image_size(rid0.0)))
            .unwrap_or((rect[2] as u32, rect[3] as u32));
        if rids[TITLE_BTN_OFF].is_none() {
            yuris_log::alert_once(
                module_path!(),
                format!("subui-btn-miss:{id:#x}"),
                format!(
                    "子画面按钮素材缺失\n\
                     按钮 id = {id:#x}\n\
                     后果: 按钮不可见,但**命中区仍存在**(可点)\n\
                     处置: 检查 rids 三态路径与 cg 包条目"
                ),
            );
        }
        self.ui_buttons.push(TitleButton {
            rect: [rect[0], rect[1], iw as f32, ih as f32],
            id,
            rids,
            shown: TITLE_BTN_OFF,
            hovered: false,
            active,
        });
        let Some(rid0) = rids[TITLE_BTN_OFF] else {
            return;
        };
        let layer = Layer {
            id,
            z,
            visible: true,
            x: rect[0],
            y: rect[1],
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(rid0),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
    }

    /// bt3 系图集条裁剪(横排 N 段;btn_back_bt3 = 蓝/淡/橙 3 态,
    /// tab bt3n = 4 态;段宽 = 图宽/段数,rid = fnv(路径) 与段号混合)。
    fn load_atlas_segment(&mut self, path: &str, seg: usize, segs: usize) -> Option<ResourceId> {
        let data = self.index.read_cg_bytes(path)?;
        let img = image::load_from_memory(&data).ok()?;
        let seg_w = img.width() / segs as u32;
        let x0 = (seg as u32 * seg_w).min(img.width());
        let crop = img.crop_imm(x0, 0, seg_w, img.height());
        let rgba = crop.to_rgba8();
        let rid = ResourceId(
            0x5C_A000_0000 ^ (((fnv1a(path.as_bytes()) & 0xFFFF_FFFF) << 2) | seg as u64),
        );
        let b = self.backend.as_mut()?;
        b.load_image_rgba(rid, rgba.as_raw(), rgba.width(), rgba.height())
            .ok()?;
        self.loaded.insert(rid.0);
        Some(rid)
    }

    /// EXTRA 标签行(原生 yst00257:7 tab @ y=10;本实现布 4 钮 ——
    /// CG/BGM 可切,RP(SCENE)/MV 以 `_na` 暗段展示为禁用(功能未实装),
    /// st/wp/sv 素材包内缺失不布)。等距 247px 防重叠(原生 95px 步距的
    /// 图集裁剪方式未取证,布点为 Likely)。活动标签 = `_on` 单图,
    /// 非活动 = bt3n 图集第 0 段,悬停切 `_on`。
    fn open_tab_bar(&mut self, base: u64, active_cg: bool) {
        let (x0, y, tw, th, pitch) = (531.0, 10.0, 237.0, 53.0, 247.0);
        // CG 标签
        let cg_on = self.load_scenario_image("cgsys/extra/btn_tab_cgmode_on");
        let cg_off = if active_cg {
            cg_on
        } else {
            self.load_atlas_segment("cgsys/extra/btn_tab_cgmode_bt3n", 0, 4)
                .or(cg_on)
        };
        self.ui_button_rids(
            base + 0xA0,
            [x0, y, tw, th],
            [cg_off, cg_on, None],
            53,
            true,
        );
        // RP(SCENE)/MV:禁用展示(_na 暗段;不参与三态/点击)
        let rp = self
            .load_atlas_segment("cgsys/extra/btn_tab_rpmode_bt3n", 3, 4)
            .or_else(|| self.load_atlas_segment("cgsys/extra/btn_tab_rpmode_bt3n", 0, 4));
        self.ui_button_rids(
            base + 0xA2,
            [x0 + pitch, y, tw, th],
            [rp, rp, None],
            53,
            false,
        );
        let mv = self
            .load_atlas_segment("cgsys/extra/btn_tab_mvmode_bt3n", 3, 4)
            .or_else(|| self.load_atlas_segment("cgsys/extra/btn_tab_mvmode_bt3n", 0, 4));
        self.ui_button_rids(
            base + 0xA3,
            [x0 + 2.0 * pitch, y, tw, th],
            [mv, mv, None],
            53,
            false,
        );
        // BGM 标签
        let bgm_on = self.load_scenario_image("cgsys/extra/btn_tab_bgmmode_on");
        let bgm_off = if active_cg {
            self.load_atlas_segment("cgsys/extra/btn_tab_bgmmode_bt3n", 0, 4)
                .or(bgm_on)
        } else {
            bgm_on
        };
        self.ui_button_rids(
            base + 0xA1,
            [x0 + 3.0 * pitch, y, tw, th],
            [bgm_off, bgm_on, None],
            53,
            true,
        );
    }

    /// bt3 三态返回钮(蓝=常态/淡=悬停/橙=按下;图集裁剪)。
    fn open_back_button(&mut self, id: u64, rect: [f32; 4], z: i64) {
        let s0 = self.load_atlas_segment("cgsys/extra/btn_back_bt3", 0, 3);
        let s1 = self.load_atlas_segment("cgsys/extra/btn_back_bt3", 1, 3);
        let s2 = self.load_atlas_segment("cgsys/extra/btn_back_bt3", 2, 3);
        self.ui_button_rids(id, rect, [s0, s1, s2], z, true);
    }

    /// 子画面白字文本(无 backend = 无层,不 panic)。
    fn ui_text(&mut self, id: u64, text: &str, x: f32, y: f32, z: i64) {
        let Some(rid) = self.render_text_layer(text) else {
            // UI 文字排版失败 ⇒ 按钮/标签文字不显示(以前完全静默)。
            yuris_log::alert_once(
                module_path!(),
                format!("ui-text-fail:{text}"),
                format!(
                    "子画面 UI 文字排版失败,该文字不显示\n\
                     内容: {text:?}  层 id = {id:#x}\n\
                     处置: 检查 CJK 字体装载/字符覆盖"
                ),
            );
            return;
        };
        let layer = Layer {
            id,
            z,
            visible: true,
            x,
            y,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
    }

    /// 子画面纯色层(压暗/兜底底图;1×1 RGBA 拉伸)。
    fn ui_fill(&mut self, id: u64, color: [u8; 4], z: i64) {
        let rid = ResourceId(
            0x5C_F000_0000 | ((color[0] as u64) << 16) | ((color[1] as u64) << 8) | color[2] as u64,
        );
        if !self.loaded.contains(&rid.0) {
            if let Some(b) = self.backend.as_mut() {
                let _ = b.load_image_rgba(rid, &color, 1, 1);
                self.loaded.insert(rid.0);
            }
        }
        let layer = Layer {
            id,
            z,
            visible: true,
            x: 0.0,
            y: 0.0,
            scale_x: logical_w(),
            scale_y: logical_h(),
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
    }

    /// ev 全图解码 → 缩略图 RGBA 直传(cell 尺寸 cover 裁剪;解码失败跳过)。
    fn load_thumb(&mut self, id: u64, path: &str, x: f32, y: f32, w: f32, h: f32) {
        let Some(data) = self.index.read_cg_bytes(path) else {
            return;
        };
        let Ok(img) = image::load_from_memory(&data) else {
            return;
        };
        let thumb = img.resize_to_fill(w as u32, h as u32, image::imageops::FilterType::Triangle);
        let rgba = thumb.to_rgba8();
        let rid = ResourceId(0x5C_A000_0000 | (fnv1a(path.as_bytes()) & 0xFFFF_FFFF));
        let uploaded = match self.backend.as_mut() {
            Some(b) => b
                .load_image_rgba(rid, rgba.as_raw(), rgba.width(), rgba.height())
                .is_ok(),
            None => false,
        };
        if !uploaded {
            return;
        }
        self.loaded.insert(rid.0);
        let layer = Layer {
            id,
            z: 51,
            visible: true,
            x,
            y,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
    }

    /// 扫描 save/yskernel_*.json 存档(mtime 新在前;显示 = mtime + file/label)。
    fn scan_save_entries(&mut self) {
        self.save_entries.clear();
        let dir = self.game_dir.join("save");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut paths: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().and_then(|s| s.to_str()).map_or(false, |s| {
                    s.starts_with("yskernel_") && s.ends_with(".json")
                })
            })
            .collect();
        paths.sort_by_key(|p| {
            std::cmp::Reverse(
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            )
        });
        for p in paths {
            let stamp = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| fmt_unix_time(d.as_secs()))
                .unwrap_or_else(|| "----/--/-- --:--".to_string());
            let label = std::fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .map(|d| {
                    format!(
                        "{}  {} / {}",
                        stamp,
                        d["file"].as_str().unwrap_or("?"),
                        d["label"].as_str().unwrap_or("?")
                    )
                })
                .unwrap_or_else(|| "(损坏的存档)".to_string());
            self.save_entries.push((p, label));
        }
    }

    /// cg\ev\*.png 清单(剥根规范名;含 update 包同名,LIFO 读取时自动覆盖)。
    fn list_ev_paths(&self) -> Vec<String> {
        let mut set: Vec<String> = self
            .index
            .keys()
            .filter_map(|k| {
                // 剥根前缀走 VFS 的键级 API(不再硬切 1 字节;成果 136)。
                let s = k.strip_root().display();
                (s.contains(r"cg\ev\") && s.ends_with(".png")).then_some(s)
            })
            .collect();
        set.sort();
        set.dedup();
        set
    }

    /// BGM 曲目名(去 bgm\ 目录与 .ogg;case 保真排序)。
    fn list_bgm_tracks(&self) -> Vec<String> {
        let mut set: Vec<String> = self
            .audio_packs
            .keys()
            .filter_map(|k| {
                // 键已折叠大小写 ⇒ 判据直接用(成果 136)。
                let s = k.strip_root().display();
                let lower = s.to_ascii_lowercase();
                (lower.contains(r"bgm\") && lower.ends_with(".ogg")).then(|| {
                    s.rsplit('\\')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(".ogg")
                        .trim_end_matches(".OGG")
                        .to_string()
                })
            })
            .collect();
        set.sort_by_key(|t| t.to_ascii_lowercase());
        set.dedup_by_key(|t| t.to_ascii_lowercase());
        set
    }

    /// P3-1:LOAD 存档列表(back_load 整屏 + 存档行 + 戻る)。
    fn open_load(&mut self) {
        self.close_subui();
        self.subui = SubUi::Load;
        if let Some(rid) = self.load_scenario_image(r"cgsys/saveload/back_load") {
            // 铺满画布 = 画布尺寸 / 素材尺寸(**不是**画布 / 1920)。
            // 勘误(成果 124):此处原为 `LOGICAL_W / 1920.0`,在常量=1920 的
            // 年代恰好恒等于 1.0(「不缩放」);改成运行期 `logical_w()` 后
            // 变成 1100/1920 = 0.573 ⇒ 整屏 UI 缩到 57%。
            let (sx, sy) = self
                .backend
                .as_ref()
                .and_then(|b| b.image_size(rid.0))
                .map(|(iw, ih)| {
                    (
                        logical_w() / iw.max(1) as f32,
                        logical_h() / ih.max(1) as f32,
                    )
                })
                .unwrap_or((1.0, 1.0));
            let layer = Layer {
                id: UI_LOAD_BASE,
                z: 50,
                visible: true,
                x: 0.0,
                y: 0.0,
                scale_x: sx,
                scale_y: sy,
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(rid),
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(layer);
        } else {
            self.ui_fill(UI_LOAD_BASE, [8, 8, 14, 255], 50);
        }
        self.scan_save_entries();
        // 存档行(最多 8 行;半透明底板 + 白字 + 不可见命中钮)
        let row_labels: Vec<String> = self
            .save_entries
            .iter()
            .take(8)
            .map(|(_, l)| l.clone())
            .collect();
        for (i, label) in row_labels.iter().enumerate() {
            let y = 200.0 + i as f32 * 92.0;
            // 2×1 半透明深色板(拉伸为行底)
            let px = [16u8, 14, 24, 170, 16, 14, 24, 170];
            let plate = ResourceId(0x5C_7000_0100 + i as u64);
            if !self.loaded.contains(&plate.0) {
                if let Some(b) = self.backend.as_mut() {
                    let _ = b.load_image_rgba(plate, &px, 2, 1);
                    self.loaded.insert(plate.0);
                }
            }
            let plate_layer = Layer {
                id: UI_LOAD_BASE + 0x40 + i as u64,
                z: 51,
                visible: true,
                x: 320.0,
                y,
                scale_x: 1280.0,
                scale_y: 80.0,
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(plate),
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(plate_layer);
            self.ui_text(UI_LOAD_BASE + 0x10 + i as u64, label, 352.0, y + 18.0, 52);
            self.ui_buttons.push(TitleButton {
                rect: [320.0, y, 1280.0, 80.0],
                id: UI_LOAD_BASE + 0x80 + i as u64,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
        }
        if self.save_entries.is_empty() {
            self.ui_text(
                UI_LOAD_BASE + 0x10,
                "セーブデータがありません",
                760.0,
                480.0,
                52,
            );
        }
        self.ui_button(
            UI_LOAD_BASE + 1,
            [1660.0, 940.0, 186.0, 87.0],
            r"cgsys/saveload/btn_back",
            ["_off", "_on", "_over"],
            53,
        );
        yuris_log::debug!("[scenario] LOAD 画面(存档 {} 件)", self.save_entries.len());
    }

    /// P3-2:CG 鉴赏(原生 yst00257 = cgmode 屏,EXTRA 直达无落地菜单;
    /// cgmode/back 3×3 画格 + 顶部标签行 + bt3 返回钮)。
    fn open_extra_cg(&mut self, page: usize) {
        self.close_subui();
        self.subui = SubUi::ExtraCg;
        if self
            .load_scenario_image(r"cgsys/extra/cgmode/back")
            .is_none()
        {
            self.ui_fill(UI_CG_BASE, [234, 244, 248, 255], 50);
        }
        self.open_tab_bar(UI_CG_BASE, true);
        if self.ev_list.is_empty() {
            self.ev_list = self.list_ev_paths();
        }
        const PER: usize = 9; // 原生画格 3×3(cgmode/back 白格)
        let pages = self.ev_list.len().div_ceil(PER).max(1);
        let page = page.min(pages - 1);
        self.cg_page = page;
        let page_items: Vec<String> = self
            .ev_list
            .iter()
            .skip(page * PER)
            .take(PER)
            .cloned()
            .collect();
        // 画格锚点(cgmode/back 白格实测:列 277/736/1192 起、行 141/435/733 起,
        // 格 ~458×294;缩略图内缩 ~7px)
        for (i, path) in page_items.iter().enumerate() {
            let x = 284.0 + (i % 3) as f32 * 458.0;
            let y = 148.0 + (i / 3) as f32 * 296.0;
            self.load_thumb(UI_CG_BASE + 0x10 + i as u64, path, x, y, 445.0, 284.0);
            self.ui_buttons.push(TitleButton {
                rect: [x, y, 445.0, 284.0],
                id: UI_CG_BASE + 0x80 + i as u64,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
        }
        self.ui_text(
            UI_CG_BASE + 0x50,
            &format!("{} / {}  ({} CG)", page + 1, pages, self.ev_list.len()),
            640.0,
            984.0,
            52,
        );
        if page > 0 {
            self.ui_buttons.push(TitleButton {
                rect: [250.0, 950.0, 220.0, 100.0],
                id: UI_CG_PREV,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
            self.ui_text(UI_CG_BASE + 0x51, "前", 330.0, 985.0, 52);
        }
        if page + 1 < pages {
            self.ui_buttons.push(TitleButton {
                rect: [1350.0, 950.0, 220.0, 100.0],
                id: UI_CG_NEXT,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
            self.ui_text(UI_CG_BASE + 0x52, "次", 1430.0, 985.0, 52);
        }
        self.open_back_button(UI_CG_BACK, [1620.0, 930.0, 237.0, 80.0], 53);
        yuris_log::debug!(
            "[scenario] CG 鉴赏 {}/{}({} 件)",
            page + 1,
            pages,
            self.ev_list.len()
        );
    }

    /// P3-2:BGM 鉴赏入口(保持上次页码)。
    fn open_extra_bgm(&mut self) {
        self.open_extra_bgm_page(self.bgm_page);
    }

    /// P3-2:BGM 鉴赏指定页(原生 extra/back = EXTRAS 双列列表底;曲目点播,
    /// 戻る停止播放)。22 曲/页(2 列 × 11 行,对齐底图画线)。
    fn open_extra_bgm_page(&mut self, page: usize) {
        self.close_subui();
        self.subui = SubUi::ExtraBgm;
        if self.load_scenario_image(r"cgsys/extra/back").is_none() {
            self.ui_fill(UI_BGM_BASE, [234, 244, 248, 255], 50);
        }
        self.open_tab_bar(UI_BGM_BASE, false);
        if self.bgm_tracks.is_empty() {
            self.bgm_tracks = self.list_bgm_tracks();
        }
        const PER: usize = 22;
        let pages = self.bgm_tracks.len().div_ceil(PER).max(1);
        let page = page.min(pages - 1);
        self.bgm_page = page;
        let tracks: Vec<String> = self
            .bgm_tracks
            .iter()
            .skip(page * PER)
            .take(PER)
            .cloned()
            .collect();
        for (i, t) in tracks.iter().enumerate() {
            let col = i / 11;
            let row = i % 11;
            let x = 368.0 + col as f32 * 600.0;
            let y = 250.0 + row as f32 * 56.0;
            self.ui_text(UI_BGM_BASE + 0x10 + i as u64, t, x, y, 52);
            self.ui_buttons.push(TitleButton {
                rect: [x - 16.0, y - 8.0, 560.0, 50.0],
                id: UI_BGM_BASE + 0x80 + i as u64,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
        }
        if self.bgm_tracks.is_empty() {
            self.ui_text(UI_BGM_BASE + 0x10, "BGM なし", 880.0, 480.0, 52);
        }
        self.ui_text(
            UI_BGM_BASE + 0x50,
            &format!("{} / {}", page + 1, pages),
            900.0,
            984.0,
            52,
        );
        if page > 0 {
            self.ui_buttons.push(TitleButton {
                rect: [250.0, 950.0, 220.0, 100.0],
                id: UI_BGM_PREV,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
            self.ui_text(UI_BGM_BASE + 0x51, "前", 330.0, 985.0, 52);
        }
        if page + 1 < pages {
            self.ui_buttons.push(TitleButton {
                rect: [1350.0, 950.0, 220.0, 100.0],
                id: UI_BGM_NEXT,
                rids: [None, None, None],
                shown: TITLE_BTN_OFF,
                hovered: false,
                active: true,
            });
            self.ui_text(UI_BGM_BASE + 0x52, "次", 1430.0, 985.0, 52);
        }
        self.open_back_button(UI_BGM_BACK, [1620.0, 930.0, 237.0, 80.0], 53);
        yuris_log::debug!(
            "[scenario] BGM 鉴赏 {}/{}({} 曲)",
            page + 1,
            pages,
            self.bgm_tracks.len()
        );
    }

    /// P3-3:END 确认对话框(dialog_end + yes/no 三态钮)。
    fn open_confirm_end(&mut self) {
        self.close_subui();
        self.subui = SubUi::ConfirmEnd;
        self.ui_fill(UI_CONFIRM_BASE, [0, 0, 0, 140], 50);
        if let Some(rid) = self.load_scenario_image(r"cgsys/confirm/dialog_end") {
            let layer = Layer {
                id: UI_CONFIRM_BASE + 1,
                z: 51,
                visible: true,
                x: 694.0,
                y: 380.0,
                scale_x: 1.0,
                scale_y: 1.0,
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(rid),
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(layer);
        }
        self.ui_button(
            UI_CONFIRM_BASE + 2,
            [740.0, 470.0, 146.0, 45.0],
            r"cgsys/confirm/btn_yes",
            ["_off", "_on", "_over"],
            52,
        );
        self.ui_button(
            UI_CONFIRM_BASE + 3,
            [1040.0, 470.0, 146.0, 45.0],
            r"cgsys/confirm/btn_no",
            ["_off", "_on", "_over"],
            52,
        );
        yuris_log::debug!("[scenario] END 确认对话框");
    }

    /// 子画面点击路由(每帧至多一次;返回恒 None —— scenario 保持标题等待)。
    fn poll_subui(&mut self) {
        if !self.frame_clicked {
            return;
        }
        let (cx, cy) = self.cursor_logical;
        let hit = self.ui_buttons.iter().find(|b| {
            b.active
                && cx >= b.rect[0] as i64
                && cx < (b.rect[0] + b.rect[2]) as i64
                && cy >= b.rect[1] as i64
                && cy < (b.rect[1] + b.rect[3]) as i64
        });
        let Some(id) = hit.map(|b| b.id) else {
            return;
        };
        match self.subui {
            SubUi::Load => {
                if id == UI_LOAD_BASE + 1 {
                    self.play_sysse(TITLE_SE_CANCEL);
                    self.close_subui();
                } else if id >= UI_LOAD_BASE + 0x80 {
                    let i = (id - UI_LOAD_BASE - 0x80) as usize;
                    if let Some((path, _)) = self.save_entries.get(i) {
                        let path = path.clone();
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.request_load_path = Some(path);
                        self.request_title_load = true;
                    }
                }
            }
            SubUi::ExtraCg => {
                // 全图查看:任意点击返回分页
                if self.cg_view.take().is_some() {
                    let scene = self.bridge.scene_mut();
                    scene.hide_layer(UI_CG_BASE + 0x60);
                    scene.hide_layer(UI_CG_BASE + 0x61);
                    return;
                }
                match id {
                    UI_CG_TAB_BGM => {
                        // 标签:BGM 鉴赏
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.open_extra_bgm();
                    }
                    UI_CG_PREV => {
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.open_extra_cg(self.cg_page.saturating_sub(1));
                    }
                    UI_CG_NEXT => {
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.open_extra_cg(self.cg_page + 1);
                    }
                    UI_CG_BACK => {
                        self.play_sysse(TITLE_SE_CANCEL);
                        self.close_subui(); // 戻る → 标题(原生各屏独立返回)
                    }
                    id if (UI_CG_BASE + 0x80..UI_CG_BASE + 0x90).contains(&id) => {
                        let i = (id - UI_CG_BASE - 0x80) as usize;
                        const PER: usize = 9; // 原生画格 3×3
                        if let Some(path) = self.ev_list.get(self.cg_page * PER + i) {
                            let path = path.clone();
                            self.play_sysse(TITLE_SE_DECIDE);
                            self.show_cg_full(&path);
                        } else {
                            // 画格 id 命中但列表项为空(分页越界):点击无反应。
                            yuris_log::alert_once(
                                module_path!(),
                                format!("cg-grid-miss:{i}"),
                                format!(
                                    "CG 鉴赏画格点击无对应条目\n\
                                     画格序号 = {i}(页 {})  条目总数 = {}\n\
                                     处置: 分页/画格映射越界 —— 检查 ev_list 与 PER=9 的排布",
                                    self.cg_page,
                                    self.ev_list.len()
                                ),
                            );
                        }
                    }
                    unknown => {
                        yuris_log::alert_once(
                            module_path!(),
                            format!("subui-cg-unmapped:{unknown:#x}"),
                            format!(
                                "CG 鉴赏子画面的按钮 id 未映射,点击被忽略\n\
                                 id = {unknown:#x}\n\
                                 处置: 在 poll_subui 的 SubUi::ExtraCg 分支补映射"
                            ),
                        );
                    }
                }
            }
            SubUi::ExtraBgm => {
                match id {
                    UI_BGM_TAB_CG => {
                        // 标签:CG 鉴赏
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.open_extra_cg(self.cg_page);
                    }
                    UI_BGM_BACK => {
                        self.audio.stop_bgm();
                        self.play_sysse(TITLE_SE_CANCEL);
                        self.close_subui(); // 戻る → 标题(停止 BGM)
                    }
                    UI_BGM_PREV => {
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.open_extra_bgm_page(self.bgm_page.saturating_sub(1));
                    }
                    UI_BGM_NEXT => {
                        self.play_sysse(TITLE_SE_DECIDE);
                        self.open_extra_bgm_page(self.bgm_page + 1);
                    }
                    id if (UI_BGM_BASE + 0x80..UI_BGM_BASE + 0x90).contains(&id) => {
                        let i = (id - UI_BGM_BASE - 0x80) as usize;
                        const PER: usize = 22;
                        if let Some(t) = self.bgm_tracks.get(self.bgm_page * PER + i) {
                            let t = t.clone();
                            self.play_sysse(TITLE_SE_DECIDE);
                            match self.resolve_audio(&self.audio_packs, "", &t) {
                                Some(data) => self.audio.play_bgm(&t, data, None),
                                None => {
                                    yuris_log::alert_once(
                                        module_path!(),
                                        format!("bgm-miss:{t}"),
                                        format!(
                                            "EXTRA/BGM 曲目未找到(列表项无音)\n\
                                             曲目: {t}\n\
                                             处置: 检查 audio 包条目名(CP932/VFS 归一化)"
                                        ),
                                    );
                                }
                            }
                        } else {
                            yuris_log::alert_once(
                                module_path!(),
                                format!("bgm-grid-miss:{i}"),
                                format!(
                                    "BGM 鉴赏行点击无对应曲目\n\
                                     行序号 = {i}(页 {})  曲目总数 = {}\n\
                                     处置: 分页/行映射越界 —— 检查 bgm_tracks 与 PER=22 的排布",
                                    self.bgm_page,
                                    self.bgm_tracks.len()
                                ),
                            );
                        }
                    }
                    unknown => {
                        yuris_log::alert_once(
                            module_path!(),
                            format!("subui-bgm-unmapped:{unknown:#x}"),
                            format!(
                                "BGM 鉴赏子画面的按钮 id 未映射,点击被忽略\n\
                                 id = {unknown:#x}\n\
                                 处置: 在 poll_subui 的 SubUi::ExtraBgm 分支补映射"
                            ),
                        );
                    }
                }
            }
            SubUi::ConfirmEnd => {
                if id == UI_CONFIRM_BASE + 2 {
                    self.play_sysse(TITLE_SE_DECIDE);
                    self.request_quit = true; // はい:主循环 event_loop.exit()
                } else if id == UI_CONFIRM_BASE + 3 {
                    self.play_sysse(TITLE_SE_CANCEL);
                    self.close_subui();
                }
            }
            SubUi::None => {}
        }
    }

    /// CG 全图查看(ev 原图整屏;点击由 poll_subui 关闭)。
    fn show_cg_full(&mut self, path: &str) {
        let Some(rid) = self.load_scenario_image(path) else {
            yuris_log::warn!("[scenario] CG 全图未命中: {path}");
            return;
        };
        let (iw, ih) = self
            .backend
            .as_ref()
            .and_then(|b| b.image_size(rid.0))
            .unwrap_or((1920, 1080));
        let layer = Layer {
            id: UI_CG_BASE + 0x60,
            z: 60,
            visible: true,
            x: 0.0,
            y: 0.0,
            scale_x: logical_w() / iw as f32,
            scale_y: logical_h() / ih as f32,
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
        self.ui_text(UI_CG_BASE + 0x61, "クリックで戻る", 830.0, 1020.0, 61);
        self.cg_view = Some(rid);
    }

    /// 渲染台词窗底框(txspace 纹理 + 半透明底)到场景(z=80)。
    fn show_window_frame(&mut self) {
        let tex = self
            .load_scenario_image("cgsys/main/button/type1/tip_meswindow_txspace")
            .map(|r| (r, 1350u32, 200u32));
        let (rid, sw, sh) = match tex {
            Some((r, w, h)) => (r, w, h),
            None => (ResourceId(0x5C_0000_0004), 2u32, 2u32),
        };
        if tex.is_none() {
            // 回退:2×2 半透明黑
            if !self.loaded.contains(&rid.0) {
                if let Some(b) = self.backend.as_mut() {
                    let _ = b.load_image_rgba(
                        rid,
                        &[
                            10, 10, 16, 160, 10, 10, 16, 160, 10, 10, 16, 160, 10, 10, 16, 160,
                        ],
                        2,
                        2,
                    );
                    self.loaded.insert(rid.0);
                }
            }
        }
        let layer = Layer {
            id: SC_WIN,
            z: 80,
            visible: true,
            x: 0.0,
            y: logical_h() - 250.0,
            scale_x: logical_w() / sw as f32,
            scale_y: 250.0 / sh as f32,
            alpha: 0.9,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
    }
}

impl ScenarioHost for PlayerCore {
    fn reset_scene(&mut self) {
        // 与标题层清理统一(含 0x5C_7000_* 标题层 / 按钮 / 精灵 / 文本窗)
        self.reset_title_layers();
    }

    fn show_bg(&mut self, name: &str, _fade_ms: u64, color: [u8; 3]) {
        let trimmed = name.trim();
        let rid = self.load_scenario_image(trimmed);
        yuris_log::trace!(
            "[scenario] BG {trimmed:?} 解析={} color={color:?}",
            if rid.is_some() { "命中" } else { "纯色" }
        );
        let Some(rid) = rid else {
            // 纯色(white/black 等):上传 1×1 色图
            let rid = ResourceId(SC_BG);
            let px = vec![color[0], color[1], color[2], 255];
            if let Some(b) = self.backend.as_mut() {
                let _ = b.load_image(rid, &px);
            }
            self.loaded.insert(rid.0);
            let layer = Layer {
                id: SC_BG,
                z: 0,
                visible: true,
                x: 0.0,
                y: 0.0,
                scale_x: logical_w(),
                scale_y: logical_h(),
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(rid),
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(layer);
            return;
        };
        let (iw, ih) = self
            .backend
            .as_ref()
            .and_then(|b| b.image_size(rid.0))
            .unwrap_or((1920, 1080));
        // 原生比例 + cover 缩放 + 相机平移(**钳制到不露黑边**)。
        // 勘误(layout-fix-plan P1 实机):参考图背景是宣发合成(缩放/裁位),
        // 不能作引擎标定;旧「原生 1:1 居中 + 自由平移」在 2400×1200 上
        // 平移越界 → 右侧/底部黑带。改为 cover(scale = max)后钳制:
        //   x = clamp((1920-dw)/2 - cam.x*s, 1920-dw, 0)
        // 方向符号(cam.x>0 = 视口右移)与缩放倍率待实机对拍(Likely)。
        let (x, y, s) = bg_cover_transform(iw, ih, self.bg_cam);
        yuris_log::debug!(
            "[scenario] BG {trimmed:?} {iw}x{ih} → ({:.1},{:.1}) s={:.3}",
            x, y, s
        );
        let layer = Layer {
            id: SC_BG,
            z: 0,
            visible: true,
            x,
            y,
            scale_x: s,
            scale_y: s,
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
    }

    /// 背景相机位移:更新状态并同步已上屏 BG 层坐标(`\BG.CMXYZ`)。
    fn bg_camera(&mut self, x: i64, y: i64, z: i64) {
        self.bg_cam = (x, y, z);
        yuris_log::debug!("[scenario] BG.CMXYZ({x},{y},{z})");
        // 尺寸从已上屏 BG 层资源查(未上屏则仅记状态,下次 show_bg 生效)
        let size = self
            .bridge
            .scene()
            .layers
            .iter()
            .find(|l| l.id == SC_BG)
            .and_then(|l| l.resource)
            .and_then(|r| self.backend.as_ref().and_then(|b| b.image_size(r.0)));
        if let Some((iw, ih)) = size {
            let (nx, ny, _s) = bg_cover_transform(iw, ih, self.bg_cam);
            if let Some(layer) = scene_mut_layer(self.bridge.scene_mut(), SC_BG) {
                layer.x = nx;
                layer.y = ny;
            }
        }
    }

    /// 事件 CG 相机(P3 EV 实装前仅记录,不猜应用)。
    fn ev_camera(&mut self, x: i64, y: i64, z: i64) {
        yuris_log::debug!("[scenario] EV.CMXYZ({x},{y},{z})(EV 未实装,记录)");
    }

    /// `\WINDOWMODE(n)`:记录 + 日志(语义 Unknown;layout-fix-plan P2-3)。
    fn window_mode(&mut self, mode: i64) {
        self.window_mode = mode.clamp(0, 255) as u8;
        yuris_log::debug!("[scenario] WINDOWMODE({mode})(语义 Unknown,记录)");
    }

    fn show_sprite(&mut self, name: &str, path: Option<&str>, x: i64, y: i64, fade_ms: u64) {
        // \S:(x,y) = 左上角(0,0 = 全屏图原位),原生尺寸(成果 69 勘误)
        let key = path.unwrap_or(name);
        let Some(rid) = self.load_scenario_image(key) else {
            yuris_log::warn!("[scenario] 资源未命中: {key}");
            return;
        };
        let id = fnv1a(name.as_bytes());
        if !self.sprites.contains(&id) {
            self.sprites.push(id);
        }
        let alpha = if fade_ms > 0 { 0.0 } else { 1.0 };
        let layer = Layer {
            id,
            z: 10,
            visible: true,
            x: x as f32,
            y: y as f32,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
        if fade_ms > 0 {
            self.sprite_fades.push((id, self.anim_now(), fade_ms, false));
        }
    }

    fn show_tachie(&mut self, name: &str, x: i64, y: i64, fade_ms: u64) {
        // \T:立绘;x = 中心偏移,y = **顶边**,显示比例 = 0.93 × m_050 原生
        // (2026-09-12 原版截图对拍:三角色实测 0.930/0.930/0.929,corr≥0.97;
        // 引擎位移动画期位置会浮动,静止锚点=顶边 —— layout-fix-plan P0)。
        let Some(rid) = self.load_scenario_image(name) else {
            yuris_log::warn!("[scenario] 立绘未命中: {name}");
            return;
        };
        let (iw, ih) = self
            .backend
            .as_ref()
            .and_then(|b| b.image_size(rid.0))
            .unwrap_or((880, 1200));
        // 角色键 = 按角色占一层(变体切换换图,不叠层;P0 勘误)
        let id = fnv1a(tachie_char_key(name).as_bytes());
        let px = logical_w() / 2.0 + x as f32 - iw as f32 * TACHIE_SCALE / 2.0;
        let py = y as f32;
        // 部件变体近似:新图显著小于当前基图(如 7KB 表情/服装部件)→
        // 保留基图、只同步位置(引擎为基图+部件合成;部件偏移表未取证)。
        let existing = self
            .bridge
            .scene()
            .layers
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.resource)
            .and_then(|r| self.backend.as_ref().and_then(|b| b.image_size(r.0)));
        if let Some((ow, oh)) = existing {
            let old_px = (ow as u64) * (oh as u64);
            let new_px = (iw as u64) * (ih as u64);
            if new_px * 2 < old_px.max(1) {
                yuris_log::warn!(
                    "[scenario] 立绘部件变体 {name}: tex={iw}x{ih} < 基图 {ow}x{oh},保留基图并移动(近似)"
                );
                if let Some(layer) = scene_mut_layer(self.bridge.scene_mut(), id) {
                    layer.x = px;
                    layer.y = py;
                    layer.visible = true;
                }
                self.tachie_float
                    .entry(id)
                    .or_insert_with(|| (tachie_float_phase(id), (px, py)))
                    .1 = (px, py);
                return;
            }
        }
        if !self.sprites.contains(&id) {
            self.sprites.push(id);
        }
        yuris_log::debug!("[scenario] 立绘 {name}: tex={iw}x{ih} → ({px},{py}) fade={fade_ms}");
        let alpha = if fade_ms > 0 { 0.0 } else { 1.0 };
        let layer = Layer {
            id,
            z: 10,
            visible: true,
            x: px,
            y: py,
            scale_x: TACHIE_SCALE,
            scale_y: TACHIE_SCALE,
            alpha,
            rotation: 0.0,
            resource: Some(rid),
            ..Default::default()
        };
        self.bridge.scene_mut().upsert_layer(layer);
        self.tachie_float
            .entry(id)
            .or_insert_with(|| (tachie_float_phase(id), (px, py)))
            .1 = (px, py);
        if fade_ms > 0 {
            self.sprite_fades.push((id, self.anim_now(), fade_ms, false));
        }
    }

    /// `\T` 8 参位移动画(x1,y1,z1 → x2,y2,z2,ms):起点建层,线性补间到终点
    /// (引擎实测有「浮动」效果;缓动曲线 Unknown,取线性基线,layout-fix-plan P0)。
    fn move_tachie(&mut self, name: &str, x1: i64, y1: i64, x2: i64, y2: i64, ms: u64) {
        // 起点建层(与 show_tachie 同锚点/比例)
        self.show_tachie(name, x1, y1, 0);
        let id = fnv1a(tachie_char_key(name).as_bytes());
        if ms == 0 {
            self.show_tachie(name, x2, y2, 0);
            return;
        }
        let (iw, _ih) = self
            .bridge
            .scene()
            .layers
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.resource)
            .and_then(|r| self.backend.as_ref().and_then(|b| b.image_size(r.0)))
            .unwrap_or((880, 1200));
        let from = (
            logical_w() / 2.0 + x1 as f32 - iw as f32 * TACHIE_SCALE / 2.0,
            y1 as f32,
        );
        let to = (
            logical_w() / 2.0 + x2 as f32 - iw as f32 * TACHIE_SCALE / 2.0,
            y2 as f32,
        );
        self.sprite_moves.retain(|m| m.0 != id);
        self.sprite_moves.push((id, self.anim_now(), ms, from, to));
        self.tachie_float
            .entry(id)
            .or_insert_with(|| (tachie_float_phase(id), from))
            .1 = from;
    }

    fn hide_sprite(&mut self, name: &str, fade_ms: u64) {
        // 立绘按角色键占层:名称形态不同仍可命中(全名键 + 角色键都试)
        self.hide_sprite_id(fnv1a(name.as_bytes()), fade_ms);
        let cid = fnv1a(tachie_char_key(name).as_bytes());
        if cid != fnv1a(name.as_bytes()) {
            self.hide_sprite_id(cid, fade_ms);
        }
    }

    fn fade(&mut self, out: bool, ms: u64, color: [u8; 3]) {
        self.fade = Some(FadeState {
            out,
            start: self.anim_now(),
            ms,
            color,
        });
    }

    fn show_text(&mut self, _line_id: Option<u32>, lt: &str, _le: &str) {
        // 游戏自带消息窗载体在场时不再画近似底框(双重叠层修复,P8.2b 第 3 步;
        // 原生消息窗 = VM 部件自绘,scenario 只出文字)。判据(成果 158)=
        // Stage 实体表里存在消息窗载体 —— 与 VM 通道**同一真值源**。
        let has_native_win = self
            .stage
            .has_matching(|n, f| is_message_window_carrier(&String::from_utf8_lossy(n), f));
        if !has_native_win {
            self.show_window_frame();
        }
        // 繁→简(显示层转换,不改剧本数据;fast2s 单字映射 + 词汇表)
        let lt: String = fast2s::convert(lt);
        // 文本与窗态同批进 Stage(单一文本路径;字体/槽池经宿主回调)。
        self.stage.set_canvas(logical_w(), logical_h());
        let cmds = [
            yuris_stage::Command::MessageWindow(true),
            yuris_stage::Command::Text {
                layer: SC_TEXT,
                content: lt,
            },
        ];
        let scene = self.bridge.scene_mut();
        let mut host = StageAssets {
            index: &self.index,
            backend: self.backend.as_mut(),
            loaded: &mut self.loaded,
            font: &self.font,
            text_px: self.vm.font_size(),
            text_slots: &mut self.text_slots,
            text_seq: &mut self.text_slot_seq,
            snap_seq: &mut self.snap_seq,
            paint_seq: 0,
        };
        self.stage.apply(&cmds, scene, &mut host);
    }

    fn clear_text(&mut self) {
        self.stage.clear_text(self.bridge.scene_mut());
    }

    fn play_voice(&mut self, name: &str) {
        if let Some(data) = self.resolve_audio(&self.audio_packs, "voice\\", name) {
            self.audio.play_voice(name, data);
        } else {
            yuris_log::warn!("[audio] voice 未命中: {name}");
        }
    }

    fn play_bgm(&mut self, name: &str, volume_permille: Option<i64>) {
        if name.trim().is_empty() {
            // \BGM(,800):仅调音量
            self.audio.play_bgm("__cur__", Vec::new(), volume_permille);
            return;
        }
        if let Some(data) = self.resolve_audio(&self.audio_packs, "bgm\\", name) {
            self.audio.play_bgm(name, data, volume_permille);
        } else {
            yuris_log::warn!("[audio] bgm 未命中: {name}");
        }
    }

    fn play_se(&mut self, name: &str) {
        if let Some(data) = self.resolve_audio(&self.audio_packs, "se\\", name) {
            self.audio.play_se(name, data);
        } else {
            yuris_log::warn!("[audio] se 未命中: {name}");
        }
    }

    fn title_screen(&mut self) {
        // 内置标题:eyecatch/st 分层素材组合(bg01a + sir/han + logo + 真实按钮列)
        // 布局对齐真机截图(用户 2026-09-05 提供):银发左 / 双马尾中 / logo 右上 / 按钮右下
        self.reset_title_layers();
        // 标题接管视觉:清空 Stage 全部实体(厂商期已门控不建,此处清
        // 游戏中返回标题的可能残留;含消息窗/文本态复位)+ 置 title_seen。
        let scene = self.bridge.scene_mut();
        self.stage.kill_all(scene);
        // ADV 主按钮行隐藏并失效(标题接管视觉;回正篇经 rebuild_vm_ui 重建)
        for b in &self.adv_buttons {
            scene.hide_layer(b.id);
        }
        self.adv_buttons.clear();
        self.title_seen = true;
        // 全屏底层(path, x, y, id)→ 拉伸全屏
        let layers: [(&str, i64, i64, u64); 5] = [
            ("eyecatch/st/bg01a", 0, 0, 0x5C_7000_0000),
            ("eyecatch/st/tet", 0, 0, 0x5C_7000_0001),
            ("eyecatch/st/sir", -1058, 0, 0x5C_7000_0002),
            ("eyecatch/st/han", -86, 0, 0x5C_7000_0003),
            ("eyecatch/st/logo", 1046, -255, 0x5C_7000_0004),
        ];
        // 按钮列:三态素材 `cgsys/title/btn_{名}_{off,on,over}`(成果 78 图像
        // 实证);默认显示 `_off` 常态,悬停/按下由 update_title_buttons 切换。
        // 坐标/绑定 = yst00259 es.BT.XY.SET/es.BT.SET 原生真值(成果 79):
        // arasuji→BTN.START(2)、start→BTN.START(1)、extra(1314)/config(1477,
        // 未实装)/end(1640) 同排 y=745。
        let buttons: [(&str, i64, i64, u64); 6] = [
            ("cgsys/title/btn_arasuji", 1393, 384, 0x5C_7000_000A),
            ("cgsys/title/btn_start", 1393, 439, 0x5C_7000_0005),
            ("cgsys/title/btn_load", 1393, 533, 0x5C_7000_0006),
            ("cgsys/title/btn_lastload", 1393, 627, 0x5C_7000_0007),
            ("cgsys/title/btn_extra", 1314, 745, 0x5C_7000_0008),
            ("cgsys/title/btn_end", 1640, 745, 0x5C_7000_0009),
        ];
        // 无存档判定(P1-4):快存文件不存在 → CONTINUE 用 `_na` 灰化态且
        // 点击无效(原生 es.BT 按 save/*.sd 存在性条件注册 BTN.LLOAD vs
        // BTN.LLOAD.NA 两个同位按钮,成果 79;本实现以快存文件为判据)。
        let has_save = self
            .game_dir
            .join("save")
            .join("yskernel_qsave.json")
            .is_file();
        let white = ResourceId(0x5C_0000_0005);
        if !self.loaded.contains(&white.0) {
            if let Some(b) = self.backend.as_mut() {
                let _ = b.load_image(white, &[255, 255, 255, 255]);
                self.loaded.insert(white.0);
            }
        }
        let bgw = Layer {
            id: SC_BG,
            z: 0,
            visible: true,
            x: 0.0,
            y: 0.0,
            scale_x: logical_w(),
            scale_y: logical_h(),
            alpha: 1.0,
            rotation: 0.0,
            resource: Some(white),
            ..Default::default()
        };
        {
            let scene = self.bridge.scene_mut();
            scene.upsert_layer(bgw);
        }
        for (path, x, y, id) in layers {
            let Some(rid) = self.load_scenario_image(path) else {
                yuris_log::warn!("[scenario] 标题层未命中: {path}");
                continue;
            };
            let (iw, ih) = self
                .backend
                .as_ref()
                .and_then(|b| b.image_size(rid.0))
                .unwrap_or((1920, 1080));
            let layer = Layer {
                id,
                z: 1,
                visible: true,
                x: x as f32,
                y: y as f32,
                scale_x: logical_w() / iw as f32,
                scale_y: logical_h() / ih as f32,
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(rid),
                ..Default::default()
            };
            let scene = self.bridge.scene_mut();
            scene.upsert_layer(layer);
        }
        for (base, x, y, id) in buttons {
            // CONTINUE 无存档 → `_na` 单素材灰化(无三态/点击无效,成果 79)。
            let na = !has_save && base.ends_with("btn_lastload");
            let mut rids: [Option<ResourceId>; 3] = [None, None, None];
            if na {
                rids[TITLE_BTN_OFF] = self.load_scenario_image(&format!("{base}_na"));
            } else {
                // 三态预载(off/on/over;单态失败 = None,不切该态)
                for (i, suffix) in ["_off", "_on", "_over"].iter().enumerate() {
                    rids[i] = self.load_scenario_image(&format!("{base}{suffix}"));
                }
            }
            let Some(rid0) = rids[TITLE_BTN_OFF] else {
                yuris_log::warn!("[scenario] 标题按钮未命中: {base}_off");
                continue;
            };
            let (iw, ih) = self
                .backend
                .as_ref()
                .and_then(|b| b.image_size(rid0.0))
                .unwrap_or((317, 76));
            // 命中区(成果 76):原生尺寸,左上角 (x,y)
            self.title_buttons.push(TitleButton {
                rect: [x as f32, y as f32, iw as f32, ih as f32],
                id,
                rids,
                shown: TITLE_BTN_OFF,
                active: !na,
                hovered: false,
            });
            let layer = Layer {
                id,
                z: 1,
                visible: true,
                x: x as f32,
                y: y as f32,
                scale_x: 1.0,
                scale_y: 1.0,
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(rid0),
                ..Default::default()
            };
            let scene = self.bridge.scene_mut();
            scene.upsert_layer(layer);
        }
        yuris_log::debug!(
            "[scenario] 标题画面(eyecatch 分层组合;按钮菜单:ARASUJI/START/LOAD/LASTLOAD/EXTRA/END; \
             原生 es.BT 坐标;存档={has_save};三态 _off/_on/_over)"
        );
    }

    fn poll_choice(&mut self, count: usize) -> Option<usize> {
        if !self.frame_clicked {
            return None;
        }
        let (cx, cy) = self.cursor_logical;
        let Some(choices) = &self.choices else {
            return None;
        };
        for (i, (_, rect)) in choices.iter().enumerate().take(count.max(1)) {
            let [x, y, w, h] = *rect;
            if cx >= x as i64 && cx < (x + w) as i64 && cy >= y as i64 && cy < (y + h) as i64 {
                yuris_log::debug!("[scenario] 选择 {i}");
                return Some(i);
            }
        }
        let _ = count;
        None
    }

    fn show_choices(&mut self, choices: &[String]) {
        // 竖排文本按钮(居中);命中区记录进 self.choices
        let mut rects = Vec::new();
        for (i, text) in choices.iter().enumerate() {
            let Some(rid) = self.render_text_layer(text) else {
                continue;
            };
            let (tw, th) = self
                .backend
                .as_ref()
                .and_then(|b| b.image_size(rid.0))
                .unwrap_or((600, 60));
            let scale = 1.4f32;
            let w = tw as f32 * scale;
            let h = th as f32 * scale;
            let x = (logical_w() - w) / 2.0;
            let y = 260.0 + i as f32 * (h + 40.0);
            // 暗色底板(白字可读)
            let bg_rid = ResourceId(0x5C_4000_0000 + i as u64);
            if !self.loaded.contains(&bg_rid.0) {
                if let Some(b) = self.backend.as_mut() {
                    let px = vec![20u8, 20, 30, 210];
                    let _ = b.load_image_rgba(bg_rid, &px, 1, 1);
                    self.loaded.insert(bg_rid.0);
                }
            }
            let bg_layer = Layer {
                id: 0x5C_5000_0000 + i as u64,
                z: 94,
                visible: true,
                x: x - 30.0,
                y: y - 20.0,
                scale_x: w + 60.0,
                scale_y: h + 40.0,
                alpha: 0.85,
                rotation: 0.0,
                resource: Some(bg_rid),
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(bg_layer);
            let layer = Layer {
                id: 0x5C_3000_0000 + i as u64,
                z: 95,
                visible: true,
                x,
                y,
                scale_x: scale,
                scale_y: scale,
                alpha: 1.0,
                rotation: 0.0,
                resource: Some(rid),
                ..Default::default()
            };
            self.bridge.scene_mut().upsert_layer(layer);
            rects.push((text.clone(), [x, y, w, h]));
        }
        self.choices = Some(rects);
    }

    fn clear_choices(&mut self) {
        let scene = self.bridge.scene_mut();
        if let Some(cs) = &self.choices {
            for i in 0..cs.len() {
                scene.hide_layer(0x5C_3000_0000 + i as u64);
                scene.hide_layer(0x5C_5000_0000 + i as u64);
            }
        }
        self.choices = None;
    }

    fn global(&self, slot: usize) -> i64 {
        // \GO.G.IF 的 G=n:仿真内部全局槽(成果 78 勘误:原 @50[n] 映射被
        // 运行时证伪 —— @50 dims=[1],idx≥1 越界;引擎真值存储 Unknown,
        // 待 es.BT.* 宏链逆向(B5)。本实现仅要求写入/读取内部自洽)。
        self.globals.get(&slot).copied().unwrap_or(0)
    }

    fn set_global(&mut self, slot: usize, value: i64) {
        // \TITLE 按钮选择写入(成果 78);与 global() 同一仿真内部槽。
        self.globals.insert(slot, value);
        yuris_log::trace!("[scenario] 写 G{slot}={value}");
    }

    fn log(&mut self, msg: &str) {
        yuris_log::debug!("[scenario] {msg}");
    }

    fn poll_title_menu(&mut self) -> Option<TitleMenuAction> {
        // P3 子画面打开中:点击全部由子画面消费,scenario 保持标题等待。
        if self.subui != SubUi::None {
            self.poll_subui();
            return None;
        }
        if !self.frame_clicked {
            return None;
        }
        let (cx, cy) = self.cursor_logical;
        for b in &self.title_buttons {
            if !b.active {
                continue; // `_na` 灰化态:点击无效(原生 BTN.LLOAD.NA,成果 79)
            }
            let [x, y, w, h] = b.rect;
            if cx >= x as i64 && cx < (x + w) as i64 && cy >= y as i64 && cy < (y + h) as i64 {
                let act = match b.id {
                    0x5C_7000_000A => TitleMenuAction::Outline,
                    0x5C_7000_0005 => TitleMenuAction::Start,
                    0x5C_7000_0006 => TitleMenuAction::Load,
                    0x5C_7000_0007 => TitleMenuAction::LastLoad,
                    0x5C_7000_0008 => TitleMenuAction::Extra,
                    0x5C_7000_0009 => TitleMenuAction::End,
                    unmapped => {
                        // **未知标题按钮 id**:点击被吞掉(不进任何菜单动作)。
                        // 以前是 `_ => continue` 完全静默 —— 用户只看到
                        // 「点了没反应」。现在按 id 去重打横幅。
                        yuris_log::alert_once(
                            module_path!(),
                            format!("title-button-id:{unmapped:#x}"),
                            format!(
                                "标题按钮 id 未映射,点击被忽略\n\
                                 id = {unmapped:#x}  矩形 = {:?}\n\
                                 处置: 在 poll_title_menu 的 id→动作表补映射",
                                b.rect
                            ),
                        );
                        continue;
                    }
                };
                yuris_log::debug!("[scenario] 标题按钮 id={:#x} → {act:?}", b.id);
                self.play_sysse(TITLE_SE_DECIDE); // 决定音(P2-2,成果 80)
                return Some(act);
            }
        }
        None
    }

    fn title_load(&mut self) {
        // P3-1(成果 81):LOAD 存档列表画面(选中槽位经 request_load_path,
        // Player::tick 消费走 restore_from;读档完成 start() 重置等待)。
        self.open_load();
    }

    fn title_extra(&mut self) {
        // P3-2(成果 81):EXTRA → CG 鉴赏屏(原生 yst00257 流程:无落地菜单,
        // 顶部标签切 CG/BGM 模式)。
        self.open_extra_cg(0);
    }

    fn request_quit(&mut self) {
        // P3-3(成果 81):END → 确认对话框(はい 才置 request_quit 真退出)。
        self.open_confirm_end();
    }
}

/// `yuris-input` 快照 → VM 快照（纯数据搬运；两个 crate 互不依赖）。
///
/// `engine_version` 决定**采纳哪些输入 id**：0.494/0.495 用 SDK 完整表
/// （133..=547）；其它版本只采纳样本已实证的光标 id —— 0.495 的 id 表
/// 不能无条件套到 v555（样本 `@409` 是游戏自己的字符串变量）。
fn snapshot_from_input(
    s: &yuris_input::InputSnapshotData,
    engine_version: u32,
) -> yuris_vm::InputSnapshot {
    let claimed = if engine_version == 494 || engine_version == 495 {
        yuris_vm::InputSnapshot::sdk_ids()
    } else {
        yuris_vm::InputSnapshot::cursor_ids()
    };
    yuris_vm::InputSnapshot {
        claimed,
        scalar: s.scalar.clone(),
        pad: s.pad.clone(),
        writable: s.writable.clone(),
        down: s.down.clone(),
        inp: s.inp.clone(),
    }
}

impl PlayerCore {
    /// 本帧输入开始：提交状态机、同步视口/设置、生成 VM 快照。
    ///
    /// 顺序固定：`InputState::tick`（四态计数 ±1）→ 光标派生 → 快照 →
    /// `vm.set_input_snapshot`。脚本在同一帧读到本帧状态。
    pub fn begin_input_frame(&mut self) {
        // 逻辑画布与渲染同源（`yuris_render::logical_w/h`）。
        self.input.set_logical_size(logical_w(), logical_h());
        if let Some(b) = self.backend.as_ref() {
            let (w, h) = b.surface_size();
            if w > 0 && h > 0 {
                self.input.resize(w, h);
            }
        }
        // VM 里累计的 INPUT/MOUSE 设置 → 输入层。
        let s = self.vm.input_settings().clone();
        self.input.apply_mouse_settings(
            s.mouse_mode,
            s.mouse_time_ms,
            s.move_reactive,
            s.click_reactive,
            s.wheel_reactive,
            s.cursor,
        );
        if let Some(v) = s.keyboard {
            self.input
                .set_source_enabled(yuris_input::InputSource::Keyboard, v);
        }
        if let Some(v) = s.mouse {
            self.input
                .set_source_enabled(yuris_input::InputSource::Mouse, v);
        }
        if let Some(v) = s.pad {
            self.input
                .set_source_enabled(yuris_input::InputSource::Pad, v);
        }
        if let Some((x, y)) = s.warp {
            self.input.request_warp(x as i32, y as i32);
        }
        self.input.tick();
        // 派生缓存：输入层有视口时以输入层为准（外部直接写
        // `cursor_logical` 的旧路径在窗口未初始化时仍然有效）。
        if self.input.viewport().window_w > 0 {
            self.cursor_logical = self.input.cursor();
        }
        // 同步 VM 的 `input_cursor`：`CGINFO ONMOUSE` 的坐标缺省与
        // 旧求值路径（`@{133}/{138}` 的 fallback）都读它。
        self.vm
            .set_input_cursor(self.cursor_logical.0, self.cursor_logical.1);
        let press_name = self.input.last_press_name().map(|s| s.to_string());
        let latches = self.input.take_latches();
        if latches.clicked {
            self.clicked = true;
            self.frame_clicked = true;
        }
        // 兼容 `$55[1]` 键名串：引擎的按钮链在 s190 读 `@409 = $55[1]`，
        // 实测确定性值 "MOUSE_L"。本内核没有 DIALOG 返回串/输入子系统的
        // 写入者（PROGRESS 成果 51 记 Unknown），故在**按下边沿**把键名
        // 广播进 `$55`（全帧 + 全局；见 `GroupVm::inject_input_key`）。
        if let Some(name) = press_name {
            let _ = self.vm.inject_input_key(name.as_bytes());
            self.key_pulse = true;
            self.key_pulse_frames = 0;
        }
        // **引擎**版本(不是当前脚本的编译器版本):混版本封包里脚本会切表,
        // 输入子系统的系统变量映射只认引擎本身。
        let engine_version = self.vm.engine_version();
        // 诊断开关(取证用):`YURIS_MUTE_INPUT=1` 不喂输入快照 —— 判定
        // 「boot 期 config 点亮是否由输入面(MOUSECHECK/悬停态)驱动」。
        // 缺省关闭,行为与原版一致。
        if std::env::var_os("YURIS_MUTE_INPUT").is_none() {
            self.vm.set_input_snapshot(snapshot_from_input(
                &self.input.snapshot(),
                engine_version,
            ));
        }
    }

    /// 本帧输入收尾：把脚本写入回灌状态机。
    pub fn end_input_frame(&mut self) {
        for (id, idx, v) in self.vm.take_input_writes() {
            let Some(n) = v.as_int_opt() else {
                // **非整数值写输入系统变量 = 被丢弃的写入**(以前静默)。
                yuris_log::alert_once(
                    module_path!(),
                    format!("input-write-nonint:{id}"),
                    format!(
                        "脚本向输入系统变量写入了非整数,写入被丢弃\n\
                         id = {id}  下标 = {idx:?}  值 = {v:?}\n\
                         处置: 输入系统变量只接受 INT;此写入不生效"
                    ),
                );
                continue;
            };
            if id == 144 {
                if let Some(code) = idx {
                    if let Ok(code) = u16::try_from(code) {
                        self.input.write_inp(code, n);
                    }
                }
            } else {
                self.input.write_sysvar(id, n);
            }
        }
    }
}

/// 播放器:播放核心 + 剧本播放器 + 窗口;平台壳持有它并每帧调 [`Player::tick`]。
pub struct Player {
    /// 游戏安装目录(封包与 `save/` 路径基准)。
    #[allow(dead_code)]
    pub game_dir: std::path::PathBuf,
    /// 播放核心(VM / 场景投影 / 输入 / 音频)。
    pub core: PlayerCore,
    /// scenario 剧本播放器(明文 `.txt` 运行器)。
    pub scenario: ScenarioPlayer,
    /// winit 窗口(未建窗或建窗失败为 `None`)。
    pub window: Option<Arc<winit::window::Window>>,
    /// 上一帧开始时刻(帧耗时统计用)。
    pub last_frame: Instant,
    /// 上次 present 完成时刻(≈ vblank;帧节拍锚点,成果 127)。
    ///
    /// **为什么锚它**:帧节拍若从「上一帧开始时刻」+16.6ms 起算,工作
    /// (VM tick + 渲染)在等待结束后才开工,present 必然错过本周期 vblank
    /// 再多等一整段(实机 8s bench:帧周期 50ms,其中 ~35ms 是 vsync 空等)。
    /// 锚到 present(Fifo 下返回时≈vblank)+间隔-3ms 起工 ⇒ 工作落在
    /// 同一个 16.6ms 窗口内,present 恰好赶上下一个 vblank。
    pub last_present: Instant,
    /// **帧网格的下一拍时刻**(固定节拍;成果 162)。
    ///
    /// 与 [`last_present`](Self::last_present) 的区别:后者是"上一帧何时
    /// 画完",本字段是"下一帧应当何时开工"。用固定网格而非
    /// `last_present + gap`:后者把每帧的工作耗时累加进间隔(实测目标
    /// 16.6ms 实际 18~27ms)。网格落后超过一拍时从当前时刻重建,不追赶。
    pub next_frame_at: Instant,
}

impl Player {
    /// 推进一帧:错误窗优先 → 输入帧提交 → VM 泵 → 场景投影。
    pub fn tick(&mut self) {
        // **崩坏级错误**：画布中央错误窗优先于一切游戏推进。错误窗自身
        // 由渲染循环持续 present；这里只负责排版/撤层。
        if self.core.update_fatal_overlay() {
            return;
        }
        // ---- P9.2 输入帧提交（必须先于 VM 泵）----
        //
        // 平台壳在两次 tick 之间把 `PlatformEvent` 推进 `core.input`；
        // 这里提交为引擎状态（四态计数器 ±1），生成本帧 VM 快照，然后
        // 才让脚本读到 1 / -1 / 2 / 3…。
        self.core.begin_input_frame();
        let in_title = self.scenario.in_title_menu();
        let core = &mut self.core;
        // **官方 vsync 链**(成果 164/168):一次 tick = 一个 do-while 链。
        //
        // 引擎真值(`FUN_004040e8`):清续帧标志 → 逻辑帧++ → 步进扁平任务链
        // (每帧每任务一步、泵到让出、无条数预算)→ TASK 开火则立即再跑一个
        // 完整逻辑帧。**链期间不上屏**,呈现 = 链末状态。旧实现「1 逻辑帧 =
        // 1 上屏」与 `FRAME_GROUP_BUDGET` 人为切批都由此退役。
        //
        // 时钟口径:u32 绝对毫秒(`timeGetTime` 等价物;回绕语义一致)。
        let prof = std::env::var("YURIS_TICK_PROF").is_ok();
        let t0 = if prof { Some(Instant::now()) } else { None };
        // **逻辑时钟(成果 172)**:`now_ms` 取 VM 自己的逻辑帧时钟,
        // 不取墙钟 —— 渲染帧率抖动(均值 24ms、尖峰 1.5s)不再改变脚本
        // 时间轴。缺省启用;`YURIS_WALL_CLOCK=1` 可回退墙钟对拍。
        if !core.vm.logical_clock() && std::env::var_os("YURIS_WALL_CLOCK").is_none() {
            core.vm.set_logical_clock(true);
        }
        // **动画时间 = 真实时间**(见 `anim_now` 的文档)。
        //
        // 这里只需按帧推进 `anim_frames` 供 `YURIS_FRAME_ANIM=1` 的
        // 确定性取证模式使用;正常游玩时 `anim_now()` 直接读墙钟,
        // 帧率高低都不影响动画时长。
        core.advance_anim_clock_by(1);
        core.vm.advance_frame_clock_by(1);
        let chain = core.vm.run_frame_chain(core.vm.now_ms_u32());
        if std::env::var_os("YURIS_TRACE_WAIT").is_some() {
            yuris_log::trace_forced!(
                "[chain] logical_frames={} tasks_stepped={} commands={} errors={}",
                chain.logical_frames,
                chain.tasks_stepped,
                chain.commands_pumped,
                chain.errors.len()
            );
        }
        for e in &chain.errors {
            // **2026-10-17 用户要求**:任务/主线执行器报错以前只落 debug
            // (默认等级下**完全不可见**),strict 挂起在实机上表现为
            // 「画面不动、日志没有根因」。现在一律升级为**致命错误**:
            // 下一帧 `update_fatal_overlay` 把错误窗盖在画布中央。
            yuris_log::report_fatal(
                module_path!(),
                "脚本执行中断 / VM ERROR",
                format!("{}", e),
            );
        }
        if yuris_log::has_fatal() {
            // 本帧已产生崩坏级错误:立即停止推进(不再跑 scenario/按钮),
            // 但仍回灌输入状态,避免下一帧状态机残留。
            core.end_input_frame();
            return;
        }
        let t1 = if prof { Some(Instant::now()) } else { None };
        // VM UI 层门控(P8.2b):进过标题且非标题等待期才放行;
        // false→true 沿 = 进入游戏 → 按注册表重放(boot 期被过滤的部件)
        // 门控(黑屏勘误,2026-10-13):旧式 `title_seen && !in_title_menu()` 的两个
        // 输入**都来自 scenario 层** —— 而 scenario 层就是画面本身的驱动者。
        // 对没有明文剧本的作品(单包 YSTB,`scenario 文件 0 个`),title_seen
        // 永不为真 ⇒ 门永不开 ⇒ 一个图层都不建 ⇒ **画面恒黑**。
        // 无 scenario 时改为「VM 的 CG 就是画面」。
        let allow_vm_ui = if self.scenario.is_empty() {
            true
        } else {
            core.title_seen && !self.scenario.in_title_menu()
        };
        // 「画面谁画」:只有**明文剧本层有内容**时才由播放器接管(内置标题 +
        // 状态件 denylist + 注册表重放);否则交还给游戏自己 —— 剧本以编译后的
        // `ysbin\yst*.ybn` 存在包里、由 VM 执行,播放器那套拿不到东西。
        let builtin_title = !self.scenario.is_empty();
        // 注册表重放只在播放器接管时做:它把 VM 的 CG 注册表全量回放上屏。
        // 游戏自己接管时不能重放 —— 会把游戏已经 CGEND 掉的界面又贴回来。
        if allow_vm_ui && !core.vm_ui_allowed_prev && builtin_title {
            core.rebuild_vm_ui();
        }
        core.vm_ui_allowed_prev = allow_vm_ui;
        core.consume_events(allow_vm_ui, builtin_title);
        let t2 = if prof { Some(Instant::now()) } else { None };
        core.update_fade();
        core.update_sprite_fades();
        core.update_sprite_moves(); // \T 位移动画(P0)
                                    // ADV 主行:点击消费(禁触台词推进;动作路由 = P9.2 待实装)
        if core.clicked && core.subui == SubUi::None && !in_title {
            if let Some(i) = core.adv_button_hit() {
                core.clicked = false;
                core.play_sysse(TITLE_SE_DECIDE);
                yuris_log::debug!("[adv] 主行按钮 #{i} 命中(动作未实装)");
            }
        }
        // **点击边沿诊断(无条件版,`YURIS_TRACE_CLICK=1`)**:
        // 在 take 之前记录"这一帧到底有没有点击",不依赖后续任何 gate。
        if std::env::var_os("YURIS_TRACE_CLICK").is_some() {
            yuris_log::trace_forced!(
                "[click-edge] core.clicked={} frame_clicked={} latches_done in_title={in_title} subui={:?}",
                core.clicked,
                core.frame_clicked,
                core.subui
            );
        }
        let clicked = std::mem::take(&mut core.clicked);
        // **点击去向诊断(2026-10-17)**:无明文剧本的作品点击全程静默 ——
        // 实机「点了没反应,日志也没提示」时无法判断点击去了哪。此处显式
        // 说明路由:输入层每帧快照,VM 侧 `@_MOUSE_L`/`@_INP(code)` 族读取
        // (ADV 主行命中在上方已各自落日志;scenario 非空时由剧本层吃)。
        if clicked && !in_title && core.subui == SubUi::None && self.scenario.is_empty() {
            yuris_log::info!("[click] 点击 → 输入层快照(YSTB 侧读 @_MOUSE_L/_INP 族)");
        }
        // **点击响应诊断**(`YURIS_TRACE_CLICK=1`):点击当帧的链产出。
        // 用户报告「点了要停一下才显示」—— 需要区分是
        // (a) 重绘节拍慢(已由 CLI 侧重置帧网格修)还是
        // (b) 点击那帧 VM 没推进(脚本要等下一帧才消费输入)。
        if clicked && std::env::var_os("YURIS_TRACE_CLICK").is_some() {
            yuris_log::trace_forced!(
                "[click] 帧内点击: chain frames={} cmds={} tasks={} events={}",
                chain.logical_frames,
                chain.commands_pumped,
                chain.tasks_stepped,
                core.vm.events().len()
            );
        }
        self.scenario.tick(core, clicked);
        let t3 = if prof { Some(Instant::now()) } else { None };
        if core.subui == SubUi::None {
            core.update_title_buttons(); // 标题按钮三态(悬停/按下帧;成果 78)
            if !in_title {
                core.update_adv_buttons(); // ADV 主行三态(layout-fix-plan P2)
            }
        } else {
            core.update_ui_buttons(); // 子画面按钮三态(成果 81)
        }
        let t4 = if prof { Some(Instant::now()) } else { None };
        // 标题 LOAD:tick 内置请求,此处消费(scenario 已在快读中被 start() 重置)
        if core.request_title_load {
            core.request_title_load = false;
            core.frame_clicked = false; // 防快读后首帧被同一次点击推进
            // 本帧输入写入仍要回灌（否则钳制后的状态会丢）。
            core.end_input_frame();
            let path = core
                .request_load_path
                .take()
                .unwrap_or_else(|| core.game_dir.join("save").join("yskernel_qsave.json"));
            self.restore_from(&path);
            return;
        }
        if core.key_pulse {
            core.key_pulse_frames += 1;
            if core.key_pulse_frames > 120 {
                let _ = core.vm.clear_input_key();
                core.key_pulse = false;
                core.key_pulse_frames = 0;
            }
        }
        // 脚本对输入变量的写入回灌状态机（钳制已在 VM 侧按物理态应用）。
        core.end_input_frame();
        core.frame_clicked = false;
        let t5 = if prof { Some(Instant::now()) } else { None };
        // **tick 分段计时**(成果 184):`--bench` 的 VM 段是**整帧** tick,
        // 若「链」只占其中一部分,剩下一律落在尾部(按钮三态/输入回灌等)。
        // 必须逐段打出来才能判断优化该落在哪。
        if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
            (t0, t1, t2, t3, t4, t5)
        {
            static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // 采样点每 30 帧一次;**另外凡是整 tick 超过 16.67ms 也必打**
            // —— 慢帧不在采样点上时会完全看不到(成果 184 踩过)。
            let total = f - a;
            if n % 30 == 0 || total > std::time::Duration::from_nanos(16_667_000) {
                yuris_log::trace_forced!(
                    "[tickprof] f{n} total={total:?} vm={:?} events={:?} scen={:?} btn={:?} tail={:?}",
                    b - a,
                    c - b,
                    d - c,
                    e - d,
                    f - e
                );
            }
        }
    }

    /// 平台壳推入一条原始输入事件。
    pub fn push_input(&mut self, ev: yuris_input::PlatformEvent) {
        self.core.input.push_event(ev);
    }

    /// 推入触摸事件（自动合成鼠标语义）。
    pub fn push_touch(
        &mut self,
        id: u64,
        phase: yuris_input::TouchPhase,
        x: f64,
        y: f64,
    ) {
        let events = self.core.input_touch.translate(id, phase, x, y);
        for ev in events {
            self.core.input.push_event(ev);
        }
    }

    /// 窗口尺寸变化（输入反算用）。
    pub fn resize_input(&mut self, w: u32, h: u32) {
        self.core.input.resize(w, h);
    }

    /// 逻辑画布尺寸变化（`yscfg.ybn` 分辨率）。
    pub fn set_input_logical_size(&mut self, w: f32, h: f32) {
        self.core.input.set_logical_size(w, h);
    }

    /// 输出 VM 的 Unsupported 命令汇总(用户要求:跳过必须可查)。
    pub fn log_vm_unsupported(&self, tag: &str) {
        self.core.vm.log_unsupported_summary(tag);
    }

    /// **把鼠标点击翻译成游戏自己的按钮事件**(2026-10-17,成果 177)。
    ///
    /// 背景:`es.BT` 是脚本宏族(`BT.NAME.SET`/`BT.QUE.*`/`BT.ACTIVEGROUP`),
    /// 入队需要**外部命中信号** —— 旧实现完全缺失:
    /// - 标题按钮点了没反应(`@1035` 恒 -1,`s58 g31` 永不成立);
    /// - 进游戏后系统栏/MESW 永不点亮(`s71` 只跑 173/1882 组)。
    ///
    /// 命中语义:在 Stage 实体表里找**可见**的 `BT.*.BTN.*` 层,按
    /// (z, 创建序)取最上层;命中后把按钮序号写进 VM 的标题选择槽。
    pub fn dispatch_ui_click(&mut self, x: i64, y: i64) -> bool {
        let (fx, fy) = (x as f32, y as f32);
        for prefix in ["BT.OFFES.TITLE.BTN.", "BT.OFFES.GAMEMAIN.BTN."] {
            if let Some((name, _z)) = self.core.stage.hit_test(fx, fy, prefix, (317.0, 76.0)) {
                let nm = String::from_utf8_lossy(&name).into_owned();
                yuris_log::info!("[ui] 命中按钮层 {nm}");
                if let Some(slot) = title_button_slot(&nm) {
                    self.core.vm.set_title_choice(slot);
                    return true;
                }
            }
        }
        false
    }

    /// **注入一次逻辑坐标点击**(无头复现用;`--click x,y --click-frame n`)。
    ///
    /// 走与真实鼠标同一条输入路径:置光标 → 标 `frame_clicked` →
    /// 下一 tick 由 `poll_title_menu`/场景层消费。
    pub fn inject_click(&mut self, x: i64, y: i64) {
        self.core.cursor_logical = (x, y);
        if self.dispatch_ui_click(x, y) {
            self.core.frame_clicked = true;
            self.core.key_pulse = true;
            self.core.clicked = true;
            yuris_log::info!("[player] 注入点击 ({x},{y}) → UI 按钮");
            return;
        }
        self.core.clicked = true;
        self.core.frame_clicked = true;
        self.core.key_pulse = true;
        yuris_log::info!("[player] 注入点击 ({x},{y})");
    }

    /// 注入一次按键(无头复现/对拍用),并推进 VM 按键态。
    pub fn inject_key(&mut self, key: &str) {
        match self.core.vm.inject_input_key(key.as_bytes()) {
            Ok(()) => {
                self.core.key_pulse = true;
                self.core.key_pulse_frames = 0;
                yuris_log::trace!("[player] 注入按键 {key}");
            }
            Err(e) => yuris_log::error!("[player] 注入失败: {e}"),
        }
    }

    /// F5 快存:写 `save/yskernel_qsave.json`;无可存锚点时升为致命错误窗。
    pub fn quick_save(&mut self) {
        let Some((file, label)) = self.scenario.save_point().ok() else {
            // **无可存锚点**:存档请求落空。以前一行 warn;现在按用户要求
            // 升为致命错误窗(存档是显式用户动作,静默失败不可接受)。
            yuris_log::report_fatal(
                module_path!(),
                "存档失败 / SAVE FAILED",
                "当前无可存锚点(尚无 \\LT/标签)。快存未写入,进度不会保存。",
            );
            return;
        };
        let mut globals = Vec::new();
        // 全局槽 = 仿真内部存储(成果 78 勘误:原 @50 映射被证伪);
        // JSON 形状不变(globals[0..64]),槽 i → 下标 i。
        for i in 0..64 {
            globals.push(self.core.globals.get(&i).copied().unwrap_or(0));
        }
        let doc = serde_json::json!({
            "file": file, "label": label, "globals": globals,
        });
        let path = self.game_dir.join("save").join("yskernel_qsave.json");
        // save/ 目录可能不存在(原生引擎首次存档时自建;本目录为干净安装)
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                yuris_log::report_fatal_error(module_path!(), "存档目录创建失败", e);
                return;
            }
        }
        let bytes = match serde_json::to_vec_pretty(&doc) {
            Ok(b) => b,
            Err(e) => {
                yuris_log::report_fatal_error(module_path!(), "存档序列化失败", e);
                return;
            }
        };
        match std::fs::write(&path, bytes) {
            Ok(()) => {
                yuris_log::info!("[save] 快存 {file}/{label} → {}", path.display());
            }
            Err(e) => {
                yuris_log::report_fatal_error(module_path!(), "存档写入失败", e);
            }
        }
    }

    /// F9 快读:从 `save/yskernel_qsave.json` 恢复。
    pub fn quick_load(&mut self) {
        self.restore_from(&self.game_dir.join("save").join("yskernel_qsave.json"));
    }

    /// 从指定存档 JSON 恢复(F9 快读与 LOAD 画面槽位共用;成果 81)。
    pub fn restore_from(&mut self, path: &std::path::Path) {
        let Ok(bytes) = std::fs::read(path) else {
            // 无快存 = 正常的「还没存过」,不是崩坏级。
            yuris_log::warn!("[load] 无快存: {}", path.display());
            return;
        };
        // **损坏的存档以前直接 panic**(`expect("快存解析")`)—— 用户看到的
        // 是进程消失、没有任何现场。现在转成致命错误窗。
        let doc: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                yuris_log::report_fatal_error(
                    module_path!(),
                    &format!("存档损坏,无法解析: {}", path.display()),
                    e,
                );
                return;
            }
        };
        let (Some(file), Some(label)) = (
            doc["file"].as_str().map(|x| x.to_string()),
            doc["label"].as_str().map(|x| x.to_string()),
        ) else {
            yuris_log::report_fatal(
                module_path!(),
                "存档损坏 / LOAD FAILED",
                format!(
                    "存档缺少 `file`/`label` 字段: {}\n\
                     快存未恢复,当前进度保持不变。",
                    path.display()
                ),
            );
            return;
        };
        // 全局槽恢复 = 仿真内部存储(成果 78 勘误;JSON 形状不变)。
        if let Some(arr) = doc["globals"].as_array() {
            for (i, v) in arr.iter().enumerate() {
                if let Some(n) = v.as_i64() {
                    self.core.globals.insert(i, n);
                }
            }
        }
        self.core.close_subui(); // 从 LOAD 画面读档 → 关闭子画面(成果 81)
        let scene = self.core.bridge.scene_mut();
        for id in self.core.sprites.drain(..) {
            scene.hide_layer(id);
        }
        self.core.sprite_fades.clear(); // 挂起淡入会把已隐藏层重新点亮(残留修复)
        self.core.sprite_moves.clear(); // 位移动画不跨读档
        scene.hide_layer(SC_TEXT);
        scene.hide_layer(SC_WIN);
        self.core.clear_choices();
        self.core.fade = None;
        let _ = self.scenario.start(&file, &label);
        yuris_log::info!("[load] 恢复 {file}/{label}");
    }
}

/// 错误窗图层（纯函数，便于单测）。
///
/// `z = i64::MAX`：引擎的 Z 可到 i64 近顶（成果 132），只有 i64::MAX 能
/// 保证压过任何脚本层。
///
/// 位图按**物理像素**排版（见 `overlay` 模块头），因此 `scale = 1/output_scale`
/// 把它映射回逻辑画布 —— 屏幕上 1 纹理像素 = 1 物理像素，文字不再被
/// letterbox 放大成糊的。
fn fatal_overlay_layer(rid: ResourceId, output_scale: f32) -> Layer {
    let inv = 1.0 / output_scale.clamp(1.0, 4.0);
    Layer {
        id: SC_FATAL,
        z: i64::MAX,
        visible: true,
        x: 0.0,
        y: 0.0,
        scale_x: inv,
        scale_y: inv,
        alpha: 1.0,
        rotation: 0.0,
        resource: Some(rid),
        ..Default::default()
    }
}

impl PlayerCore {
    /// **物理像素 / 逻辑像素**的缩放比（错误窗排版用）。
    ///
    /// `None` = 后端未注入或表面尺寸为 0（此时按 1.0 排版，稍后可重排）。
    fn fatal_output_scale(&self) -> f32 {
        let (sw, sh) = self.backend.as_ref().map(|b| b.surface_size()).unwrap_or((0, 0));
        if sw == 0 || sh == 0 {
            return 1.0;
        }
        let lw = logical_w().max(1.0);
        let lh = logical_h().max(1.0);
        // 取宽高比的较小者:letterbox 时窗口比例 ≠ 逻辑比例,取小值才能
        // 保证「逻辑 1px ≥ 物理 1px」,不会把面板顶出屏幕。
        (sw as f32 / lw).min(sh as f32 / lh).clamp(1.0, 4.0)
    }

    /// 错误窗滚动（物理像素增量；返回是否消费了本次输入）。
    ///
    /// 调用方：滚轮 / 触摸拖动。**不**触发重排 —— 只改数字，
    /// [`Self::update_fatal_overlay`] 在帧内统一应用（避免拖动时每像素
    /// 重排一次巨大 RGBA）。
    pub fn scroll_fatal_overlay(&mut self, delta_px: i32) -> bool {
        if self.fatal_overlay.is_none() {
            return false;
        }
        let next = self
            .fatal_scroll
            .saturating_add(delta_px)
            .clamp(0, self.fatal_max_scroll.max(0));
        if next == self.fatal_scroll {
            return false;
        }
        self.fatal_scroll = next;
        true
    }

    /// 当前错误窗是否还能向某方向滚动（供输入层决定是否吞掉滚轮事件）。
    pub fn fatal_overlay_can_scroll(&self, delta_px: i32) -> bool {
        if self.fatal_overlay.is_none() {
            return false;
        }
        if delta_px > 0 {
            self.fatal_scroll < self.fatal_max_scroll
        } else if delta_px < 0 {
            self.fatal_scroll > 0
        } else {
            false
        }
    }

    /// 检测并维护**崩坏级错误窗**（画布中央，可滚动）。
    ///
    /// 数据源 = [`yuris_log`]：任意 crate 调 `fatal!` / `report_fatal_error`
    /// 都会在这里被消费。
    ///
    /// 只在**报告序号 / 输出缩放 / 滚动量**变化时重排（滚动是输入驱动的，
    /// 必须收敛到这里才不重复上传几 MB 纹理）。
    ///
    /// 返回 `true` = 当前有致命错误（调用方应停止推进游戏逻辑）。
    pub fn update_fatal_overlay(&mut self) -> bool {
        let Some(report) = yuris_log::fatal_report() else {
            self.hide_fatal_overlay();
            return false;
        };
        let scale = self.fatal_output_scale();
        if self
            .fatal_overlay
            .as_ref()
            .is_some_and(|st| st.seq == report.seq && st.scroll == self.fatal_scroll && st.scale == scale)
        {
            return true;
        }
        let (cw, ch) = (logical_w().max(1.0), logical_h().max(1.0));
        let out_w = (cw * scale).round().max(64.0) as u32;
        let out_h = (ch * scale).round().max(64.0) as u32;
        // 先按当前滚动量排版(它会给出夹紧后的实际滚动量与上限),
        // 再用返回的真值回写 —— 报告变短后旧滚动量不会留成死值。
        let panel = overlay::build_fatal_panel(
            &self.font,
            &report,
            out_w,
            out_h,
            scale,
            self.fatal_scroll,
        );
        self.fatal_scroll = panel.scroll;
        self.fatal_max_scroll = panel.max_scroll;
        let Some(backend) = self.backend.as_mut() else {
            // 图形后端尚未注入：保留待重试（窗口创建后下一帧再画）。
            return true;
        };
        let rid = ResourceId(SC_FATAL_TEX);
        if backend
            .load_image_rgba(rid, &panel.rgba, panel.width, panel.height)
            .is_err()
        {
            return true;
        }
        let layer = fatal_overlay_layer(rid, scale);
        self.bridge.scene_mut().upsert_layer(layer);
        let first = self
            .fatal_overlay
            .as_ref()
            .map(|st| st.seq != report.seq)
            .unwrap_or(true);
        self.fatal_overlay = Some(FatalOverlayState {
            seq: report.seq,
            rid,
            scroll: panel.scroll,
            scale,
        });
        if first {
            yuris_log::error!(
                "致命错误已上屏：{}（报告 #{}，{}×{} @{:.2}x）",
                report.message.lines().next().unwrap_or(""),
                report.seq,
                panel.width,
                panel.height,
                scale
            );
        }
        true
    }

    /// 撤下错误窗（致命状态被清除后）。
    pub fn hide_fatal_overlay(&mut self) {
        if self.fatal_overlay.take().is_some() {
            self.bridge.scene_mut().hide_layer(SC_FATAL);
        }
        self.fatal_scroll = 0;
        self.fatal_max_scroll = 0;
    }

    /// **关闭错误窗并退出**（Esc / 点按 / 触摸点按共用的语义）。
    ///
    /// 只做状态清理；真正的进程退出仍由平台壳决定（`request_quit`
    /// 不在 VM 层，壳在 `Player::tick` 之外读 `yuris_log::has_fatal`）。
    pub fn dismiss_fatal_overlay(&mut self) {
        yuris_log::clear_fatal();
        self.hide_fatal_overlay();
    }

    /// 错误窗是否在场（输入层据此决定「吞掉全部游戏输入」）。
    pub fn fatal_overlay_active(&self) -> bool {
        self.fatal_overlay.is_some()
    }

    /// 平台壳用：当前错误窗排版缩放比（触摸位移要换算到物理像素）。
    pub fn fatal_output_scale_pub(&self) -> f32 {
        self.fatal_output_scale()
    }

    /// 标题层清理(reset 与进入对话时)。
    fn reset_title_layers(&mut self) {
        self.close_subui(); // 子画面(LOAD/EXTRA/确认框)一并复位(成果 81)
        let scene = self.bridge.scene_mut();
        for i in 0..12u64 {
            scene.hide_layer(0x5C_7000_0000 + i);
        }
        self.title_buttons.clear(); // 命中区同步失效(成果 78)
                                    // scenario 精灵层同步隐藏(残留修复,成果 81 勘误 3):快速点击压缩
                                    // 时序时,\\S.D 的淡出动画可能未完成即遇 reset —— 只清 sprite_fades
                                    // 不隐藏层,LOGO/注意事项等会以当时 alpha 永久残留(标题与正篇)。
        for id in self.sprites.drain(..) {
            scene.hide_layer(id);
        }
        self.sprite_fades.clear();
        self.sprite_moves.clear();
        self.tachie_float.clear();
        scene.hide_layer(SC_TEXT);
        scene.hide_layer(SC_WIN);
        scene.hide_layer(SC_FADE);
        for i in 0..3u64 {
            scene.hide_layer(0x5C_6000_0000 + i);
        }
        self.clear_choices();
        self.fade = None;
    }
}

#[cfg(test)]
mod text_metrics_tests {
    //! 临时探针:实测字体 metrics(勿久留,验证后可删)。
    use super::*;

    #[test]
    fn probe_stheiti_metrics() {
        let bytes = load_cjk_font().expect("no cjk font");
        let font = fontdue::Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                collection_index: 0,
                scale: 40.0,
                load_substitutions: true,
            },
        )
        .unwrap();
        let hm = font.horizontal_line_metrics(40.0).expect("hm");
        println!(
            "HM ascent={} descent={} line_gap={}",
            hm.ascent, hm.descent, hm.line_gap
        );
        for ch in ['猫', '渲', '。', '「', 'A'] {
            let (m, _) = font.rasterize(ch, 40.0);
            println!(
                "'{ch}': width={} height={} xmin={} ymin={} adv={}",
                m.width, m.height, m.xmin, m.ymin, m.advance_width
            );
        }
    }
}

#[cfg(test)]
mod title_se_tests {
    //! P2-2 回归(成果 80):标题按钮悬停进入/点击 → sysse SE 决策。
    //! 空音频包下 resolve 未命中,决策经 `Audio::se_log` 断言(不出声)。

    use super::*;
    use yuris_format::ystb::YstbFile;

    /// 最小合成 YSTB(1 组 0 窗;GroupVm 唯一构造路径 `load`)。
    fn minimal_vm() -> GroupVm {
        fn xor(r: &[u8], key: [u8; 4]) -> Vec<u8> {
            r.iter().enumerate().map(|(i, b)| b ^ key[i % 4]).collect()
        }
        let key = [0x2b, 0x90, 0x4f, 0x93];
        let part1 = xor(&[0u8, 0, 0, 0], key); // 1 组 0 窗(Σcount*12==0)
        let p4 = xor(&[0u8; 4], key);
        let mut out = Vec::new();
        out.extend_from_slice(b"YSTB");
        out.extend_from_slice(&555u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes()); // unknown1 = 组数
        out.extend_from_slice(&(part1.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // command_len
        out.extend_from_slice(&0u32.to_le_bytes()); // content_len
        out.extend_from_slice(&(p4.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&part1);
        out.extend_from_slice(&p4);
        GroupVm::load(YstbFile::from_bytes(&out, key).expect("ystb")).expect("vm")
    }

    pub(super) fn make_core() -> PlayerCore {
        let bytes = load_cjk_font().expect("no cjk font");
        let font = fontdue::Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                collection_index: 0,
                scale: 40.0,
                load_substitutions: true,
            },
        )
        .unwrap();
        PlayerCore {
            debug_frames: 0,
            stage: yuris_stage::Stage::new(),
            snap_seq: HashMap::new(),
        layer_z_memo: HashMap::new(),
            index: Arc::new(PacFileIndex::default()),
            vm: minimal_vm(),
            bridge: SceneBridge::new(),
            loaded: HashSet::new(),
            backend: None,
            events_cursor: 0,
            input: yuris_input::InputState::new(),
            input_touch: yuris_input::TouchAdapter::new(),
            cursor_logical: (0, 0),
            key_pulse: false,
            key_pulse_frames: 0,
            font,
            fade: None,
            clicked: false,
            sprites: Vec::new(),
            sprite_fades: Vec::new(),
            text_slots: (0..TEXT_SLOT_MAX).map(|_| None).collect(),
            text_slot_seq: 0,
            sprite_moves: Vec::new(),
            tachie_float: std::collections::HashMap::new(),
            float_clock: Instant::now(),
            anim_epoch: Instant::now(),
            anim_frames: 0,
            anim_debt_ms: 0.0,
            audio_packs: Arc::new(PacFileIndex::default()),
            audio: crate::audio::Audio::headless(),
            choices: None,
            last_cursor_px: None,
            frame_clicked: false,
            title_buttons: Vec::new(),
            request_title_load: false,
            request_quit: false,
            globals: HashMap::new(),
            subui: SubUi::None,
            ui_buttons: Vec::new(),
            save_entries: Vec::new(),
            request_load_path: None,
            unsup_note_seen: std::collections::HashSet::new(),
            ev_list: Vec::new(),
            cg_page: 0,
            cg_view: None,
            bgm_tracks: Vec::new(),
            bgm_page: 0,
            title_seen: false,
            bg_cam: (0, 0, 0),
            adv_buttons: Vec::new(),
            window_mode: 0,
            vm_ui_allowed_prev: false,
            frame_budget: frame_budget_from_env(),
            fatal_overlay: None,
            fatal_scroll: 0,
            fatal_max_scroll: 0,
            game_dir: std::path::PathBuf::new(),
        }
    }

    fn btn(id: u64, active: bool) -> TitleButton {
        TitleButton {
            rect: [100.0, 100.0, 50.0, 50.0],
            id,
            rids: [None, None, None],
            shown: TITLE_BTN_OFF,
            hovered: false,
            active,
        }
    }

    #[test]
    fn title_hover_se_fires_on_enter_edge_only() {
        let mut core = make_core();
        core.title_buttons.push(btn(0x5C_7000_0005, true));
        core.cursor_logical = (110, 110);
        core.update_title_buttons();
        assert_eq!(core.audio.se_log, vec!["sse02"]);
        core.update_title_buttons(); // 悬停保持:不重复
        assert_eq!(core.audio.se_log.len(), 1);
        core.cursor_logical = (10, 10);
        core.update_title_buttons(); // 离开
        assert_eq!(core.audio.se_log.len(), 1);
        core.cursor_logical = (120, 120);
        core.update_title_buttons(); // 再进入:再响一次
        assert_eq!(core.audio.se_log, vec!["sse02", "sse02"]);
    }

    #[test]
    fn title_click_plays_decide_se() {
        let mut core = make_core();
        core.title_buttons.push(btn(0x5C_7000_0005, true));
        core.cursor_logical = (110, 110);
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), Some(TitleMenuAction::Start));
        assert_eq!(core.audio.se_log, vec!["sse03"]);
    }

    #[test]
    fn title_na_button_is_silent() {
        let mut core = make_core();
        core.title_buttons.push(btn(0x5C_7000_0007, false));
        core.cursor_logical = (110, 110);
        core.update_title_buttons(); // `_na`:不参与三态/不响
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None); // 点击无效
        assert!(core.audio.se_log.is_empty());
    }
}

#[cfg(test)]
mod subui_tests {
    //! P3 回归(成果 81):END 确认流 / LOAD 槽位选择 / EXTRA 导航。
    //! 空索引 + 无 backend:层不落,状态机与命中区断言照常成立。

    use super::*;

    pub(super) fn make_core() -> PlayerCore {
        let bytes = load_cjk_font().expect("no cjk font");
        let font = fontdue::Font::from_bytes(
            bytes,
            fontdue::FontSettings {
                collection_index: 0,
                scale: 40.0,
                load_substitutions: true,
            },
        )
        .unwrap();
        PlayerCore {
            debug_frames: 0,
            stage: yuris_stage::Stage::new(),
            snap_seq: HashMap::new(),
        layer_z_memo: HashMap::new(),
            index: Arc::new(PacFileIndex::default()),
            vm: minimal_vm(),
            bridge: SceneBridge::new(),
            loaded: HashSet::new(),
            backend: None,
            events_cursor: 0,
            input: yuris_input::InputState::new(),
            input_touch: yuris_input::TouchAdapter::new(),
            cursor_logical: (0, 0),
            key_pulse: false,
            key_pulse_frames: 0,
            font,
            fade: None,
            clicked: false,
            sprites: Vec::new(),
            sprite_fades: Vec::new(),
            text_slots: (0..TEXT_SLOT_MAX).map(|_| None).collect(),
            text_slot_seq: 0,
            sprite_moves: Vec::new(),
            tachie_float: std::collections::HashMap::new(),
            float_clock: Instant::now(),
            anim_epoch: Instant::now(),
            anim_frames: 0,
            anim_debt_ms: 0.0,
            audio_packs: Arc::new(PacFileIndex::default()),
            audio: crate::audio::Audio::headless(),
            choices: None,
            last_cursor_px: None,
            frame_clicked: false,
            title_buttons: Vec::new(),
            request_title_load: false,
            request_quit: false,
            globals: HashMap::new(),
            subui: SubUi::None,
            ui_buttons: Vec::new(),
            save_entries: Vec::new(),
            request_load_path: None,
            unsup_note_seen: std::collections::HashSet::new(),
            ev_list: Vec::new(),
            cg_page: 0,
            cg_view: None,
            bgm_tracks: Vec::new(),
            bgm_page: 0,
            title_seen: false,
            bg_cam: (0, 0, 0),
            adv_buttons: Vec::new(),
            window_mode: 0,
            vm_ui_allowed_prev: false,
            frame_budget: frame_budget_from_env(),
            fatal_overlay: None,
            fatal_scroll: 0,
            fatal_max_scroll: 0,
            game_dir: std::path::PathBuf::new(),
        }
    }

    /// 复用 title_se_tests 的最小 VM 构造(同名逻辑独立成此处夹具)。
    fn minimal_vm() -> GroupVm {
        fn xor(r: &[u8], key: [u8; 4]) -> Vec<u8> {
            r.iter().enumerate().map(|(i, b)| b ^ key[i % 4]).collect()
        }
        let key = [0x2b, 0x90, 0x4f, 0x93];
        let part1 = xor(&[0u8, 0, 0, 0], key);
        let p4 = xor(&[0u8; 4], key);
        let mut out = Vec::new();
        out.extend_from_slice(b"YSTB");
        out.extend_from_slice(&555u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(part1.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(p4.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&part1);
        out.extend_from_slice(&p4);
        GroupVm::load(yuris_format::ystb::YstbFile::from_bytes(&out, key).expect("ystb"))
            .expect("vm")
    }

    fn temp_game_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("yuris_p3_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("save")).unwrap();
        dir
    }

    #[test]
    fn end_confirm_yes_quits_no_stays() {
        let mut core = make_core();
        core.title_buttons.push(TitleButton {
            rect: [100.0, 100.0, 50.0, 50.0],
            id: 0x5C_7000_0009, // END
            rids: [None, None, None],
            shown: TITLE_BTN_OFF,
            hovered: false,
            active: true,
        });
        core.cursor_logical = (110, 110);
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), Some(TitleMenuAction::End));
        core.request_quit(); // scenario 路由:END → 确认对话框
        assert_eq!(core.subui, SubUi::ConfirmEnd);
        assert!(!core.request_quit, "确认前不得置退出请求");
        // いいえ → 留在标题
        core.cursor_logical = (1100, 490); // btn_no(1040,470,146,45)
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert_eq!(core.subui, SubUi::None);
        assert!(!core.request_quit);
        assert_eq!(core.audio.se_log.last().map(String::as_str), Some("sse06"));
        // 再走一遍 → はい → 退出请求
        core.request_quit();
        core.cursor_logical = (810, 490); // btn_yes(740,470,146,45)
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert!(core.request_quit);
    }

    #[test]
    fn load_screen_slot_click_requests_restore() {
        let dir = temp_game_dir("load");
        std::fs::write(
            dir.join("save").join("yskernel_qsave.json"),
            r#"{"file":"maho2_01.txt","label":"m1","globals":[0]}"#,
        )
        .unwrap();
        let mut core = make_core();
        core.game_dir = dir.clone();
        core.open_load();
        assert_eq!(core.subui, SubUi::Load);
        assert_eq!(core.save_entries.len(), 1);
        // 戻る → 回标题
        core.cursor_logical = (1700, 960); // btn_back(1660,940,186,87)
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert_eq!(core.subui, SubUi::None);
        // 重开 → 点槽位 0 → 读档请求
        core.open_load();
        core.cursor_logical = (400, 240); // 槽 0(320,200,1280,80)
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert!(core.request_title_load);
        assert_eq!(
            core.request_load_path.as_deref(),
            Some(dir.join("save").join("yskernel_qsave.json").as_path())
        );
        assert_eq!(core.audio.se_log.last().map(String::as_str), Some("sse03"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vm_ui_filter_rules() {
        // P8.2b(成果 82/勘误 7):debug 覆盖层/标题期/title-extra 通道不建层;
        // main/saveload/config 等全 cgsys 系统 UI 放行
        let main = "cgsys/main/button/type1/tip_meswindow";
        let main_bs = r"cgsys\main\button\type1\tip_meswindow";
        let debug = r"cgsys\debug\btn_back";
        let title = "cgsys/title/btn_start_off";
        let extra = r"cgsys\extra\btn_tab_cgmode_bt3n";
        let saveload = r"cgsys\saveload\btn_back_off";
        assert!(vm_ui_layer_allowed(main, true, true));
        assert!(vm_ui_layer_allowed(main_bs, true, true));
        assert!(vm_ui_layer_allowed(saveload, true, true));
        assert!(!vm_ui_layer_allowed(main, false, true)); // 标题等待期
        assert!(!vm_ui_layer_allowed(debug, true, true)); // debug 覆盖层
        assert!(!vm_ui_layer_allowed(title, true, true)); // 内置标题接管
        assert!(!vm_ui_layer_allowed(extra, true, true)); // EXTRA 子画面接管
    }

    #[test]
    fn vm_ui_base_prio_is_identity() {
        // 通用化(成果 132):层基名 = CG 名(恒等),不再按 v555 的
        // `.BT.OFF/OVER/ONOV/NA` 命名做「同钮各态收敛」—— 那是 E 槽
        // 未被消费时期的补丁;现在每个 CG ID 是独立持久层,显隐由 E 决定
        // (REA docs/engine/render/02-layer-model.md §1)。
        let off = b"ES.GAMEMAIN.BTN.VOICEM.BT.OFF=0=1";
        let over = b"ES.GAMEMAIN.BTN.VOICEM.BT.OVER=0=1";
        let plain = b"some-cg-name";
        assert_eq!(vm_ui_base_prio(off).0, off.to_vec());
        assert_eq!(vm_ui_base_prio(over).0, over.to_vec());
        assert_ne!(vm_ui_base_prio(off).0, vm_ui_base_prio(over).0);
        assert_eq!(vm_ui_base_prio(plain).0, plain.to_vec());
        // 旧折叠规则保留(若将来按版本 Profile 接回):同钮各态归一个基名。
        let legacy_off = vm_ui_base_prio_legacy(off);
        let legacy_over = vm_ui_base_prio_legacy(over);
        assert_eq!(legacy_off.0, legacy_over.0);
        assert!(legacy_off.1 < legacy_over.1);
    }

    #[test]
    fn reset_hides_pending_sprites() {
        // 残留修复(成果 81 勘误 3):快速点击时序压缩 → reset 时淡出未完成
        // → 精灵层必须同步隐藏(此前只清 sprite_fades,层以当时 alpha 残留)。
        let mut core = make_core();
        let id = 0x1234;
        core.sprites.push(id);
        core.sprite_fades.push((id, 0.0, 800, true));
        core.bridge.scene_mut().upsert_layer(Layer {
            id,
            z: 10,
            visible: true,
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            alpha: 0.4,
            rotation: 0.0,
            resource: None,
            ..Default::default()
        });
        core.reset_title_layers();
        let l = core
            .bridge
            .scene()
            .layers
            .iter()
            .find(|l| l.id == id)
            .expect("层应在场景中(隐藏态)");
        assert!(!l.visible);
        assert!(core.sprite_fades.is_empty());
        assert!(core.sprites.is_empty());
    }

    #[test]
    fn extra_screens_nav_and_back() {
        let mut core = make_core();
        // EXTRA → 直达 CG 鉴赏(原生 yst00257 流程,无落地菜单)
        core.title_extra();
        assert_eq!(core.subui, SubUi::ExtraCg);
        // BGM 标签(1272,10,237,53;4 钮标签行第 4 位)
        core.cursor_logical = (1350, 30);
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert_eq!(core.subui, SubUi::ExtraBgm);
        // CG 标签(531,10,237,53)
        core.cursor_logical = (600, 30);
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert_eq!(core.subui, SubUi::ExtraCg);
        // 戻る(1620,930,237,80)→ 标题
        core.cursor_logical = (1700, 960);
        core.frame_clicked = true;
        assert_eq!(core.poll_title_menu(), None);
        assert_eq!(core.subui, SubUi::None);
    }
}

#[cfg(test)]
mod fatal_overlay_scroll_tests {
    //! 错误窗滚动/关闭的纯状态机回归（成果 187）。
    //!
    //! 排版本身（像素）在 `overlay` 模块测；这里只锁「输入 → 状态」这一层：
    //! 夹紧、方向、是否吞掉输入，以及关闭时清干净。

    use super::*;

    fn core_with_panel(max_scroll: i32) -> PlayerCore {
        let mut core = subui_tests::make_core();
        core.fatal_overlay = Some(FatalOverlayState {
            seq: 1,
            rid: ResourceId(SC_FATAL_TEX),
            scroll: 0,
            scale: 1.0,
        });
        core.fatal_max_scroll = max_scroll;
        core
    }

    /// 向下滚 = scroll 增大；夹紧到 max_scroll，不会滚过头。
    #[test]
    fn scroll_clamps_to_content_extent() {
        let mut core = core_with_panel(300);
        assert!(core.fatal_overlay_active());
        assert!(core.scroll_fatal_overlay(120));
        assert_eq!(core.fatal_scroll, 120);
        assert!(core.scroll_fatal_overlay(500));
        assert_eq!(core.fatal_scroll, 300, "不得越过内容底部");
        // 到头后继续滚 = 未消费（平台壳据此把事件让回去）。
        assert!(!core.scroll_fatal_overlay(120));
        assert_eq!(core.fatal_scroll, 300);
    }

    /// 向上滚到底同样夹紧，且负值不会把 scroll 变成负数。
    #[test]
    fn scroll_never_goes_negative() {
        let mut core = core_with_panel(300);
        assert!(core.scroll_fatal_overlay(100));
        assert!(core.scroll_fatal_overlay(-1000));
        assert_eq!(core.fatal_scroll, 0);
        assert!(!core.scroll_fatal_overlay(-10));
    }

    /// 一屏放得下（max_scroll = 0）时滚轮不消费 —— 面板不「假滚动」。
    #[test]
    fn no_scroll_when_content_fits() {
        let mut core = core_with_panel(0);
        assert!(!core.scroll_fatal_overlay(120));
        assert_eq!(core.fatal_scroll, 0);
        assert!(!core.fatal_overlay_can_scroll(120));
    }

    /// 无错误窗时滚动/查询都不生效（输入必须落回游戏）。
    #[test]
    fn no_panel_means_no_consumption() {
        let mut core = subui_tests::make_core();
        assert!(!core.fatal_overlay_active());
        assert!(!core.scroll_fatal_overlay(120));
        assert!(!core.fatal_overlay_can_scroll(-120));
    }

    /// 关闭：撤层 + 清滚动量 + 清全局致命状态。
    #[test]
    fn dismiss_clears_layer_and_scroll() {
        let mut core = core_with_panel(300);
        core.scroll_fatal_overlay(60);
        assert!(core.fatal_overlay_active(), "构造后错误窗应在场");
        assert_eq!(core.fatal_scroll, 60);
        core.dismiss_fatal_overlay();
        assert!(core.fatal_overlay.is_none());
        assert_eq!(core.fatal_scroll, 0);
        assert_eq!(core.fatal_max_scroll, 0);
        core.scroll_fatal_overlay(60);
        assert_eq!(core.fatal_scroll, 0);
        assert!(!yuris_log::has_fatal());
    }
}

#[cfg(test)]
mod layout_tests {
    //! 布局修复回归(layout-fix-plan P2-5):状态件可见性规则 + 主行定义。

    use super::*;

    #[test]
    fn vm_ui_filter_hides_state_families() {
        // 状态件家族:仅悬停/激活时显示 → 默认隐藏(P2-1)
        assert!(!vm_ui_layer_allowed(
            "cgsys/main/pop/tip_config",
            true,
            true
        ));
        assert!(!vm_ui_layer_allowed(
            "cgsys/main/autoskipicon/icon_01",
            true,
            true
        ));
        assert!(!vm_ui_layer_allowed(
            "cgsys/main/skipicon/icon_01",
            true,
            true
        ));
        assert!(!vm_ui_layer_allowed("cgsys/main/count/icon_02", true, true));
        // 常显件与存档目录不受影响
        assert!(vm_ui_layer_allowed(
            "cgsys/main/button/type1/btn_auto_bt4",
            true,
            true
        ));
        assert!(vm_ui_layer_allowed("cgsys/saveload/back_load", true, true));
        // 标题等待期整体关闭
        assert!(!vm_ui_layer_allowed(
            "cgsys/main/button/type1/btn_auto_bt4",
            false,
            true
        ));
    }

    #[test]
    fn bg_engine_model_matches_on_machine_measure() {
        // 引擎实测(2026-09-12 原版截图,corr 0.908):S=1.25,
        // 可见区 1536×864,中心 + cam×0.5
        let (x, y, s) = bg_cover_transform(2400, 1200, (0, 0, 0));
        assert!((s - 1.25).abs() < 1e-6);
        assert_eq!((x, y), (-540.0, -210.0)); // x0=432, y0=168
                                              // maho2_01 办公室:cam(507,296) → x0=685.5, y0=316(实测 694/336 ±1%)
        let (x, y, _) = bg_cover_transform(2400, 1200, (507, 296, 0));
        assert!((x + 856.875).abs() < 0.01, "x={x}");
        assert!((y + 395.0).abs() < 0.01, "y={y}");
        // 相机越界 → 钳制在图内(不露黑边)
        let (x, y, _) = bg_cover_transform(2400, 1200, (100000, 100000, 0));
        assert_eq!((x, y), (-1080.0, -420.0));
        // 小于视口的图 → 保底 cover 放大填满
        let (x, y, s) = bg_cover_transform(1280, 720, (0, 0, 0));
        assert!((s - 1.5).abs() < 1e-6);
        assert_eq!((x, y), (0.0, 0.0));
    }

    #[test]
    fn vm_ui_filter_normalizes_alias_root() {
        // cgsys_c 运行期别名根(known-issues 2.4)必须与 cgsys 同判
        assert!(!vm_ui_layer_allowed(
            "cgsys_c/main/pop/tip_config",
            true,
            true
        ));
        assert!(!vm_ui_layer_allowed(
            "cgsys_c/main/autoicon/icon_01",
            true,
            true
        ));
        assert!(!vm_ui_layer_allowed(
            "cgsys/main/autoicon/icon_01",
            true,
            true
        ));
        assert!(vm_ui_layer_allowed(
            "cgsys_c/main/button/type1/btn_auto_bt4",
            true,
            true
        ));
        // 画面归属(黑屏勘误):游戏自己接管时(builtin_title=false),
        // 标题/EXTRA/状态件**不再被拒** —— 否则没有明文剧本的作恒黑。
        assert!(vm_ui_layer_allowed("cgsys/title/logo", true, false));
        assert!(vm_ui_layer_allowed("cgsys/extra/rpmode/x", true, false));
        assert!(vm_ui_layer_allowed(
            "cgsys/main/pop/tip_config",
            true,
            false
        ));
        // debug 覆盖层两种情形都挡
        assert!(!vm_ui_layer_allowed("cgsys/debug/overlay", true, false));
    }

    #[test]
    fn tachie_char_key_groups_variants() {
        // 同角色变体共享键(按角色占一层,不叠层)
        assert_eq!(tachie_char_key("L_NYA_1A0100"), "L_NYA");
        assert_eq!(tachie_char_key("L_NYA_1A0200"), "L_NYA");
        assert_eq!(tachie_char_key("A_HAN_2B0200"), "A_HAN");
    }

    #[test]
    fn tachie_float_is_bounded_and_periodic() {
        // 正弦浮动:幅度有界;一个周期后回到同值
        let mut max = 0.0f32;
        for i in 0..280 {
            let dy = tachie_float_dy(i as f32 * 0.01, 0.0);
            max = max.max(dy.abs());
        }
        assert!((max - TACHIE_FLOAT_AMP).abs() < 0.5, "max={max}");
        let a = tachie_float_dy(0.3, 1.2);
        let b = tachie_float_dy(0.3 + TACHIE_FLOAT_PERIOD, 1.2);
        assert!((a - b).abs() < 1e-3);
        // 相位不同 → 偏移不同(三玩偶错相位)
        assert!((tachie_float_dy(0.7, 0.0) - tachie_float_dy(0.7, 1.5)).abs() > 0.1);
    }

    #[test]
    fn adv_row_defs_are_on_screen() {
        assert_eq!(ADV_BUTTONS.len(), 12, "参考图主行 = 10 底行 + home/power");
        for &(_, x, y, w, h, _, segs) in ADV_BUTTONS {
            assert!(segs >= 3, "至少 off/on/over 三态");
            assert!(x >= 0.0 && y >= 0.0);
            assert!(x + w as f32 <= logical_w(), "x+ w 越界");
            assert!(y + h as f32 <= logical_h(), "y + h 越界");
        }
    }
}

#[cfg(test)]
mod sound_bridge_tests {
    //! 成果 162：`SOUND` 事件 → 音频声层命令的映射与候选名生成。

    use super::*;

    fn sound_event(file: Option<&[u8]>) -> yuris_vm::VmEvent {
        yuris_vm::VmEvent::Sound {
            pc: 7,
            id: Some("BGM/B1".into()),
            enabled: Some(1),
            file: file.map(|f| f.to_vec()),
            play: Some(1),
            pause: None,
            fade_ms: Some(800),
            start_ms: Some(-1000),
            restart_ms: Some(3200),
            end_ms: Some(9000),
            loop_count: Some(3),
            volume: Some(200),
            pan: Some(100),
            pitch: Some(300),
            speed: Some(512),
            param_count: 10,
        }
    }

    #[test]
    fn full_parameter_mapping() {
        let ev = sound_event(None);
        let cmd = sound_command_from_event(&ev).expect("mapped");
        assert_eq!(cmd.id, "BGM/B1");
        assert_eq!(cmd.enabled, Some(1));
        assert_eq!(cmd.play, Some(1));
        assert_eq!(cmd.pause, None);
        assert_eq!(cmd.fade_ms, Some(800));
        assert_eq!(cmd.start_ms, Some(-1000));
        assert_eq!(cmd.restart_ms, Some(3200));
        assert_eq!(cmd.end_ms, Some(9000));
        assert_eq!(cmd.loop_count, Some(3));
        assert_eq!(cmd.volume, Some(200));
        assert_eq!(cmd.pan, Some(100));
        assert_eq!(cmd.pitch, Some(300));
        assert_eq!(cmd.speed, Some(512));
        // FILE 不写入命令：字节由播放器解析后直接 `load_bytes`。
        assert!(cmd.file.is_none());
    }

    #[test]
    fn empty_id_is_not_mapped() {
        let mut ev = sound_event(None);
        if let yuris_vm::VmEvent::Sound { id, .. } = &mut ev {
            *id = Some(String::new());
        }
        assert!(sound_command_from_event(&ev).is_none());
    }

    #[test]
    fn layer_prefix_is_stripped() {
        assert_eq!(strip_layer_prefix(":/S0/bgm01"), "bgm01");
        assert_eq!(strip_layer_prefix("bgm01"), "bgm01");
        assert_eq!(strip_layer_prefix_bytes(b":/S0/bgm01"), b"bgm01");
        assert_eq!(strip_layer_prefix_bytes(b"bgm01"), b"bgm01");
    }

    #[test]
    fn candidates_cover_manual_extension_order() {
        let c = audio_name_candidates(b"bgm/theme");
        let shown: Vec<String> = c
            .iter()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect();
        assert_eq!(shown[0], "bgm/theme.ogg");
        assert_eq!(shown[1], "bgm/theme.wav");
        assert_eq!(shown[2], "bgm/theme");
        // 带扩展名 = 只试原名。
        let c = audio_name_candidates(b"bgm/theme.ogg");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn bare_name_tries_audio_roots() {
        let shown: Vec<String> = audio_name_candidates(b"theme")
            .iter()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect();
        assert!(shown.iter().any(|s| s == "bgm/theme.ogg"), "{shown:?}");
        assert!(shown.iter().any(|s| s == "se/theme.ogg"), "{shown:?}");
        assert!(shown.iter().any(|s| s == "voice/theme.ogg"), "{shown:?}");
    }

    #[test]
    fn cp932_file_name_survives() {
        // "琴音" 的 CP932 字节；任何 UTF-8 lossy 都会毁掉它。
        let name = b"voice/\x8bg\x89\xb9";
        let c = audio_name_candidates(name);
        assert_eq!(&c[0][..6], b"voice/");
        assert_eq!(&c[0][6..10], &name[6..]);
    }
}
