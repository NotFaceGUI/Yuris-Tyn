# 日志与致命错误层（`yuris-log`）

> 成果 163（2026-10-17）。本文件是 `yuris-log` 的规格：等级语义、输出端、
> 致命错误契约与各 crate 的接入约定。

## 1. 为什么独立成包

改造前，诊断输出有四套并行体系：

| 体系 | 位置 | 问题 |
|---|---|---|
| `eprintln!` 直打 | 全仓库约 130 处（player-core 62、vm 30） | 等级全靠 `[前缀]` 猜；无法统一关闭/过滤 |
| `tracing` 宏 | audio/render/resource 共 13 处 | **没有安装 subscriber**，实际无输出（等于死代码） |
| CLI 独立日志终端 | `yuris-cli/src/logsink.rs` | 依赖 `wt` 子进程，等级与主日志各管一套 |
| 环境变量门控 | `YURIS_TRACE_*` / `YURIS_LOG_ERR` 等 | 开关散落，语义不统一 |

`yuris-log` 把这些收敛为**一个入口 + 一套等级 + 一个错误汇聚点**：

- 任何 crate 只需依赖 `yuris-log`（无第三方依赖），调宏即可；
- 等级由 `YURIS_LOG_LEVEL` 一处控制；
- `Result::Err` 用 `log_err!` / `fatal_err!` 消费，不再手工拼 `eprintln!("[x] {e}")`；
- 崩坏级错误写入 `FatalReport`，播放器壳在**画布中央**渲染错误窗。

## 2. 等级

| 等级 | 数值 | 用途 | 缺省 |
|---|---|---|---|
| `Trace` | 0 | 逐指令/逐事件取证（原 `YURIS_TRACE_*` 的细粒度输出） | 关 |
| `Debug` | 1 | 诊断细节、状态重放摘要 | 关 |
| `Info` | 2 | 生命周期、状态变化、存档读写 | **开** |
| `Warn` | 3 | 可恢复异常：资源缺失、未实装命令、索引自洽率低 | 开 |
| `Error` | 4 | 功能失败但可继续 | 开 |
| `Fatal` | 5 | **崩坏级**：错误窗上屏、主循环停止推进 | 开 |
| `Off` | 6 | 阈值专用（不产生记录） | — |

过滤语义：`record.level >= threshold` 才输出。阈值 `Warn` 放行 Warn/Error/Fatal。

`Fatal` **旁路阈值**——即使 `YURIS_LOG_LEVEL=off`，致命记录仍会进环形缓存、
输出端与 `FatalReport`（错误窗需要现场，不能因为调了日志等级就变成哑崩）。

## 3. 环境变量

| 变量 | 含义 | 缺省 |
|---|---|---|
| `YURIS_LOG_LEVEL` | `trace/debug/info/warn/error/fatal/off`（接受缩写） | `info` |
| `YURIS_LOG_STDERR` | `0` 关闭 stderr 输出 | 开 |
| `YURIS_LOG_FILE` | 追加写入的日志文件路径 | 不写 |
| `YURIS_LOG_COLOR` | `0/1` 强制 ANSI 颜色 | 自动（`WT_SESSION`/`TERM`） |
| `YURIS_LOG_RING` | 环形缓存条数 | 512 |

## 4. 输出端（`Sink`）

| 实现 | 用途 |
|---|---|
| `StderrSink` | 进程 stderr；`color_auto()` 决定是否带 ANSI（老 conhost 不着色） |
| `FileSink` | 追加写文件（跨平台；崩溃后仍可复盘） |
| `MemorySink` | 内存收集（单测/工具/取证） |

自定义输出端实现 `Sink`（`emit` + 可选 `flush`），交给
`Logger::with_sinks` 或 `Config` 装配。**输出端实现必须 `Send + Sync`**：
音频线程与渲染线程都会写日志。

## 5. 致命错误契约

```rust
// 任意 crate：
yuris_log::fatal!("无法初始化图形后端: {e}");
yuris_log::report_fatal_error(module_path!(), "渲染失败", e);

// 播放器壳（每帧）：
if core.update_fatal_overlay() {
    return; // 停止推进游戏逻辑
}
```

- `FatalReport` 不可变：同一进程的**后续致命错误只追加 tail**，`seq`/标题/主消息
  保留首因（避免连环崩坏把根因盖掉）。
- 回调：`yuris_log::on_fatal(f)` 注册，可在任意线程触发；用于唤醒主循环。
- 状态查询：`has_fatal()` / `fatal_report()`；恢复时 `clear_fatal()`。

### 错误窗

- 数据源 = `FatalReport`；排版 = `yuris_player_core::overlay::build_fatal_panel`。
- 输出 = **画布尺寸 RGBA**，作为 `SC_FATAL`（`z = i64::MAX`）图层提交。
  因此它与游戏画面共用 letterbox 变换，不会在窗口缩放时跑位。
- 内容 = 标题栏 / 主消息（自动换行，超长截断并提示 `YURIS_LOG_FILE`）/
  最近日志（优先 warn 及以上）/ Esc 提示。
- 输入 = 错误窗在场时游戏输入全部屏蔽；Esc 关闭错误窗并退出。
- 验证示例：`cargo run -p yuris-player-core --example fatal_panel -- out.png`。

## 6. 接入约定（各 crate）

1. **只依赖 `yuris-log`**，不要再引入 `tracing`/`log`（已确认全仓库 0 处调用后移除依赖）。
   需要 subscriber 生态的独立工具可以自行桥接，但核心包不引入。
2. **宏优先**：`yuris_log::{trace,debug,info,warn,error,fatal}!("…{x}")`。
   宏先判阈值再 `format!`，热路径（逐指令 trace）无额外分配。
3. **错误消费**：`log_err!(Level::Warn, "解码失败", result)`；
   不可继续的错误用 `report_fatal_error(module_path!(), ctx, e)`。
4. **等级映射**（既有代码的既定口径）：
   - 资源未命中 / 未实装命令 / 索引自洽率低 → `Warn`
   - 上传/解码/写盘失败但已降级 → `Warn`；功能直接失败 → `Error`
   - `YURIS_TRACE_*` 门控的取证输出 → `Trace`（保留原有环境变量门控）
   - 生命周期/存档成功 → `Info`
5. **不要**在库代码里 `println!`；CLI 的 stdout 只给工具型子命令
   （`yuris-tools` 仍用 stdout 打印表格，那是命令输出而非日志）。

## 7. 验证

```bash
export TEMP="<side-repo>/workbench/tmp" TMP="$TEMP"
cargo test -p yuris-log            # 9 单测 + 1 doctest
cargo test --workspace --no-fail-fast
YURIS_LOG_LEVEL=debug cargo run -p yuris-player-core --example fatal_panel -- shot/fatal_panel.png
```

`grep -rn "tracing::" crates --include=*.rs` 应为 0 命中；
`grep -rn "eprintln!" crates/*/src` 只应剩 `yuris-log` 自身的启动失败兜底
（`Logger::new` 打不开日志文件时的唯一兜底；另有 `yuris-core/src/commands.rs`
单测里的 3 处「文档缺失，skip」提示 —— 属测试诊断，不是运行日志）。
