//! 日志输出端。

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use crate::Record;

/// 日志输出端。实现必须可跨线程共享（日志可能来自音频/渲染线程）。
pub trait Sink: Send + Sync {
    /// 接收一条已过滤的记录。
    fn emit(&self, record: &Record);
    /// 冲刷缓冲（进程退出前由 [`Logger::flush`](crate::Logger::flush) 调用）。
    fn flush(&self) {}
}

/// 写 stderr 的输出端（可选 ANSI 颜色）。
pub struct StderrSink {
    color: bool,
}

impl StderrSink {
    /// 新建（`color = false` 时不输出 ANSI 序列）。
    pub fn new(color: bool) -> Self {
        Self { color }
    }

    /// 按环境自动判断颜色支持（Windows 仅在 Windows Terminal / 类 Unix
    /// 终端下启用，避免老 conhost 打印 `←[36m` 乱码）。
    pub fn auto() -> Self {
        Self::new(color_auto())
    }
}

impl Sink for StderrSink {
    fn emit(&self, record: &Record) {
        let mut err = std::io::stderr().lock();
        if self.color {
            // **逐行着色**（勘误 2026-10-18）：旧实现只在整条记录开头写一次
            // SGR，隐含假设终端会把属性保持到 `\x1b[0m`。`alert!` 的多行
            // 横幅证明该假设不成立 —— Windows Terminal / 部分日志渲染器
            // 在换行处重置属性，实测表现为「`WARN yuris_vm: ` 是黄的，
            // 横幅本体全是默认白」。逐行加前缀 + 逐行复位，与行数无关地稳定。
            let ansi = record.level.ansi();
            for line in colored_lines(ansi, &record.format_line()) {
                let _ = writeln!(err, "{line}");
            }
        } else {
            let _ = writeln!(err, "{}", record.format_line());
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

/// 把一段文本格式化成**每行自带 SGR 前缀与复位**的若干行。
///
/// 纯函数（便于单测）。返回值**不带行尾换行**：调用方逐行 `writeln!`，
/// 由它负责换行 —— 否则会与 `format_line()` 自带的换行叠加成空行
/// （实机现象：日志每行之间空一行）。
fn colored_lines(ansi: &str, text: &str) -> Vec<String> {
    if ansi.is_empty() {
        return vec![text.to_string()];
    }
    // 用 `split` 而不是 `lines()`：后者会丢掉**尾随空行**，而 `alert!`
    // 正是靠那个尾随换行在两条告警之间留出分隔空行。
    text.split('\n')
        .map(|body| {
            if body.is_empty() {
                // 空行不包颜色（避免产生只有转义的行）。
                return String::new();
            }
            let mut out = String::with_capacity(body.len() + ansi.len() + 6);
            out.push_str(ansi);
            out.push_str(body);
            out.push_str("\x1b[0m");
            out
        })
        .collect()
}

/// 追加写文件的输出端（无颜色；`YURIS_LOG_FILE` 指定路径）。
pub struct FileSink {
    file: Mutex<std::fs::File>,
}

impl FileSink {
    /// 打开/创建日志文件（追加模式）。
    pub fn create(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl Sink for FileSink {
    fn emit(&self, record: &Record) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{}", record.format_line());
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

/// 内存输出端（测试 / 工具 / 错误窗取数）。
#[derive(Default)]
pub struct MemorySink {
    records: Mutex<Vec<Record>>,
}

impl MemorySink {
    /// 新建空输出端。
    pub fn new() -> Self {
        Self::default()
    }

    /// 复制当前全部记录。
    pub fn records(&self) -> Vec<Record> {
        self.records.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// 清空。
    pub fn clear(&self) {
        if let Ok(mut v) = self.records.lock() {
            v.clear();
        }
    }
}

impl Sink for MemorySink {
    fn emit(&self, record: &Record) {
        if let Ok(mut v) = self.records.lock() {
            v.push(record.clone());
        }
    }
}

/// 是否应输出 ANSI 颜色。
///
/// 优先级：`YURIS_LOG_COLOR` > `NO_COLOR` > `CLICOLOR` / `CLICOLOR_FORCE` > 缺省。
///
/// **勘误（2026-10-18）**：旧实现在 Windows 上要求 `WT_SESSION` 或 `TERM`
/// 才启用颜色。实机反馈「控制台日志全是白的」—— 由宿主/IDE 直接拉起、
/// 或 stderr 经管道转发时这两个变量都不存在，于是颜色被**静默关掉**。
/// 现代 Windows 终端（Windows Terminal / VS Code / conhost + VT）都支持
/// ANSI，故缺省改为**开启**；不支持的环境用 `YURIS_LOG_COLOR=0` 关闭。
pub fn color_auto() -> bool {
    if let Ok(v) = std::env::var("YURIS_LOG_COLOR") {
        return parse_bool(&v);
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if let Ok(v) = std::env::var("CLICOLOR") {
        if v.trim() == "0" {
            return false;
        }
    }
    if let Ok(v) = std::env::var("CLICOLOR_FORCE") {
        if parse_bool(&v) {
            return true;
        }
    }
    true
}

pub(crate) fn parse_bool(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "y"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 多行消息必须**每行**带 SGR 前缀与复位（实机 alert 横幅曾只有首行是色）。
    #[test]
    fn colored_lines_prefixes_every_line() {
        let text = "[1.0s] WARN  m: 标题\n!! a\n!! b";
        let out = colored_lines("\x1b[33m", text);
        assert_eq!(out.len(), 3);
        for line in &out {
            assert!(line.starts_with("\x1b[33m"), "每行都要有前缀: {line:?}");
            assert!(line.contains("\x1b[0m"), "每行都要复位: {line:?}");
        }
        // 行内容不被改写
        assert!(out[1].contains("!! a"));
        // 返回值不带行尾换行(换行由 writeln! 负责,否则日志里会多出空行)
        for line in &out {
            assert!(!line.contains('\n'), "行内不得含换行: {line:?}");
            assert!(line.ends_with("\x1b[0m"), "每行以复位收尾: {line:?}");
        }
        // 空白行不产生只含转义的行
        let blank = colored_lines("\x1b[33m", "a\n\nb");
        assert_eq!(blank.len(), 3);
        assert_eq!(blank[1], "");
        // 尾随换行必须保留成一个空行（连续告警之间的分隔）
        let trailing = colored_lines("\x1b[33m", "bar\n");
        assert_eq!(trailing.len(), 2);
        assert_eq!(trailing[1], "");
    }

    #[test]
    fn no_color_is_passthrough() {
        assert_eq!(colored_lines("", "a\nb"), vec!["a\nb".to_string()]);
    }
}
