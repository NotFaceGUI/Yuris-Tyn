//! yuris-cli 可执行文件:兼容内核播放器(桌面壳)。
//!
//! 桌面平台壳:winit 事件循环 + 窗口创建 + 键鼠输入翻译。播放核心
//! (PlayerCore/Player/scenario/音频)已抽至 `yuris-player-core`,与
//! Android 壳(`neko-android`)共用。
//!
//! `run <游戏目录>`:双脚本系统并行驱动 ——
//! 1. **YSTB VM**:Bootstrap 启动链(SYSTEM_START)→ 帧节拍驱动
//!    (WAIT FRAME/TIME 挂起恢复)→ SceneBridge(CG/CGEND/TEXT → 图层);
//! 2. **scenario 播放器**(P7.3):sc.ypf 明文剧本(Big5)→ 背景/立绘/
//!    淡入淡出/双语台词(点击推进)。
//!
//! 用法:cargo run --release -p yuris-cli -- run "AnimalTrailGirlishSquare 2"

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use yuris_render::WgpuBackend;
use yuris_vm::bridge::SceneBridge;
use yuris_vm::boot::Bootstrap;
use yuris_vm::host::PacFileIndex;

use yuris_player_core::{load_cjk_font, Player, PlayerCore, ScenarioPlayer};
use yuris_stage;

/// 记一行日志（统一走 `yuris-log`；等级由 `YURIS_LOG_LEVEL` 控制）。
macro_rules! plog {
    ($lvl:expr, $($arg:tt)*) => {
        yuris_log::log!($lvl, $($arg)*)
    };
}

fn main() {
    // ---- Windows 高精度节拍(成果 167)----
    //
    // 默认系统定时器精度 15.6ms:事件循环的 `ControlFlow::WaitUntil` 按
    // 60fps 网格(16.67ms)等醒,实测落到 ~23ms/帧(≈43fps),全局拉慢
    // 1.38 倍 —— TIME 型等待按墙钟走、FRAME 型等待按逻辑帧走,两者口径
    // 就此错开。`timeBeginPeriod(1)` 提到 1ms 精度(进程级,退出由 OS
    // 回收;官方引擎主循环同为毫秒级节拍)。
    #[cfg(windows)]
    {
        #[link(name = "winmm")]
        extern "system" {
            fn timeBeginPeriod(period: u32) -> u32;
        }
        unsafe {
            timeBeginPeriod(1);
        }
    }

    // ---- 日志:统一入口(yuris-log),必须先于任何诊断输出 ----
    // 等级/文件/颜色全部由环境变量控制(`YURIS_LOG_LEVEL` / `YURIS_LOG_FILE`
    // / `YURIS_LOG_COLOR`);旧的独立彩色日志终端(Windows Terminal 子进程)
    // 已退役 —— 日志分流不再是等级管理的组成部分。
    yuris_log::init_from_env();

    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_default();

    if cmd != "run" {
        yuris_log::error!(
            "用法: yuris-cli run <游戏目录> [--lenient] [--at <标签>] [--key-hex <8位>]
             环境变量: YURIS_LOG_LEVEL=trace|debug|info|warn|error|fatal|off  YURIS_LOG_FILE=<路径>"
        );
        std::process::exit(2);
    }
    let dir = args.next().unwrap_or_else(|| ".".into());
    let rest: Vec<String> = args.collect();

    plog!(
        yuris_log::Level::Info,
        "YurisKernel v{} · yuris-cli · 日志等级 {}",
        yuris_core::VERSION,
        yuris_log::level()
    );

    let at_label: Option<String> = rest
        .iter()
        .position(|a| a == "--at")
        .and_then(|i| rest.get(i + 1).cloned());
    // 调试项:YSTB 4 字节循环 XOR 密钥(不同游戏各异)。缺省 = 本项目样本密钥。
    // 正式方案是启动链对首个 YSTB 条目自动 guess-key(见 yuris ystb guess-key)。
    let _ystb_key: [u8; 4] = rest
        .iter()
        .position(|a| a == "--key-hex")
        .and_then(|i| rest.get(i + 1))
        .and_then(|hex| {
            if hex.len() == 8 {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .map(|v| v.to_be_bytes())
            } else {
                None
            }
        })
        .unwrap_or([0x2b, 0x90, 0x4f, 0x93]);
    // 调试项:非 strict 模式 —— Unsupported 命令记录事件后继续(默认 strict 挂起)。
    let lenient = rest.iter().any(|a| a == "--lenient");
    // --dump-tex <子串>:把封包内匹配的资源解码落 PNG(素材取证;成果 127)。
    let dump_tex: Option<String> = rest
        .iter()
        .position(|a| a == "--dump-tex")
        .and_then(|i| rest.get(i + 1).cloned());
    // ---- 性能/取证开关(成果 127)----
    //   --bench <秒>   跑 N 秒后打印帧耗时统计(p50/p95/最差;VM 与渲染分段)并退出
    //   --shot <帧号>  该帧渲染完成后,按逻辑画布分辨率截图落 PNG(--shot-dir,缺省 shot/)
    let bench_secs: u64 = rest
        .iter()
        .position(|a| a == "--bench")
        .and_then(|i| rest.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut shots: Vec<u64> = Vec::new();
    {
        let mut it = rest.iter().peekable();
        while let Some(a) = it.next() {
            if a == "--shot" {
                if let Some(v) = it.next().and_then(|v| v.parse::<u64>().ok()) {
                    shots.push(v);
                }
            }
        }
    }
    // `--quit-after <帧>`:跑满 N 帧后自动退出(诊断批跑的后台任务 bez 阻塞根:
    // winit 程序不退出 ⇒ 后台任务永远「正在获取任务输出」)。与之配套,
    // **给了 `--shot` 时缺省即自动退出**(截完最后一个 shot 帧 = 诊断已完整;
    // 显式 `--quit-after 0` 关闭该缺省,恢复常驻)。
    //
    // **`--shot` 批跑 = 帧量化动画时钟**(2026-10-17,成果 184):正常游玩时
    // 动画读**真实时间**(帧率无关);但截图取证要求同帧三跑可复现,
    // 墙钟做不到。批跑模式下自动置 `YURIS_FRAME_ANIM=1` 切回帧量化。
    // 显式给出 `YURIS_FRAME_ANIM` 时以后者为准。
    if !shots.is_empty() && std::env::var_os("YURIS_FRAME_ANIM").is_none() {
        std::env::set_var("YURIS_FRAME_ANIM", "1");
    }
    let quit_after: Option<u64> = {
        let explicit = rest
            .iter()
            .position(|a| a == "--quit-after")
            .and_then(|i| rest.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok());
        match explicit {
            Some(v) => Some(v),
            None if !shots.is_empty() => shots.iter().copied().max(),
            None => None,
        }
    };
    let shot_dir = rest
        .iter()
        .position(|a| a == "--shot-dir")
        .and_then(|i| rest.get(i + 1))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("shot"));
    // `--click x,y` / `--click-frame n`:无头复现点击(诊断跳转链)。
    let click_at: Option<(i64, i64)> = rest
        .iter()
        .position(|a| a == "--click")
        .and_then(|i| rest.get(i + 1))
        .and_then(|v| {
            let (x, y) = v.split_once(',')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        });
    let click_frame: u64 = rest
        .iter()
        .position(|a| a == "--click-frame")
        .and_then(|i| rest.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(240);
    let game_dir = std::path::PathBuf::from(&dir);
    if !game_dir.is_dir() {
        yuris_log::error!("游戏目录不存在: {}", game_dir.display());
        std::process::exit(2);
    }

    // ---- 包发现(通用:根目录 + pac/ 子目录,逐包自动判别索引方言)----
    let index = Arc::new(
        PacFileIndex::scan_game_dir_auto(&game_dir).expect("扫描封包"),
    );
    if let Some(pat) = &dump_tex {
        let mut n = 0;
        let names: Vec<String> = discover_packs(&game_dir)
            .iter()
            .flat_map(|(_, idx)| idx.entries.iter().map(|e| String::from_utf8_lossy(&e.name).to_string()))
            .collect();
        for name in &names {
            let name = name.clone();
            if name.to_ascii_lowercase().contains(&pat.to_ascii_lowercase()) {
                if let Some(data) = index.read_image_bytes(&name) {
                    if let Ok(img) = image::load_from_memory(&data) {
                        let out = std::path::PathBuf::from("dump_tex").join(
                            format!("{n:02}_{}", name.replace(['\\', '/'], "_")),
                        );
                        let _ = std::fs::create_dir_all(&out);
                        let _ = img.to_rgba8().save(out.with_extension("png"));
                        plog!(yuris_log::Level::Info, "[dump-tex] {name} → {}", out.display());
                        n += 1;
                    }
                }
            }
        }
        if n == 0 {
            plog!(yuris_log::Level::Error, "[dump-tex] 无匹配条目:{pat}");
        }
        std::process::exit(0);
    }
    let packs = discover_packs(&game_dir);
    if packs.is_empty() {
        yuris_log::error!("游戏目录内没有可解析的 .ypf 封包: {}", game_dir.display());
        std::process::exit(2);
    }
    plog!(yuris_log::Level::Info, "[player] 封包 {} 个", packs.len());

    // ---- 脚本包 = 含 ysb in 下 yst*.ybn 的那一个 ----
    let (script_pack, script_idx) = packs
        .iter()
        .find(|(_, idx)| idx.entries.iter().any(|e| yuris_vm::host::is_script_entry(&e.name)))
        .map(|(p, i)| (p.clone(), i))
        .unwrap_or_else(|| {
            plog!(yuris_log::Level::Error, "!! 没有封包包含脚本条目(ysbin/yst*.ybn)");
            std::process::exit(2);
        });
    let scripts = script_idx
        .entries
        .iter()
        .filter(|e| yuris_vm::host::is_script_entry(&e.name))
        .count();
    plog!(yuris_log::Level::Info, "[player] 脚本包 {} 脚本 {} 个", script_pack.display(), scripts);

    // ---- 启动链:宿主自动判别方言 + 自动猜脚本密钥 ----
    let host = yuris_vm::host::YpfScriptHost::open(&script_pack).expect("打开脚本包");
    let index_dialect = host.dialect();
    plog!(
        yuris_log::Level::Info,
        "[player] 索引方言 {:?}  脚本密钥 {:02x?}",
        index_dialect,
        host.script_key()
    );
    let booted = Bootstrap::boot_with_host(host, None).expect("启动链编排");
    plog!(
        yuris_log::Level::Info,
        "[player] 入口 script {} pc {}（YSVR {} 条）分派表: {}",
        booted.script_id, booted.entry_pc, booted.applied_ysvr, booted.command_profile_source
    );
    // 标题/标识所需的信息在 `booted` 拆开前取出。
    let caption = booted.caption.clone();
    // 游戏自身分辨率(逻辑画布真值;成果 121)
    let booted_screen = booted.screen;
    let engine_desc = booted.command_profile_source.clone();
    let ypf_version = script_idx.header.version;
    let mut vm = booted.vm;
    vm.set_file_probe(index.clone());
    // `yscfg.dat`(游戏运行期配置;存在性 Confirmed / 字段语义 Unknown)。
    // 引擎启动即读(字符串 `\yscfg.dat`@v494 0x5d57d8);本内核结构解析
    // + 日志留痕,不做行为门控(不猜)。
    match yuris_format::yscfgdat::YscfgDat::from_file(&game_dir.join("yscfg.dat")) {
        Ok(cfg) => yuris_log::info!(
            "[yscfg.dat] 32 槽:{:02x?}",
            cfg.slots.iter().map(|v| v.to_string()).collect::<Vec<_>>()
        ),
        Err(e) if e.to_string().contains("长度") => {
            yuris_log::warn!("[yscfg.dat] 存在但结构异常: {e}")
        }
        Err(_) => yuris_log::debug!("[yscfg.dat] 不存在(引擎按缺省零值启动)"),
    }
    // **运行时 = 严格模式 + 致命错误窗(2026-10-17 用户要求)**:
    // - strict(默认):任何未实现/未建模命令都**挂起**,不再「记录后继续」;
    // - fatal_reporting:挂起的同时登记 FatalReport,下一帧由
    //   `PlayerCore::update_fatal_overlay` 把错误窗盖在画布中央。
    // `--lenient` 是**取证专用**旁路(仍会打横幅,但继续执行)。
    vm.set_fatal_reporting(true);
    if lenient {
        vm.set_strict(false);
        yuris_log::alert_once(
            module_path!(),
            "cli-lenient",
            "--lenient 已开启:未实现命令只记录不中断\n\
             注意: 场景/变量的推进结果**不可信**;正式游玩请去掉该开关"
                .to_string(),
        );
    }
    // **确定性种子(成果 172)**:VM 的 MT19937 缺省用时间派生种子
    // (引擎真种子未取证),⇒ 凡走 `MATH RND` 的分支每次运行不同,
    // 同帧截图 SHA 三跑不一致。播放器缺省注入固定种子;
    // `YURIS_MATH_SEED=<n>` 可覆盖(复现特定随机流)。
    let seed = std::env::var("YURIS_MATH_SEED")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(0x5eed_1234);
    vm.set_math_seed(seed);
    // 播放器不需要逐组执行事件(引擎不产生;那是 golden/诊断面)。
    // boot 单帧可达 85 万组 ⇒ 关掉可省同等数量的事件对象(成果 171)。
    vm.set_group_events(false);

    // ---- scenario 文件(明文 .txt;该族封包没有则空)----
    let mut sc_files: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, idx) in &packs {
        let txts: Vec<Vec<u8>> = idx
            .entries
            .iter()
            .filter(|e| {
                let n = String::from_utf8_lossy(&e.name);
                n.to_ascii_lowercase().ends_with(".txt")
            })
            .map(|e| e.name.clone())
            .collect();
        if txts.is_empty() {
            continue;
        }
        let Ok(mut r) = yuris_format::ypf::YpfReader::from_index(
            path,
            yuris_format::ypf::YpfIndex::from_path_auto(path).expect("idx").0,
        ) else {
            continue;
        };
        for name in txts {
            if let Ok(data) = r.read(&name) {
                sc_files.push((String::from_utf8_lossy(&name).to_string(), data));
            }
        }
    }
    plog!(yuris_log::Level::Info, "[player] scenario 文件 {} 个", sc_files.len());
    let scenario = ScenarioPlayer::new(sc_files);

    // ---- 音频包(与资源包同源)----
    //
    // **复用资源索引,不再重扫(2026-10-18 启动性能修复)**:
    // 旧实现把同一个 3.3GB 包**索引了两遍**(`scan_game_dir_auto` 每次都要
    // 读完整个条目表 + 逐条归一化/插入三张结构)。实测这一步占启动 ~5s。
    // 两者查的是同一批封包(`bgm/se/voice/sysse/sysvo` 都在同一包里),
    // 索引本身与用途无关 ⇒ 直接共享 `Arc`。
    let audio_packs = Arc::clone(&index);
    let audio = yuris_player_core::audio::Audio::new();

    // ---- CJK 字体(繁中优先;按平台回退)----
    let font_bytes = load_cjk_font().expect("系统 CJK 字体");
    let font = fontdue::Font::from_bytes(
        font_bytes,
        fontdue::FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: true,
        },
    )
    .expect("字体解析");

    let mut core = PlayerCore {
        index,
        vm,
        bridge: SceneBridge::new(),
        loaded: HashSet::new(),
        text_slots: (0..yuris_player_core::TEXT_SLOT_MAX).map(|_| None).collect(),
        text_slot_seq: 0,
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
        sprite_moves: Vec::new(),
        tachie_float: std::collections::HashMap::new(),
        float_clock: Instant::now(),
        anim_epoch: Instant::now(),
        anim_frames: 0,
        anim_debt_ms: 0.0,
        audio_packs,
        audio,
        choices: None,
        last_cursor_px: None,
        frame_clicked: false,
        title_buttons: Vec::new(),
        request_title_load: false,
        request_quit: false,
        globals: std::collections::HashMap::new(),
        debug_frames: 0,
        stage: yuris_stage::Stage::new(),
        snap_seq: HashMap::new(),
        layer_z_memo: HashMap::new(),
        subui: yuris_player_core::SubUi::None,
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
        frame_budget: yuris_player_core::frame_budget_from_env(),
        fatal_overlay: None,
        fatal_scroll: 0,
        fatal_max_scroll: 0,
        game_dir: game_dir.clone(),
    };
    // `SOUNDINFO` 查询通道：VM 不依赖音频层，由播放器侧注入（见
    // `yuris_player_core::audio_probe`）。必须在任何 VM 步进之前接好。
    yuris_player_core::audio_probe::install_audio_probe(&mut core.vm, core.audio.system());
    let mut player = Player {
        game_dir,
        core,
        scenario,
        window: None,
        last_frame: Instant::now(),
        last_present: Instant::now(),
        next_frame_at: Instant::now(),
    };
    // scenario 入口:scenario_start.txt → RELEASE 段(构建模式段选择,Likely)
    // scenario 入口:--at 调试跳转 或 scenario_start.txt RELEASE 段
    let started = if let Some(l) = &at_label {
        match player.scenario.goto(l) {
            Ok(()) => true,
            Err(e) => {
                plog!(yuris_log::Level::Warn, "[scenario] --at {l} 跳转失败: {e}");
                false
            }
        }
    } else {
        player
            .scenario
            .start("scenario\\scenario_start.txt", "SCENARIO_START_RELEASE")
            .is_ok()
    };
    if !started {

    }

    // ---- 窗口标题:游戏自己的标题 + 内核/CLI 标识与版本 ----
    // 游戏标题取自游戏自身数据(`ysbin\yscfg.ybn` 的 caption 字段),
    // **写死任何一款游戏的名字都是错的**;caption 缺失时回退到目录名。
    let game_title = caption.clone().unwrap_or_else(|| {
        player
            .game_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未知作品".into())
    });
    let window_title = format!(
        "{game_title} — YurisKernel v{ver} · yuris-cli · {engine} · YPF v{ypf} · {dialect:?}",
        ver = yuris_core::VERSION,
        engine = engine_desc,
        ypf = ypf_version,
        dialect = index_dialect,
    );
    plog!(yuris_log::Level::Info, "[player] 窗口标题: {window_title}");

    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    // 逻辑画布 + 窗口尺寸 = **游戏自身 `yscfg.ybn` 的分辨率**(成果 121)。
    // 内核曾把两者分别写死为 1920×1080 / 1280×720(最初样本 v555 的值),
    // 款款不同 ⇒ 实机「分辨率有问题」「CG 混乱」。缺省回退 1280×720 保持兼容。
    let game_screen = booted_screen.unwrap_or((1280, 720));
    yuris_render::set_logical_canvas(game_screen.0 as f32, game_screen.1 as f32);
    plog!(
        yuris_log::Level::Info,
        "[player] 逻辑画布 = {}×{}（来自游戏 yscfg.ybn；缺省 1280×720 表示未配置）",
        game_screen.0,
        game_screen.1
    );
    let mut wrapper = AppWrapper {
        player: Some(player),
        harness: std::rc::Rc::new(std::cell::RefCell::new(Harness {
            bench: if bench_secs > 0 {
                Some(Bench {
                    until: std::time::Instant::now() + std::time::Duration::from_secs(bench_secs),
                    frames: 0,
                    total_ns: 0,
                    vm_ns: 0,
                    render_ns: 0,
                    worst_ns: 0,
                    samples_ns: Vec::new(),
                })
            } else {
                None
            },
            shots,
            shot_dir,
            click_at,
            click_frame,
            press_at: None,
            frame_no: 0,
            quit_after,
        })),
        title: window_title,
        window_size: game_screen,
    };
    if let Err(e) = event_loop.run_app(&mut wrapper) {
        yuris_log::report_fatal_error(module_path!(), "事件循环异常退出", e);
        std::process::exit(1);
    }
}

/// 客户区像素 → 逻辑坐标(整窗线性映射;letterbox 误差由渲染侧吸收)。
/// winit 0.30 ApplicationHandler 要求 &mut App;包一层以支持延迟构造窗口。
/// 取证状态(bench + 截图;成果 127)。
struct Harness {
    bench: Option<Bench>,
    shots: Vec<u64>,
    shot_dir: std::path::PathBuf,
    /// 本进程帧号(从 1 起;不依赖 YURIS_LOG 的 debug_frames)。
    frame_no: u64,
    /// 到此帧(含)后自动退出事件循环(诊断批跑;None = 常驻)。
    quit_after: Option<u64>,
    /// `--click x,y`:在该帧注入一次逻辑坐标点击。
    click_at: Option<(i64, i64)>,
    click_frame: u64,
    /// 最近一次**真实按下**的 (时刻, 当时帧号, 按钮名)。
    /// `YURIS_TRACE_CLICK=1` 时在下一帧出图后打印「按下→出图」的墙钟差,
    /// 用于客观判定「点击延迟」到底是多少毫秒(而不是靠观感)。
    press_at: Option<(std::time::Instant, u64, String)>,
}

/// `--bench` 统计(成果 127):帧耗时(含 VM tick 与渲染两段)。
struct Bench {
    until: std::time::Instant,
    frames: u64,
    total_ns: u128,
    vm_ns: u128,
    render_ns: u128,
    worst_ns: u128,
    /// **逐帧耗时样本**(成果 184):用于分位数与「稳态均值」。
    ///
    /// 均值的口径缺陷(实测 2026-10-17):boot 前 4 帧 =
    /// 1154 + 251 + 1050 + 205ms ≈ 2.66s,而 8 秒 bench 总共约 5.4s ——
    /// **一半的「均值」来自这 4 帧**,稳态真实帧耗被完全淹没
    /// (`YURIS_TICK_PROF` 实测稳态 5–12ms,早已超过 60fps)。
    /// 判据必须看 p50/p95 与「丢帧计数」。
    samples_ns: Vec<u64>,
}

struct AppWrapper {
    player: Option<Player>,
    /// bench/截图取证状态(`Rc<RefCell>`:window_event 里 player 借用 self.player
    /// 期间还要读写这些 —— RefCell 借用与字段借用不相交)。
    harness: std::rc::Rc<std::cell::RefCell<Harness>>,
    /// 窗口标题(游戏标题 + 内核/CLI 标识与版本;构造前算好)。
    title: String,
    /// 窗口尺寸 = **游戏自身分辨率**(`yscfg.ybn`;缺省回退 1280×720)。
    ///
    /// **勘误(2026-10-13,成果 121)**:旧实现写死 `1280×720`(窗口)与
    /// `1920×1080`(逻辑画布)—— 而款款不同(本作 1100×825)。后果即实机
    /// 报告的「分辨率有问题 + CG 位置/缩放全乱」。
    window_size: (u32, u32),
}

impl winit::application::ApplicationHandler for AppWrapper {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Some(player) = self.player.as_mut() else {
            return;
        };
        if player.window.is_some() {
            return;
        }
        let attrs = winit::window::Window::default_attributes()
            .with_title(self.title.clone())
            // 窗口尺寸 = 游戏自身分辨率(成果 121);写死会让窗口比例与画面不符。
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.window_size.0,
                self.window_size.1,
            ));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                yuris_log::report_fatal_error(module_path!(), "创建窗口失败", e);
                event_loop.exit();
                return;
            }
        };
        match WgpuBackend::new(window.clone()) {
            Ok(b) => {
                player.core.backend = Some(b);
                player.window = Some(window.clone());
            }
            Err(e) => {
                // **崩坏级**:无法创建图形后端 ⇒ 画布错误窗会尽量渲染；
                // 若连错误窗都无法上屏，报告仍留在 stderr/日志文件。
                yuris_log::report_fatal_error(module_path!(), "wgpu 初始化失败", e);
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let Some(p) = self.player.as_mut() else {
            return;
        };
        if p.window.is_none() {
            return;
        }
        if yuris_log::has_fatal() {
            // 错误窗是静态画面：10fps 足够维持 present（窗口暴露/改尺寸后
            // 恢复画面），同时避免满帧重绘白白吃 GPU。
            if let Some(w) = p.window.as_ref() {
                w.request_redraw();
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                Instant::now() + std::time::Duration::from_millis(100),
            ));
            return;
        }
        // **帧节拍(成果 124)**:定时等到下一帧时刻,到点才请求重绘。
        //
        // 旧实现是 `about_to_wait`「距上帧 ≥15ms 就 request_redraw」**加上**
        // `RedrawRequested` 末尾再 `request_redraw()` —— 两者互相续期,事件循环
        // **从不休眠**:CPU 满载、窗口拖动/改尺寸期间无响应(实机「卡顿严重」
        // 「窗口移动问题」)。现在由 `WaitUntil` 定时器驱动,空闲时真正阻塞。
        // 帧节拍锚点 = 上次 present(vblank)+ 1ms 起工(成果 127;实测勘误:
        // 「间隔-3ms 起工」是错的 —— 工作落在窗口末端,present 必然错过下一
        // 个 vblank 再等一整段)。vblank 后立刻起工 ⇒ 工作(实测 ≤11ms)落在
        // 同一个 16.6ms 窗口内,present 恰好赶上下一个 vblank;闲置帧的剩余
        // 时间由 Fifo present 阻塞,不烧 CPU。详见 Player::last_present 文档。
        // **锁定脚本帧率(成果 162)**:`FPS[SET=n]` 是引擎的帧推进基准
        // (手册 `fps/fps.html`;本作 s0 g318 设 60)。内核此前只把 FPS 当
        // 普通子系统记录、从不消费 ⇒ 帧间隔完全由 present 节拍决定。
        //
        // 现在:帧间隔 = 1/n 秒,前提是渲染器能把该间隔兑现(高刷屏 Fifo
        // 会按 vblank 量化,低于屏率时 WaitUntil 主导)。`--fps` 可强制覆盖
        // (对拍/诊断用;`YURIS_FPS` 同义)。
        let fps = p
            .core
            .vm
            .fps
            .or_else(|| {
                std::env::var("YURIS_FPS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u32>().ok())
            })
            .filter(|n| *n > 0)
            .unwrap_or(60);
        let frame_gap = std::time::Duration::from_secs_f64(1.0 / f64::from(fps));
        // **节拍锚点 = 固定的帧网格,不是「present 完成时刻 + gap」**
        // (勘误 2026-10-17):后者会把本帧工作耗时叠加进间隔 —— 实测
        // 目标 16.6ms 实际跑出 18~27ms(工作 2~10ms 全部累加),300 帧
        // 拖到 8 秒。改为维护 `next_frame_at` 网格:到点就推进一个 gap;
        // 若已落后超过一整个 gap(卡顿/丢帧),直接从当前时刻重新起网格,
        // 不追赶 —— 与引擎「按帧率采样、慢帧不补」的行为一致。
        let now = Instant::now();
        let mut fire = false;
        if now >= p.next_frame_at {
            // 到点:本帧起工(必须**先**请求重绘,再推进网格 ——
            // 顺序写反会让网格先跑到未来,request_redraw 永不触发,
            // 表现为整窗黑屏不动)。
            fire = true;
            while p.next_frame_at <= now {
                p.next_frame_at += frame_gap;
            }
        }
        let next = p.next_frame_at;
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(next));
        if fire {
            if let Some(w) = p.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let harness = self.harness.clone();
        let Some(player) = self.player.as_mut() else {
            return;
        };
        // 连续渲染失败计数（错误窗自愈/退出判定；见 RedrawRequested）。
        static RENDER_FAILURES: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);
        let render_failures = RENDER_FAILURES.load(std::sync::atomic::Ordering::Relaxed);
        match event {
            winit::event::WindowEvent::CloseRequested => event_loop.exit(),
            winit::event::WindowEvent::KeyboardInput { event, .. } => {
                if event.state == winit::event::ElementState::Pressed
                    && event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                {
                    // 错误窗在场:Esc = **退出游戏**(不是「关闭面板继续玩」)。
                    // 运行时已停止推进,没有任何恢复路径;面板底部也这样写。
                    if yuris_log::has_fatal() {
                        player.core.dismiss_fatal_overlay();
                    }
                    event_loop.exit();
                }
                if yuris_log::has_fatal() {
                    // 崩坏级：错误窗优先，游戏输入全部屏蔽。
                    return;
                }
                // 键盘事件统一进输入层（P9.2）：四态计数 / `_INP` / `_KEYLAST`
                // 都由 `yuris-input` 维护，壳只负责翻译。
                if let Some(key) = yuris_input::winit_adapter::map_key(&event.logical_key) {
                    player.push_input(yuris_input::PlatformEvent::Key {
                        key,
                        pressed: event.state == winit::event::ElementState::Pressed,
                        repeat: event.repeat,
                    });
                }
                if event.state == winit::event::ElementState::Pressed {
                    match event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::F5) => {
                            player.quick_save();
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::F9) => {
                            player.quick_load();
                        }
                        _ => {}
                    }
                }
            }
            winit::event::WindowEvent::CursorMoved { position, .. } => {
                if yuris_log::has_fatal() {
                    return;
                }
                // **确定性命中(成果 172)**:`--bench`/`--shot` 是无人值守批跑,
                // 真实鼠标位置随窗口/桌面状态漂移 ⇒ 标题按钮悬停三态不同 ⇒
                // 同帧截图三跑 SHA 不一致(实测差异区域恰好落在按钮条)。
                // 批跑模式下屏蔽真实指针(保持初始光标),`YURIS_LIVE_CURSOR=1`
                // 可强制恢复(交互调试用)。
                // **只有 `--bench` 才屏蔽真实指针**(无人值守性能批跑)。
                // `--shot` 是「跑游戏并截图」:用户**要能点** —— 早期版本把
                // `--shot` 也算进 batch,导致按了 `--shot` 就没法操作(实机反馈)。
                // 截图帧的确定性另有保障(逻辑时钟 + 固定随机种子)。
                let batch = {
                    let h = harness.borrow();
                    h.bench.is_some() && std::env::var_os("YURIS_LIVE_CURSOR").is_none()
                };
                if batch {
                    return;
                }
                player.core.last_cursor_px = Some((position.x, position.y));
                player.push_input(yuris_input::PlatformEvent::PointerMoved {
                    x: position.x,
                    y: position.y,
                });
            }
            winit::event::WindowEvent::MouseInput { state, button, .. } => {
                if yuris_log::has_fatal() {
                    // **错误窗在场:点按不关闭** —— 面板会明说「只能关闭游戏」。
                    // 这里把鼠标输入整体吞掉,避免误触退出或漏到游戏层。
                    return;
                }
                // `MouseInput` 不带坐标；输入层用最近一次指针指针位置。
                player.push_input(yuris_input::PlatformEvent::PointerButton {
                    button: yuris_input::winit_adapter::map_button(button),
                    pressed: state == winit::event::ElementState::Pressed,
                });
                // **点击即时出帧(成果 180)**:按下是离散事件,而重绘原本
                // 只由 `about_to_wait` 的帧网格定时器驱动 ⇒ 用户点击后要
                // 等下一格 1/60s;再叠加 Fifo present 的 vblank 阻塞,实机
                // 观感"点了要停一下"(最坏 ~33ms,与原版差距明显)。
                //
                // 修法 = **把帧网格拉回当前时刻**:点击时把 `next_frame_at`
                // 重置为 now,`about_to_wait` 立刻 fire 一次;网格随后从
                // now 起重新铺。这会丢掉不足一格的等待(最多 16.6ms),
                // 属于「输入优先」的正常交互行为;不追赶、不加速后续帧。
                if state == winit::event::ElementState::Pressed {
                    player.next_frame_at = std::time::Instant::now();
                    if std::env::var_os("YURIS_TRACE_CLICK").is_some() {
                        let h = harness.borrow();
                        let f = h.frame_no;
                        drop(h);
                        harness.borrow_mut().press_at =
                            Some((std::time::Instant::now(), f, format!("{button:?}")));
                    }
                    if let Some(w) = player.window.as_ref() {
                        w.request_redraw();
                    }
                }
            }
            winit::event::WindowEvent::MouseWheel { delta, .. } => {
                if yuris_log::has_fatal() {
                    // **错误窗在场**:滚轮改为滚动面板(不转交游戏)。
                    //
                    // 单位必须分清(winit 两路语义不同,踩错就会滚飞):
                    // - `LineDelta` 是**行**(正 = 上/远离用户)→ 一行按 60
                    //   逻辑像素折算,再乘输出缩放变物理像素;
                    // - `PixelDelta` 已经是**物理像素**(触控板/高精度滚轮)
                    //   → 直接用,绝不能再乘缩放。
                    // **滚轮向上 = 看更早内容 ⇒ scroll 减小**(两个分支已在
                    // 各自方向取好负号：LineDelta 的 +y、PixelDelta 的 +y
                    // 都表示「向上」)。
                    let px = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => {
                            let scale = player.core.fatal_output_scale_pub();
                            let n: f32 = if y.abs() >= 1.0 { y } else { y.signum() };
                            -(n * 60.0 * scale) as i32
                        }
                        winit::event::MouseScrollDelta::PixelDelta(p) => -(p.y as i32),
                    };
                    if px != 0 && player.core.scroll_fatal_overlay(px) {
                        if let Some(w) = player.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                // 行增量（正 = 上/远离用户，与 WHEEL_DELTA 同号）；
                // 像素增量按 60px/格折算，至少 1 格。
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y as f64,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if dy != 0.0 {
                    let step = if dy.abs() >= 1.0 {
                        if dy > 0.0 { 120 } else { -120 }
                    } else {
                        let units = (dy.abs() / 60.0).max(1.0);
                        ((if dy > 0.0 { 1.0 } else { -1.0 }) * 120.0 * units) as i32
                    };
                    let (x, y) = player.core.input.cursor_window_px();
                    player.push_input(yuris_input::PlatformEvent::Wheel {
                        delta: step,
                        x,
                        y,
                    });
                }
            }
            winit::event::WindowEvent::Focused(false) => {
                player.push_input(yuris_input::PlatformEvent::FocusLost);
            }
            winit::event::WindowEvent::Touch(touch) => {
                // **错误窗在场:触摸驱动面板滚动,点按关闭** —— 不转交游戏。
                // 拖动位移换算成物理像素(与排版同一坐标系),并记录起止点
                // 以区分「拖动」与「点按」(阈值 8 逻辑像素)。
                if player.core.fatal_overlay_active() {
                    struct Drag {
                        id: u64,
                        last_y: f64,
                    }
                    static DRAG: std::sync::Mutex<Option<Drag>> =
                        std::sync::Mutex::new(None);
                    let mut g = match DRAG.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    match touch.phase {
                        winit::event::TouchPhase::Started => {
                            *g = Some(Drag {
                                id: touch.id,
                                last_y: touch.location.y,
                            });
                        }
                        winit::event::TouchPhase::Moved => {
                            if let Some(d) = g.as_mut().filter(|d| d.id == touch.id) {
                                // 手指上移 = 内容下移(scroll 减小)。
                                // `Touch.location` 是**物理像素**(winit 语义),
                                // 与面板排版坐标系一致 ⇒ 不再乘 scale。
                                let dy = touch.location.y - d.last_y;
                                d.last_y = touch.location.y;
                                player.core.scroll_fatal_overlay(-(dy.round() as i32));
                                if let Some(w) = player.window.as_ref() {
                                    w.request_redraw();
                                }
                            }
                        }
                        winit::event::TouchPhase::Ended
                        | winit::event::TouchPhase::Cancelled => {
                            // 触摸结束只清拖动状态 —— **点按不关闭**(见面板提示)。
                            let _ = g.take();
                        }
                    }
                    return;
                }
                let phase = match touch.phase {
                    winit::event::TouchPhase::Started => yuris_input::TouchPhase::Started,
                    winit::event::TouchPhase::Moved => yuris_input::TouchPhase::Moved,
                    winit::event::TouchPhase::Ended => yuris_input::TouchPhase::Ended,
                    winit::event::TouchPhase::Cancelled => yuris_input::TouchPhase::Cancelled,
                };
                player.push_touch(touch.id, phase, touch.location.x, touch.location.y);
            }
            winit::event::WindowEvent::Moved(_) => {}
            winit::event::WindowEvent::Resized(size) => {
                if let Some(b) = player.core.backend.as_mut() {
                    b.resize(size.width, size.height);
                }
                player.resize_input(size.width, size.height);
            }
            winit::event::WindowEvent::RedrawRequested => {
                let frame_t = std::time::Instant::now();
                // `--click x,y --click-frame n`:该帧注入一次点击(无头复现跳转链)。
                {
                    let (at, fr, cur) = {
                        let h = harness.borrow();
                        (h.click_at, h.click_frame, h.frame_no)
                    };
                    if let Some((x, y)) = at {
                        if cur == fr {
                            player.inject_click(x, y);
                        }
                    }
                }
                player.tick();
                let vm_ns = frame_t.elapsed().as_nanos();
                if player.core.request_quit {
                    event_loop.exit();
                    return;
                }
                // ---- 启动/运行期诊断(黑屏排查用;`YURIS_LOG=1` 开)----
                if std::env::var("YURIS_LOG").is_ok() {
                    let n = player.core.debug_frames + 1;
                    player.core.debug_frames = n;
                    let dump_at: Vec<u64> = std::env::var("YURIS_DUMP_AT")
                        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
                        .unwrap_or_default();
                    if n == 1 || n % 120 == 0 || dump_at.contains(&n) {
                        let scene = player.core.bridge.scene();
                        let layers = scene.layers.len();
                        let visible = scene
                            .layers
                            .iter()
                            .filter(|l| l.visible && l.resource.is_some())
                            .count();
                        // 可见层的几何/状态摘要(成果 127 排错:x/y/z/α/过渡/混合)
                        let names = player.core.vm.cg_registry_snapshot();
                        let name_of = |id: u64| {
                            names
                                .iter()
                                .find(|(n, _, _, _)| yuris_vm::bridge::fnv1a(n) == id)
                                .and_then(|(n, _, _, _)| String::from_utf8(n.clone()).ok())
                                .unwrap_or_default()
                        };
                        let tex = |id: u64| {
                            player
                                .core
                                .backend
                                .as_ref()
                                .and_then(|b| b.image_size(id))
                                .map(|(w, h)| format!("{w}x{h}"))
                                .unwrap_or_else(|| "?".into())
                        };
                        let draws = scene
                            .layers
                            .iter()
                            .filter(|l| l.visible && l.resource.is_some())
                            .map(|l| {
                                format!(
                                    "[{} {}tex]@({},{},{},a{:.2}{}{})",
                                    name_of(l.id),
                                    tex(l.id),
                                    l.x as i32,
                                    l.y as i32,
                                    l.z,
                                    l.alpha,
                                    l.blend.to_engine(),
                                    l.transition
                                        .as_ref()
                                        .map(|t| format!(",T{:.2}", t.t))
                                        .unwrap_or_default(),
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        plog!(
                            yuris_log::Level::Debug,
                            "[frame {n:5}] 图层 {layers} 可见(有资源) {visible}  GPU纹理 {}  精灵 {}  UI钮 {}  CG注册 {}  文本 {:?}  draws=[{draws}]",
                            player.core.backend.as_ref().map(|b| b.image_count()).unwrap_or(0),
                            player.core.sprites.len(),
                            player.core.ui_buttons.len(),
                            player.core.vm.cg_registry_len(),
                            scene.text.as_ref().map(|t| t.sjis.len()).unwrap_or(0),
                        );
                        {
                            use std::collections::BTreeMap;
                            let evs = player.core.vm.events();
                            let mut hist: BTreeMap<String, usize> = BTreeMap::new();
                            for e in evs {
                                *hist.entry(event_kind_name(e)).or_default() += 1;
                            }
                            let top: Vec<String> =
                                hist.iter().map(|(k, v)| format!("{k}={v}")).collect();
                            plog!(
                                yuris_log::Level::Debug,
                                "[frame {n:5}] VM 状态={:?} pc={} 事件 {} 条: {}",
                                player.core.vm.state(),
                                player.core.vm.pc(),
                                evs.len(),
                                top.join(" ")
                            );
                            if n == 1 {
                                for e in evs.iter().rev().take(6).rev() {
                                    plog!(yuris_log::Level::Debug, "[frame 1]   末事件: {e:?}");
                                }
                            }
                        }
                    }
                }
                let mut render_ns = 0u128;
                if let Some(b) = player.core.backend.as_mut() {
                    let t = std::time::Instant::now();
                    let r = b.render(player.core.bridge.scene());
                    render_ns = t.elapsed().as_nanos();
                    if let Err(e) = r {
                        // 渲染失败 = 崩坏级。**不立刻退出**：错误窗与游戏画面
                        // 走同一渲染路径，下一帧若能画出来就停在错误窗；连续
                        // 3 帧仍失败才认定后端已死并退出。
                        yuris_log::report_fatal_error(module_path!(), "渲染失败", e);
                        let failures = render_failures + 1;
                        RENDER_FAILURES.store(failures, std::sync::atomic::Ordering::Relaxed);
                        if failures >= 3 {
                            event_loop.exit();
                            return;
                        }
                    } else {
                        RENDER_FAILURES.store(0, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                // ---- 截图(成果 127):逻辑画布分辨率,对拍/排错取证 ----
                let this_frame = {
                    let mut h = harness.borrow_mut();
                    h.frame_no += 1;
                    h.frame_no
                };
                if harness.borrow().shots.contains(&this_frame) {
                    let shot_dir = harness.borrow().shot_dir.clone();
                    if let Some(b) = player.core.backend.as_mut() {
                        match b.render_scene_to_rgba(player.core.bridge.scene()) {
                            Ok((rgba, w, h)) => {
                                let _ = std::fs::create_dir_all(&shot_dir);
                                let path =
                                    shot_dir.join(format!("frame_{this_frame:05}.png"));
                                match image::save_buffer(
                                    &path,
                                    &rgba,
                                    w,
                                    h,
                                    image::ColorType::Rgba8,
                                ) {
                                    Ok(()) => {
                                        plog!(
                                            yuris_log::Level::Info,
                                            "[shot] 帧 {this_frame} → {} (画布 {w}×{h})",
                                            path.display()
                                        );
                                        // 呈现面诊断(`YURIS_TRACE_SHOT=1`):列出
                                        // 本帧**可见**层(z/alpha/资源),定位
                                        // 「某帧纯黑」到底是"层全灭"还是"全被盖"。
                                        if std::env::var_os("YURIS_TRACE_SHOT").is_some() {
                                            {
                                                let sc = player.core.bridge.scene();
                                                let vis: Vec<String> = sc
                                                    .drawable_layers()
                                                    .map(|l| {
                                                        format!(
                                                            "{:x}z{}a{:.2}",
                                                            l.id & 0xFFFF_FFFF,
                                                            l.z,
                                                            l.alpha
                                                        )
                                                    })
                                                    .collect();
                                                plog!(
                                                    yuris_log::Level::Info,
                                                    "[shot-layers] f{this_frame} n={} {:?}",
                                                    vis.len(),
                                                    vis
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        plog!(yuris_log::Level::Error, "[shot] 写 PNG 失败 {path:?}: {e}")
                                    }
                                }
                            }
                            Err(e) => plog!(yuris_log::Level::Error, "[shot] 帧 {this_frame} 离屏渲染失败: {e}"),
                        }
                    }
                }
                // ---- 自动退出(--quit-after / --shot 末帧;诊断批跑bez阻塞)----
                {
                    let hit = harness.borrow().quit_after.is_some_and(|n| this_frame >= n);
                    if hit {
                        plog!(yuris_log::Level::Info, "[quit-after] 帧 {this_frame} 达上限,自动退出");
                        player.log_vm_unsupported("退出");
                        event_loop.exit();
                        return;
                    }
                }
                // ---- bench 汇总(成果 127)----
                let mut bench = harness.borrow_mut().bench.take();
                if let Some(b) = bench.as_mut() {
                    let total = frame_t.elapsed().as_nanos();
                    b.frames += 1;
                    b.total_ns += total;
                    b.vm_ns += vm_ns;
                    b.render_ns += render_ns;
                    b.worst_ns = b.worst_ns.max(total);
                    b.samples_ns.push(total as u64);
                    if total > 50_000_000 {
                        plog!(
                            yuris_log::Level::Info,
                            "[slow] 帧 {} 耗时 {:.1}ms (VM {:.1} + 渲染 {:.1})",
                            b.frames,
                            total as f64 / 1e6,
                            vm_ns as f64 / 1e6,
                            render_ns as f64 / 1e6
                        );
                    }
                    if std::time::Instant::now() >= b.until {
                        let n = b.frames.max(1);
                        plog!(
                            yuris_log::Level::Info,
                            "[bench] {} 帧 / 均值 {:.2}ms (VM {:.2} + 渲染 {:.2})  最差 {:.2}ms",
                            b.frames,
                            b.total_ns as f64 / n as f64 / 1e6,
                            b.vm_ns as f64 / n as f64 / 1e6,
                            b.render_ns as f64 / n as f64 / 1e6,
                            b.worst_ns as f64 / 1e6,
                        );
                        // **分位数 + 稳态均值**(成果 184):均值被 boot 尖峰稀释,
                        // 达标判据看这里。`budget` = 1000/60 ms ≈ 16.67ms。
                        let mut s = b.samples_ns.clone();
                        s.sort_unstable();
                        let pct = |p: f64| -> f64 {
                            if s.is_empty() {
                                return 0.0;
                            }
                            let i = ((s.len() as f64 - 1.0) * p).round() as usize;
                            s[i.min(s.len() - 1)] as f64 / 1e6
                        };
                        let budget_ns: u64 = 16_667_000;
                        let over = s.iter().filter(|x| **x > budget_ns).count();
                        let steady: Vec<u64> = s.iter().copied().skip(s.len() / 10).collect();
                        let steady_avg = if steady.is_empty() {
                            0.0
                        } else {
                            steady.iter().map(|x| *x as f64).sum::<f64>()
                                / steady.len() as f64
                                / 1e6
                        };
                        plog!(
                            yuris_log::Level::Info,
                            "[bench-pct] p50={:.2}ms p95={:.2}ms p99={:.2}ms | 稳态均值(p10起)={:.2}ms | 超预算(>16.67ms) {}/{} 帧",
                            pct(0.50),
                            pct(0.95),
                            pct(0.99),
                            steady_avg,
                            over,
                            s.len()
                        );
                        harness.borrow_mut().bench = bench;
                        player.log_vm_unsupported("bench 结束");
                        event_loop.exit();
                        return;
                    }
                }
                harness.borrow_mut().bench = bench;
                // 帧时刻由 `about_to_wait` 的 `ControlFlow::WaitUntil` 驱动,
                // 此处**不得**再 `request_redraw()`(会变成永不休眠的忙转)。
                player.last_frame = Instant::now();
                // present(Fifo)返回时 ≈ vblank —— 帧节拍锚点(成果 127)。
                player.last_present = Instant::now();
                // **点击延迟实测**(`YURIS_TRACE_CLICK=1`):按下 → 本帧 present
                // 完成的墙钟差。这是判定「点击是否及时」的唯一客观数字,
                // 不靠观感(观感受 vsync 与显示器刷新影响)。
                if std::env::var_os("YURIS_TRACE_CLICK").is_some() {
                    // 借用一次取全部字段(嵌套 borrow_mut + borrow 会 panic:
                    // "RefCell already mutably borrowed")。
                    let pending = {
                        let mut h = harness.borrow_mut();
                        h.press_at.take().map(|(t, f, b)| (t, f, b, h.frame_no))
                    };
                    if let Some((t, f, b, cur)) = pending {
                        let dt = t.elapsed().as_secs_f64() * 1000.0;
                        plog!(
                            yuris_log::Level::Info,
                            "[click-lat] {b} 按下(帧{f}) → 出图: {dt:.1}ms(本帧 {cur})"
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// 发现游戏目录内的全部可解析封包(根 + `pac/`),逐包自动判别索引方言。
///
/// **不假设**包的个数、文件名或 `name_key` —— 这些都是逐游戏的事实。
fn discover_packs(
    dir: &std::path::Path,
) -> Vec<(std::path::PathBuf, yuris_format::ypf::YpfIndex)> {
    let mut out = Vec::new();
    for root in [dir.to_path_buf(), dir.join("pac")] {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut paths: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("ypf"))
                    .unwrap_or(false)
            })
            .collect();
        paths.sort();
        for p in paths {
            match yuris_format::ypf::YpfIndex::from_path_auto(&p) {
                Ok((idx, dialect, score)) => {
                    plog!(
                        yuris_log::Level::Debug,
                        "[player]   {} → ypf v{} / {} 条 / 方言 {dialect:?} / 自洽 {:.1}%",
                        p.file_name().unwrap().to_string_lossy(),
                        idx.header.version,
                        idx.entries.len(),
                        score * 100.0
                    );
                    out.push((p, idx));
                }
                Err(e) => plog!(
                    yuris_log::Level::Warn,
                    "[player]   {} 不可解析: {e}",
                    p.file_name().unwrap().to_string_lossy()
                ),
            }
        }
    }
    out
}

/// 事件种类名(诊断日志用;从 Debug 串取首个标识符,避免逐个变体穷举)。
fn event_kind_name(e: &yuris_vm::VmEvent) -> String {
    let d = format!("{e:?}");
    d.split(|c: char| c == '(' || c == ' ' || c == '{')
        .next()
        .unwrap_or("?")
        .to_string()
}
