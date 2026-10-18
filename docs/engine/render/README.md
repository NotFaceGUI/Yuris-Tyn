# 渲染框架逆向（SDK 0.495 `yu-ris.exe`）

> 拆解对象：官方 SDK 0.495 的 `yu-ris.exe`
> （sha256 `4f5eefa3544c7244a907477d89ccee97286f559df5e38b1b1fa674206981138f`，
> 本机路径 `<side-repo>/workbench/project-eris/yu-ris.exe`）。
> 工具：REA 6.4.0 + Ghidra 12.1.4；全量反编译基线 = `<side-repo>/workbench/analysis/decomp/`（3964 函数中已导出 3698 个 `.c`）。
> 拆解日期：2026-10-15。上游基础：`旁线仓库/docs/render-semantics.md`（其 §2b 的更正继续有效）。

## 目的与本目录文档的用法

本目录是 Yuris-Tyn 渲染后端（`yuris-render` + `yuris-scene` + 播放器图层系统）的**对齐规格**：
公式与状态序列一律以二进制为准，手册只提供名字与参数范围。
**对齐总原则（见 `05-parity-matrix.md`）：SDK 渲染框架的每一项可观察行为都必须被支持 ——
允许做超集（扩展、性能优化），不允许存在"不支持"的原版行为。**

| 文档 | 内容 |
|---|---|
| `01-pipeline.md` | 帧循环、设备/效果对象表、图层遍历、每帧 D3D 调用序列、错误处理风格 |
| `02-layer-model.md` | 图层结构偏移表、图层树/图层组（渲染到纹理）、属性语义、FILE 替换保留属性 |
| `03-blend-modes.md` | `MODE=0..18` 描画模式：8 种固定管线模式的完整 D3D 状态序列 + 其余模式的可编程路径挂号 |
| `04-alpha-transition-offscreen.md` | `A=0..256`、`T/TA/TID` 过渡（HLSL effect）、掩码、`CGACT[COPY]`/离屏捕获 |
| `05-parity-matrix.md` | 1:1 对齐要求、功能矩阵（SDK 特性 × 本仓库现状）、验收与对拍方法、遗留挂号 |

## 证据约定

- 每条结论标注证据等级：**Confirmed**（反编译/字节实测 + 可复现）/ **Likely**（间接证据，实现须 `// UNVERIFIED` + 对照测试）/ **Unknown**（保持未实现，不猜）。
- 引用格式：函数用 `FUN_004xxxxx`（Ghidra 命名，`004xxxxx` = VA - 0x400000 之后的 VA 低 32 位写法，即 `0x4xxxxx+0x400000` 基址下的地址）；局部偏移用 `层+0xNN` 表示图层结构体字段。
- 反编译产物引用路径（`analysis/decomp/…c`）位于 `<side-repo>/workbench/analysis/decomp/`，不入库、可按 §"复现" 重新生成。

## 本次拆解的一页速览

1. 引擎是 **D3D9 固定管线为主 + ID3DXEffect（HLSL）为辅** 的 Immediate-Mode 立绘合成器：
   每图层每帧由 CPU 生成最多 84 字节/顶点的四边形顶点，`DrawPrimitiveUP(TRIANGLESTRIP, 2)` 直接画。
2. `MODE`（混合模式）的消费点在图层绘制函数 `FUN_00414ba4`（基址+0x14ba4）：
   8 种模式（0/1/2/3/5/6/7/9）走**固定管线**（`SetTextureStageState` + `SRCBLEND/DESTBLEND/BLENDOP`）；
   其余模式（4/8/10..18）在固定管线分支里**没有对应 case**，只能走可编程路径 —— 后者挂着
   `g_lpEffect`（HLSL），uniform 为 `YS_Texture1/2/3`、`YS_AlphaVal`、`YS_FadeVal`、`YS_FadeVal2`、`YS_TexSize`、`YS_TexDiffU`。
3. `A`（不透明度，手册 0..256）在绘制入口被 **clamp 到 0..255** 供固定管线（TEXTUREFACTOR/顶点色）使用；
   HLSL 侧则以 `A × 1/256`（0.00390625）为 uniform —— 两条路径的 256 语义不同，这是像素级对拍必须对齐的细节。
4. 过渡（`T/TA/TID`）= 双纹理 cross-fade：`YS_Texture1`=本层纹理、`YS_Texture2`=配对纹理，
   `YS_FadeVal = T / (256/(256/TA + 128)) × 1/256`（形状 Confirmed，参数槽位指认 Likely）。
5. 图层组（`ID` 尾 `/`）= **递归遍历 + 渲染到纹理**：子层先合成进组的离屏目标，组再作为普通层绘制
   —— 手册"组的 A 作用于子层合成后的结果"的二进制证据。
6. `CGACT[COPY]`（渲染到纹理）与截图共用 `GetRenderTargetData` 家族；
   捕获路径带一段 **CPU 逐通道 LUT**（以 0x80 为中性点的颜色调整）。

## 复现

```powershell
# REA/Ghidra 会话（env 四件套见 AGENTS.md §5 与 旁线仓库/docs/mcp-rea-registration.md）
# REA 工具链:open_binary(project-eris/yu-ris.exe) → binary_overview → 定向反编译
# 本地反编译树(免 REA 流量):
ls <side-repo>/workbench/analysis/decomp/ | measure   # 3698 个 .c
# .rdata 浮点常量核对(镜像偏移 = RVA, image base 0x400000):
#   0x58eb98=100.0  0x58eba0=0.5  0x58ebc0=100.0  0x58ebc8=0.0  0x58ebd0=0.5
#   0x58ec00=0.00390625(1/256)  0x58ec08=256.0  0x58ec10=128.0  0x58ef38=0.0078125(1/128)
```
