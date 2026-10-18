# 01 · 渲染管线总览（帧循环 / 对象表 / 图层遍历）

> 目标 `yu-ris.exe` sha256 `4f5eefa3…`。除注明外均为 **Confirmed**（反编译实测）。
> 反编译引用 = `<side-repo>/workbench/analysis/decomp/<地址>_FUN_<地址>.c`（下称 `decomp/`）。

## 1. 技术形态定性

SDK 0.495 的渲染层是 **Direct3D 9 Immediate Mode** 的 2D 合成器：

- 顶点由 CPU 逐四边形生成（16.16 定点中间计算，见 §4），`DrawPrimitiveUP(D3DPT_TRIANGLESTRIP, PrimitiveCount=2, pVertexStreamZeroData, VertexStreamZeroStride=0x54)` 直绘；
  错误串 `DrawPrimitiveUP E2.`（0x80ba38 附近区域）。
- 混合与纹理调制主要由**固定管线**（`SetRenderState` + `SetTextureStageState`）完成（见 `03-blend-modes.md`）；
  另有一条 **ID3DXEffect（HLSL）可编程通道**用于过渡/着色器路径（见 `04-alpha-transition-offscreen.md`）。
- vtable 偏移全部与 D3D9 标准 COM 表吻合（本表按 `偏移 = 序号×4`，序号含 IUnknown 3 项）：
  `+0x44` Present(17)、`+0x94` SetRenderTarget(37)、`+0xa4` BeginScene(41)、`+0xa8` EndScene(42)、
  `+0xac` Clear(43)、`+0xe4` SetRenderState(57)、`+0x104` SetTexture(65)、`+0x10c` SetTextureStageState(67)、
  `+0x114` SetSamplerState(69)、`+0x14c` DrawPrimitiveUP(83)、`+0x80` GetRenderTargetData(32)、
  `+0x34` surface LockRect(13)、`+0x38` UnlockRect(14)。
  ⇒ 渲染状态数值可直接按 D3D9 头文件枚举解读（`SRCBLEND=19`、`DESTBLEND=20`、`TEXTUREFACTOR=60`、`BLENDOP=171`，
  与 `旁线仓库/docs/render-semantics.md` §2 的"BLENDOP=171"互证）。

> 勘误留痕：`render-semantics.md` §2b 曾把 `push 0x1a`(26) 解读为 `D3DRS_STENCILFUNC`；
> 按 D3D9 头文件 26 = `D3DRS_DITHERENABLE`。该解读属于那一条线的笔误，**不影响其"0x462e40=离屏纹理创建器"的结论**（该结论本次复核成立，见 `04`）。

## 2. 全局对象表（.data 槽位）

| 全局 | 类型 | 证据 |
|---|---|---|
| `DAT_00801d5c` | `IDirect3DDevice9*`（`YS_D3DDevice`） | 绘制/帧入口处处判空 + 错误串 |
| `DAT_00801e7c` | `ID3DXEffect*`（`g_lpEffect`，HLSL） | Begin/BeginPass/CommitChanges/End vtable 序列 + `HLSL g_lpEffect Error.` |
| `DAT_00801dac` | 背板 surface（Present 用） | `YS_BACKBUFFER Error.` 判空 |
| `DAT_00801db4` | `YS_OffSuf_LAYER` 离屏表面（捕获用） | `FUN_00417384` 判空 + LockRect/UnlockRect |
| `DAT_00801d98` / `DAT_00801d9c` | 两张引擎自备辅助纹理 | `MODE=3`/`MODE=5` 分支里 `SetTexture(1, …)` 引用（用途 Unknown，见 03 §3） |
| `DAT_00801750` | 调试模式开关 | 图层遍历/绘制中的 `!= 0` 分支（配合 `层+0xa4==2` 调试层） |
| `DAT_005b4804` / `DAT_005b4808` / `DAT_0080131c` | 纹理过滤配置：MAG / MIN / MIP | 帧入口三段 `SetSamplerState`（值域 0=POINT/1=LINEAR/2=ANISOTROPIC；MIP 另有 -1=NONE）——即 `エンジン設定.exe`（引擎设定工具）写出的配置项 |

## 3. 帧循环（每帧 D3D 调用序列）

帧入口 `FUN_0043cd6c`（`decomp/0043cd6c`；无 C 级调用者 ⇒ 由主循环经函数指针调用）：

```
1. 判空 DAT_00801d5c / DAT_00801dac（"YS_D3DDevice Error." / "YS_BACKBUFFER Error."）
2. 背板 GetSurfaceDesc 类取参 → SetRenderTarget(0, 背板)      // 失败 "GETSS SetRenderTarget Error 1."
3. FUN_00413b58()                        // 状态复位类（细节未逐行核）
4. BeginScene()                          // 失败 "D3D BeginScene Error."
5. g_lpEffect->Begin(0, 0)               // 失败 "HLSL Begin Error."
6. SetSamplerState(stage 0) + SetSamplerState(stage 1)   // MAG/MIN/MIP 三项 × 两个采样器,值来自 §2 配置全局
7. FUN_00414ba4()                        // ★ 图层树整体绘制（02/03/04）
8. FUN_0043d308(): FUN_00417360() + EndScene()           // 失败 "D3D EndScene Error."
9. GetRenderTargetData 捕获路径:按 分配 `W*4 + W*4*H` 缓冲并把结果交给调用方字段   // 失败 "GETSS SetRenderTarget Error 1."
```

Present 封装 = `FUN_0041a420`（错误串 `D3D Present Error.` @0x80b964）。
Clear 封装见遍历器调试分支（`D3D Clear Error.` @0x80b6a4）。

> 实现含义（对齐要求）：本仓库 wgpu 后端不必复刻 Immediate-Mode 的**调用序列**，
> 但必须复刻其**每帧可见语义**：整帧 = 背板 Clear（由 D3D 隐含的 Present 语义 + 显式 Clear 点）→
> 按图层树顺序逐层 TRIANGLESTRIP 合成 → 可选的捕获/读回。任何"跳过图层树直接整屏后处理"的捷径都不等价。

## 4. 图层遍历器（绘制顺序与图层组）

`FUN_00413dc8`（`decomp/00413dc8`）——**递归链表遍历**，这就是绘制顺序的权威：

```
walk(nodes, count):
  n = 第 count 个节点(FUN_004046dc: 沿 node+0xc 走链)
  loop:
    if n == 0: return
    if node+0x30 != 0: walk(子树)          // ★ 图层组:先递归子层
    layer = node+0x34
    skip 若: layer+0xa4 == 0(E=无效) / layer+0x89 == 0 / layer+0x90 == 0(无纹理)
    if 调试模式 && layer+0xa4 == 2:         // 调试层:切到层自身 RT(+0x61c) 单独画
        SetRenderTarget(layer+0x61c…) → Clear(黑) → 专用绘制
        (进度公式: 0.5 × (i64)[layer+0x1e8] / 100.0;常量 0x58eba0=0.5、0x58eb98=100.0)
    if layer+0x4c / layer+0x210 标志: FUN_004141e4 + FUN_00414559   // 帧推进类(动画/耦合状态;细节未逐行核)
    FUN_00414ba4(layer…)                    // ★ 画本层
    n = node+0xc
```

- **绘制顺序 = 链表顺序**（`Z` 排序发生在链表维护侧，不在遍历侧）。
- **图层组**的"组 A 作用于子层合成后的结果"由此获得二进制支撑：子层先画进组的离屏目标，
  组自身持合成纹理（`+0x90`）再作为普通层绘制（离屏目标的创建/捕获见 `04` §4）。
- 跳过条件即手册 `E`(有効/無効) 的消费点之一：`+0xa4==0` ⇒ 连绘制函数都不进。
  ⚠ 本仓库当前把 `E` 落在 `Cg` 事件外层（成果 121 只做了 `CG` 的 `E` 槽读取），**遍历级"不进绘制函数"的语义**
  要对齐为：`E=0` 的层不产生任何 draw（连采样器状态都不得触碰）。

## 5. 错误处理风格（兼容点）

- 所有 D3D/HLSL 调用都有错误串封装（0x80b334–0x80bdc8 的 `…Error.` 群，全目录见 `03` §5）；
  失败路径两种：**记日志后 `return -1` 终止本帧绘制**（绘制函数内多数 `SetTexture`/`SetRenderState` 失败），或**记日志后继续**（BeginPass/HLSL SetFloat 类）。
- 对齐含义：本仓库在 strict 模式下可把渲染后端失败升级为错误；但默认播放体验应与引擎一致——
  单层状态失败不崩播放器（记录 + 跳过该层），这与"错误态单一入口"（成果 100）不冲突，注意区分层。

## 6. 已定位但未逐行核的周边（登记）

| 项 | 锚点 | 状态 |
|---|---|---|
| D3D 设备创建/窗口化/分辨率协商 | `YS_3D_INIT. YS_DISPNO = %d, Now_Width = %d, Now_Height = %d`（0x80b1e0） | 锚点已挂，未拆。本仓库分辨率已按 yscfg 走（成果 121），初始化序列对齐**低优先** |
| Present 封装 `FUN_0041a420` | `D3D Present Error.` | 未逐行核（对齐低价值） |
| 帧推进 `FUN_004141e4`/`FUN_00414559` | 遍历器 +0x4c/+0x210 分支 | 未拆（属动画子系统，对齐时按可观察行为挂号） |
| `YS_PASTESPRITE`（`FUN_0040ae99` → `FUN_0040af1c`） | 视频帧/系统贴图 blit（W/H 自 `+0x78/+0x7c`） | 未拆；YMV 播放对齐时再取 |
| 顶点 FVF 精确标志 | `D3D SetFVF Error.`（0x80b334）xref | **Unknown**。顶点结构 84 字节实证见 03 §4;FVF 子集声明待读（对拍前必须补） |
