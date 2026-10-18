# 输入层（`yuris-input`）—— 引擎语义与移植边界

| 项 | 值 |
|---|---|
| 目标 | 引擎 v555 / v494（样本 + <game-b>）与 SDK 0.495 官方文档 |
| 证据 | 官方手册 `basic/input.html`、`basic/svariable.html`、`system/input.html`、`system/mouse.html`、`cg/cginfo.html`；SDK 0.495 反汇编 `FUN_0041a4b8`（每帧四态转移）、`FUN_00461530`（四态函数表初始化）、`FUN_00447014`（脚本写回钳制）、`FUN_0045a3b4`（`_MOUSELAST`/`_KEYLAST`）、`FUN_00403ee0`（光标逻辑坐标）、`FUN_00427350`（系统变量二次派发） |
| 状态 | 核心语义 **Confirmed**；符号键 VK 逐值 **Likely**；双击分级 / 手柄编号 **Unknown** |

## 1. 状态模型（每按钮一个 64 位帧计数器）

官方手册 `basic/input.html` 原文：按下时 `1 → 2 → 3 …`，抬起时 `-1 → -2 → -3 …`，
每帧 ±1；并明确说明这是 64 位值（约 49 亿年才溢出），因此实现用
`i64::wrapping_add/sub` 即可。

引擎每帧按 `(上一帧按下, 本帧按下)` 四态转移（`FUN_0041a4b8` 的循环
调用 `0x801e84` 函数表；表项由 `FUN_00461530` 初始化）：

| prev | down | 动作 | 表项地址 |
|---|---|---|---|
| 0 | 0 | `value -= 1` | `0x45a118` |
| 0 | 1 | `value = +1` | `0x45a168` |
| 1 | 0 | `value = -1` | `0x45a148` |
| 1 | 1 | `value += 1` | `0x45a130` |

**按下次数**（`_KEYNUM_*` / `_MOUSENUM_*`）只在 `0 → 1` 边沿 +1。

## 2. 变量面（SDK 0.495 的 YSCM 系统变量序号）

| id | 名字 | 说明 |
|---|---|---|
| 133 / 138 | `_MOUSE_X` / `_MOUSE_Y` | 主窗口光标逻辑坐标；134..137 / 139..142 是另外三个窗口对象（本内核只有主窗口 → 0，引擎对空 HWND 同样写 0） |
| 143 | `_PAD_NUMBER` | 已连接手柄数（位计数） |
| 144 | `_INP` | 数组：下标 = 按钮码（VK），`@_INP(@_CODE_Z)` 的用法见手册 `basic/svariable.html` 注 3 |
| 145 / 146 | `_KEYLAST` / `_MOUSELAST` | 最近一次按下的键码 / 鼠标码；脚本清 0 可取消 skip（ERIS `es_Text` 实证） |
| 147..166 | `_MOUSE_L/L1..L3`、`_R*`、`_M*`、`_U*`、`_D*` | 鼠标按键状态（分级语义 Unknown → 同基础按钮） |
| 167..274 | `_KEY_A…_KEY_BACKSLASH` | 键盘状态 |
| 275 | `_KEY_ANY` | 任意键 |
| 276..280 | `_PAD_UP/DOWN/LEFT/RIGHT`、`_PAD_BUTTON` | 手柄（按手柄号索引；按钮二维 `(n,m)`） |
| 281..409 | `_MOUSENUM_*` / `_KEYNUM_*` | 按下次数 |
| 410..414 | `_PADNUM_*` | 手柄按下次数 |
| 415..547 | `_CODE_*` | 按钮码常量（手册 ○ = 可赋值，用于键位设置） |

`ysc.ybn`（YSCM）与此表**同序**：命令参数槽按名解析，系统变量 id 直接
对应上表（逐版本会漂移：0.479 的 `_MOUSE_X` = 111，0.495 = 133）。

### 版本面（`InputSnapshot::claimed`）

系统变量 id 表**不能跨版本无条件套用**：样本 v555 里 `@409` 是游戏自己的
字符串变量（`@409 = $55[1]`，PROGRESS 成果 51 的按钮链），而 0.495 表里
409 是 `_KEYNUM_ANY`。若按 0.495 区间硬判，会把这个游戏全局变量当输入
状态劫持掉。

因此 VM 用**显式声明集合**（`claimed`）：

- 494 / 495 → `(133..=547)`（按 SDK `YSCom.ycd` 序列，Likely）；
- 其它版本（含 v555）→ 只声明 `{133, 138}`（光标，样本 Confirmed）。

未声明的 id 一律走普通变量存储，行为与改动前一致。要有完整键盘/鼠标
状态，需要该版本自己的系统变量表 —— 与 `CommandProfile` 同理（应由
启动链从游戏数据构建，而不是复用别的版本的表）。

## 3. 写回钳制（`FUN_00447014`，Confirmed）

脚本可以写输入状态变量（手册 ○），但引擎按**当前物理状态**钳制：

```text
prev_down == 0: value >= 0 → 写 0；value < 0 → 写 value
prev_down != 0: value >  0 → 写 value；否则写 0
```

这正是 ERIS 按钮互斥的实现方式：

```text
IF[@_MOUSE_L==1]
    IF[@BT.TH(@no)==0] @_MOUSE_L=2 IFEND[]   // 只让一个按钮响应
```

`yuris-input` 的 `write_sysvar` / `write_inp` 与 VM 的 `write_input_var`
都按此规则（VM 侧在写入时立即钳制，保证同一帧后续读取看到新值）。

## 4. `INPUT` / `MOUSE` 命令

手册 `system/input.html`：`INPUT[KEY/MOUSE/PAD]`（0/1）启用或禁用输入源。
手册 `system/mouse.html`：`MOUSE[MODE/TIME/X/Y/MOVE/CLICK/WHEEL/CURSOR]`。

- 参数槽按命令自身 YSCM 参数名解析（v555 的 `INPUT` 槽 0=KEY、1=PAD、2=MOUSE；
  `MOUSE` 槽 0=MODE、1=TIME、3=CLICK……）。
- 禁用某源后该源的事件被丢弃且状态机冻结（引擎跳过整段更新）。
- `MOUSE[MODE=1 TIME=…]` 的自动隐藏、光标形状编号属于平台表现，当前只记录设置。

## 5. 光标坐标反算

引擎 `FUN_00403ee0` 用**逐轴拉伸**（`x * 逻辑宽 / 客户区宽`），因为原引擎
用 `StretchDIBits` 把后缓冲铺满窗口。本内核渲染是 letterbox（等比 + 居中），
因此输入反算必须是 `letterbox_matrix` 的逆映射：

```text
scale = min(win_w / logical_w, win_h / logical_h)
logical = (pixel - offset) * 逻辑边长 / 受限窗口边长   // 有理式，避免 1px 误差
```

窗口比例 == 逻辑比例时两者完全一致（原版默认窗口即如此）。

## 6. `CGINFO ONMOUSE` 命中

手册 `cg/cginfo.html`：命中返回该点通道值，**未命中返回 -1**。
引擎 `FUN_0045aa44` 读窗口遮罩位图的 24 位像素，`> 0` 才算命中 ——
这就是“按钮只有非透明像素可点”的底层语义。

本内核当前只有 CG 注册表（几何，没有纹理像素读回）：
- 命中判定 = 矩形（`SX/SY` 显式尺寸优先，否则图像原始尺寸）；
- 命中返回 **255**（“不透明哨兵”，语料按 `>= BT.MAP.A` 判定，阈值缺省 0）；
- 未命中返回 **-1**（手册明确值）；
- 像素级遮罩待渲染侧提供读回后替换（标注 UNVERIFIED）。

## 7. 移植边界

- 核心模块**零平台依赖**，`cargo check -p yuris-input --target wasm32-unknown-unknown` 通过。
- 桌面：`feature = "winit"` 的 `winit_adapter` 只做 `WindowEvent → PlatformEvent` 翻译。
- Android / wasm：壳把触摸送进 `TouchAdapter`（单指=左键、双指轻点=右键、
  双指上下滑=滚轮）；手柄走 `PlatformEvent::PadButton/PadAxis`。
- 键位设置：`_CODE_*` 可写（`write_sysvar` 记录覆盖表），`_INP(code)` 的
  code 来自脚本变量，天然支持重映射。

## 8. 验证

```bash
# 核心语义单测（四态转移 / 写回钳制 / 计数器 / 光标反算 / 触摸）
cargo test -p yuris-input
# winit 翻译
cargo test -p yuris-input --features winit
# VM 接线（真实编码的 LET/IF + INPUT/MOUSE 命令）
cargo test -p yuris-vm --test p9_input
# wasm 可编译
cargo check -p yuris-input --target wasm32-unknown-unknown
```
