//! 生成一张「崩坏级错误窗」示例 PNG（排版取证/回归对照）。
//!
//! 用法：
//! `cargo run -p yuris-player-core --example fatal_panel -- [输出路径] [scale] [scroll] [extra_tail]`
//!
//! - 输出路径：缺省 `shot/fatal_panel.png`（逻辑画布 1100×825，与样本游戏一致）；
//! - `scale`：渲染缩放 = 物理像素 / 逻辑像素（缺省 1.0；2.0 验证高 DPI 清晰度）；
//! - `scroll`：滚动量（物理像素；配合 `extra_tail` 验证滚动视图与滚动条）；
//! - `extra_tail`：追加的合成回溯条数（缺省 0；用于把内容撑出视口）。
//!
//! 本示例不依赖窗口/wgpu —— 直接调用与运行期相同的排版函数。

use yuris_log::{FatalReport, Level, Record};
use yuris_player_core::load_cjk_font;
use yuris_player_core::overlay::build_fatal_panel;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "shot/fatal_panel.png".to_string());
    let scale: f32 = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let scroll: i32 = std::env::args()
        .nth(3)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let extra_tail: usize = std::env::args()
        .nth(4)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let bytes = load_cjk_font().expect("系统 CJK 字体");
    let font = fontdue::Font::from_bytes(
        bytes,
        fontdue::FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: true,
        },
    )
    .expect("字体解析");

    let mut report = FatalReport {
        seq: 42,
        title: "致命错误 / FATAL ERROR".into(),
        message: "wgpu 初始化失败: request adapter: 无兼容适配器\n\
                  图形后端不可用，游戏无法继续。"
            .into(),
        tail: vec![
            Record {
                seq: 38,
                level: Level::Warn,
                target: "yuris_audio::engine",
                message: "音频设备不可用,降级无头播放: no default device".into(),
                elapsed_ms: 120,
                unix_ms: 0,
            },
            Record {
                seq: 39,
                level: Level::Info,
                target: "yuris_cli",
                message: "[player] 封包 5 个".into(),
                elapsed_ms: 180,
                unix_ms: 0,
            },
            Record {
                seq: 40,
                level: Level::Error,
                target: "yuris_render",
                message: "create surface: unsupported adapter".into(),
                elapsed_ms: 260,
                unix_ms: 0,
            },
            Record {
                seq: 42,
                level: Level::Fatal,
                target: "yuris_cli",
                message: "wgpu 初始化失败: request adapter: 无兼容适配器".into(),
                elapsed_ms: 300,
                unix_ms: 0,
            },
        ],
        unix_ms: 0,
    };
    // 合成回溯：验证「内容超出视口 → 可滚动 + 滚动条」。
    for i in 0..extra_tail {
        report.tail.push(Record {
            seq: 100 + i as u64,
            level: if i % 3 == 0 { Level::Warn } else { Level::Info },
            target: "yuris_vm::exec",
            message: format!("合成回溯 #{i}: 数组声明上界含未建模算子,该声明已跳过"),
            elapsed_ms: 400 + (i as u64) * 10,
            unix_ms: 0,
        });
    }
    // 把 FatalReport 移进函数参数位置（`tail` 已补齐）。
    let report = report;

    let logical = (1100u32, 825u32);
    let width = ((logical.0 as f32) * scale).round() as u32;
    let height = ((logical.1 as f32) * scale).round() as u32;
    let panel = build_fatal_panel(&font, &report, width, height, scale, scroll);
    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(
        &out,
        &panel.rgba,
        panel.width,
        panel.height,
        image::ColorType::Rgba8,
    )
    .expect("写 PNG");
    println!(
        "{out} ({}×{}, scale {scale}, max_scroll {})",
        panel.width, panel.height, panel.max_scroll
    );
}
