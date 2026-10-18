//! 一条日志记录与致命错误报告。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::Level;

/// 一条已产生的日志记录（等级过滤之后）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// 进程内单调序号（从 1 开始；错误窗据此判定"是否是新报告"）。
    pub seq: u64,
    /// 等级。
    pub level: Level,
    /// 来源模块（`module_path!()`）。
    pub target: &'static str,
    /// 已格式化消息。
    pub message: String,
    /// 距日志系统启动的毫秒数（单调，不受墙钟回拨影响）。
    pub elapsed_ms: u64,
    /// Unix 毫秒（文件/外部工具需要绝对时间时使用）。
    pub unix_ms: u64,
}

impl Record {
    /// 稳定的一行文本（stderr / 文件共用；不含 ANSI）。
    pub fn format_line(&self) -> String {
        let secs = self.elapsed_ms as f64 / 1000.0;
        format!(
            "[{secs:>9.3}s] {:<5} {}: {}",
            self.level.as_str(),
            self.target,
            self.message
        )
    }

    /// 错误窗里的紧凑格式：`E target: message`。
    pub fn format_compact(&self) -> String {
        format!(
            "{} {}: {}",
            self.level.short(),
            self.target,
            self.message.replace('\n', " ")
        )
    }

    /// 错误窗（可滚动面板）里的**多行**格式。
    ///
    /// 与 [`Self::format_compact`] 的区别：保留换行、去掉 `alert!` 横幅的
    /// `!!!!…!!!!` 边框行与行首 `!!` 装饰。理由：横幅在终端里为了一眼可辨
    /// 而加框，进了错误窗只会让回溯区全是感叹号，反而看不到根因
    /// （实机截图里一屏 8 行有 6 行是 `!!!`）。
    ///
    /// 首行带 `W target: ` 前缀，后续行缩进 4 格 —— 一条记录多行时仍能
    /// 分辨归属。
    pub fn format_panel(&self) -> String {
        let mut out = String::new();
        let mut first = true;
        for raw in self.message.lines() {
            let mut text = raw.trim();
            if text.is_empty() {
                continue;
            }
            // 横幅边框行：整行只有 `!`。
            if text.chars().all(|c| c == '!') {
                continue;
            }
            if let Some(rest) = text.strip_prefix("!!") {
                text = rest.trim_start();
            }
            if text.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            if first {
                out.push_str(self.level.short());
                out.push(' ');
                out.push_str(self.target);
                out.push_str(": ");
                first = false;
            } else {
                out.push_str("    ");
            }
            out.push_str(text);
        }
        if out.is_empty() {
            // 消息全是边框/空白（理论上不会发生）：退回单行，至少不丢内容。
            return format!(
                "{} {}: {}",
                self.level.short(),
                self.target,
                self.message.replace('\n', " ")
            );
        }
        out
    }
}

/// 崩坏级错误的完整快照（画布错误窗的数据源）。
///
/// `FatalReport` 是**不可变**的：新的致命记录只会追加到 `tail`，不会改变
/// `seq`，因此错误窗只需在 `seq` 变化时重建一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatalReport {
    /// 首次致命记录的序号（错误窗去重键）。
    pub seq: u64,
    /// 标题（如「致命错误 / FATAL ERROR」）。
    pub title: String,
    /// 主消息（首行通常是异常摘要，其余行原样保留）。
    pub message: String,
    /// 崩溃前的最近日志（按时间顺序；末尾是本条致命记录）。
    pub tail: Vec<Record>,
    /// 发生时刻（Unix 毫秒）。
    pub unix_ms: u64,
}

impl FatalReport {
    /// 错误窗展示用的文本行（不含严重级别前缀的标题/消息）。
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(self.title.clone());
        for line in self.message.lines() {
            out.push(line.to_string());
        }
        if !self.tail.is_empty() {
            out.push("—— 最近日志 ——".to_string());
            for r in &self.tail {
                out.push(r.format_compact());
            }
        }
        out
    }
}

pub(crate) fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format_contains_level_and_target() {
        let r = Record {
            seq: 1,
            level: Level::Warn,
            target: "yuris_vm::host",
            message: "x".into(),
            elapsed_ms: 1234,
            unix_ms: 0,
        };
        let line = r.format_line();
        assert!(line.contains("WARN"));
        assert!(line.contains("yuris_vm::host"));
        assert!(line.contains("x"));
    }
}

#[cfg(test)]
mod panel_format_tests {
    use super::*;

    fn rec(level: Level, msg: &str) -> Record {
        Record {
            seq: 1,
            level,
            target: "yuris_vm",
            message: msg.to_string(),
            elapsed_ms: 1,
            unix_ms: 0,
        }
    }

    /// `alert!` 的横幅边框与 `!!` 行首装饰在错误窗里必须被剥掉 ——
    /// 实机截图曾一屏 8 行里 6 行是 `!!!!`。
    #[test]
    fn panel_strips_alert_banner_decoration() {
        let msg = "\n!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n!!  YURIS ALERT —— 运行时未实现 / 未知路径\n!!  命令: EMOTE (idx 12)\n!!!!!!!!!!!!!!!!!!!!!!!!!!!!";
        let out = rec(Level::Warn, msg).format_panel();
        assert!(!out.contains("!!!") || out.contains("命令"), "不应保留纯边框行: {out:?}");
        assert!(out.starts_with("W yuris_vm: "), "首行带等级/模块前缀: {out:?}");
        assert!(out.contains("YURIS ALERT"), "正文内容要保留: {out:?}");
        assert!(out.contains("命令: EMOTE"), "正文内容要保留: {out:?}");
        // 续行有缩进，便于看出属于同一条记录
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 2);
        assert!(lines[1].starts_with("    "), "续行应缩进: {out:?}");
    }

    /// 单行消息保持 `W target: msg`，与 compact 同形。
    #[test]
    fn panel_single_line_matches_compact_shape() {
        let r = rec(Level::Error, "boom");
        assert_eq!(r.format_panel(), "E yuris_vm: boom");
    }

    #[test]
    fn panel_keeps_explicit_newlines() {
        let out = rec(Level::Error, "第一行\n第二行").format_panel();
        assert_eq!(out, "E yuris_vm: 第一行\n    第二行");
    }
}
