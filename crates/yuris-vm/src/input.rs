//! 输入系统变量的 VM 侧桥。
//!
//! ## 为什么桥而不是让 VM 依赖 `yuris-input`
//!
//! 依赖方向是 `player-core → {yuris-vm, yuris-input}`；VM 是 L3、输入层是 L6。
//! VM 只认识**引擎语义**（系统变量 id / `_INP` 数组），平台层每帧把
//! [`InputSnapshot`] 推进来，跑完后把脚本写入取走。这样：
//!
//! - VM 不碰 winit/Android/wasm；
//! - 输入状态的单一真值仍在 `yuris-input`（平台无关状态机）；
//! - 对拍时可以直接喂一个合成快照，不必起窗口。
//!
//! ## 写回（Confirmed：`FUN_00447014`）
//!
//! 脚本写输入变量时引擎按物理状态钳制。钳制发生在**输入层**
//! （`InputState::write_sysvar`）；VM 只记录写入并让同一帧内的后续读取
//! 看到新值（overlay），帧末由播放器应用回状态机。

use std::collections::{HashMap, HashSet};

use yuris_core::Result;
use yuris_format::ystb::CommandSlot;
use yuris_value::Value;

use crate::util::{value_summary, value_truthy};
use crate::GroupVm;

/// 一帧输入快照。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputSnapshot {
    /// **本帧声明为引擎输入系统变量的 id 集合**。
    ///
    /// 为什么不按 id 区间硬判：系统变量表序号逐版本漂移（SDK 0.479 比
    /// 0.495 整体前移 22）。实测样本 v555 里 `@409` 是**游戏自己的字符串
    /// 变量**（`@409 = $55[1]`），而在 0.495 表里 409 是 `_KEYNUM_ANY`
    /// —— 按 0.495 区间硬判会把游戏全局变量当输入状态劫持掉。
    /// 故由播放器按**已实证的版本面**声明：任何版本至少含 133/138
    /// （光标，样本 Confirmed）；494/495 声明完整表。
    pub claimed: HashSet<u16>,
    /// 标量系统变量（`id → 值`）。
    pub scalar: HashMap<u16, i64>,
    /// 数组型输入变量（`_PAD_*` 等；扁平化存储，见 [`Self::pad_index`]）。
    pub pad: HashMap<u16, Vec<i64>>,
    /// 可写系统变量 id（手册 ○ 标记）。
    pub writable: HashSet<u16>,
    /// 物理按住状态的按钮码（引擎钳制写回用；`FUN_00447014` 的
    /// `_DAT_00685400[code]`）。
    pub down: HashSet<u16>,
    /// `_INP` 数组（下标 = 按钮码）。
    pub inp: Vec<i64>,
}

impl InputSnapshot {
    /// `_INP(code)`。越界/未知 → 0（引擎边界外读 = 类型默认值）。
    pub fn inp_at(&self, code: i64) -> i64 {
        if code < 0 {
            return 0;
        }
        self.inp.get(code as usize).copied().unwrap_or(0)
    }

    /// 该 id 是否由输入层提供（必须在 `claimed` 里）。
    pub fn is_input_var(&self, id: u16) -> bool {
        self.claimed.contains(&id)
    }

    /// 该 id 是否可以由脚本赋值。
    pub fn is_writable(&self, id: u16) -> bool {
        self.claimed.contains(&id) && (id == 144 || self.writable.contains(&id))
    }

    /// SDK 0.494 / 0.495 的完整输入 id 表（133..=547）。
    ///
    /// `ysc.ybn` 不含系统变量表，只能按 SDK 的 `YSCom.ycd` 序列声明；
    /// 0.494 与 0.495 的逐项差异未取证（Likely）。
    pub fn sdk_ids() -> HashSet<u16> {
        (133..=547).collect()
    }

    /// 任何版本都实证的 id（光标坐标；样本 v555 Confirmed）。
    pub fn cursor_ids() -> HashSet<u16> {
        [133u16, 138].into_iter().collect()
    }

    /// 该按钮码当前是否物理按住。
    pub fn is_down(&self, code: u16) -> bool {
        self.down.contains(&code)
    }

    /// 输入系统变量 id → 按钮码（`@_KEY_*`/`@_MOUSE_*`；非按钮类 → None）。    ///
    /// 与 SDK 0.495 的 YSCM 系统变量序号对齐（`_MOUSE_L`=147、
    /// `_KEY_A`=167……`_KEY_ANY`=275）。
    pub fn button_of(id: u16) -> Option<u16> {
        match id {
            147..=166 => Some(MOUSE_STATE_CODES[(id - 147) as usize]),
            167..=274 => Some(KEY_STATE_CODES[(id - 167) as usize]),
            275 => Some(0), // `_KEY_ANY`：无单一物理键
            _ => None,
        }
    }

    /// 数组型输入变量的扁平下标：`_PAD_UP/DOWN/LEFT/RIGHT` 是 `pad`，
    /// `_PAD_BUTTON` 是 `pad * 32 + button`。
    pub fn pad_index(id: u16, indices: &[i64]) -> Option<usize> {
        match id {
            // `_PAD_UP(n)`：无下标按手柄 0（引擎参数必填；不因缺省报错）。
            276..=279 => Some(indices.first().map(|p| (*p).max(0) as usize).unwrap_or(0)),
            280 => match indices {
                [p, b, ..] => Some(((*p).max(0) as usize) * 32 + (*b).clamp(0, 31) as usize),
                [p] => Some(((*p).max(0) as usize) * 32),
                _ => Some(0),
            },
            _ => None,
        }
    }

    /// 读一个输入系统变量（含数组型）；未声明 → `None`。
    pub fn read(&self, id: u16, indices: &[i64]) -> Option<i64> {
        if !self.claimed.contains(&id) {
            return None;
        }
        if id == 144 {
            return Some(self.inp_at(indices.first().copied().unwrap_or(0)));
        }
        if let Some(v) = self.pad.get(&id) {
            let i = Self::pad_index(id, indices)?;
            return Some(v.get(i).copied().unwrap_or(0));
        }
        // 已声明但本帧未展开的输入 id（如手柄计数）：引擎里它们始终有值，
        // 缺省 = 0（类型默认），不得回落成“未定义变量”。
        Some(self.scalar.get(&id).copied().unwrap_or(0))
    }
}

/// `INPUT` / `MOUSE` 命令设置（引擎子系统状态）。
///
/// 手册 `system/input.html` / `system/mouse.html` 的关键字。播放器每帧读取
/// 并应用到输入层；`None` = 该关键字从未被脚本设置（保持输入层缺省）。
#[derive(Debug, Clone, PartialEq)]
pub struct EngineInputSettings {
    /// `INPUT[KEY]`；`None` = 未设置。
    pub keyboard: Option<bool>,
    /// `INPUT[MOUSE]`。
    pub mouse: Option<bool>,
    /// `INPUT[PAD]`。
    pub pad: Option<bool>,
    /// `MOUSE[MODE]`（0 常隐 / 1 自动隐藏 / 2 常显）。
    pub mouse_mode: Option<u8>,
    /// `MOUSE[TIME]`（毫秒）。
    pub mouse_time_ms: Option<u32>,
    /// `MOUSE[MOVE]`。
    pub move_reactive: Option<bool>,
    /// `MOUSE[CLICK]`。
    pub click_reactive: Option<bool>,
    /// `MOUSE[WHEEL]`。
    pub wheel_reactive: Option<bool>,
    /// `MOUSE[CURSOR]`（0..13）。
    pub cursor: Option<u8>,
    /// `MOUSE[X/Y]`：请求把 OS 光标移到窗口像素坐标。
    pub warp: Option<(i64, i64)>,
}

impl Default for EngineInputSettings {
    fn default() -> Self {
        Self {
            keyboard: None,
            mouse: None,
            pad: None,
            mouse_mode: None,
            mouse_time_ms: None,
            move_reactive: None,
            click_reactive: None,
            wheel_reactive: None,
            cursor: None,
            warp: None,
        }
    }
}

impl GroupVm {
    /// 该 id 是否属于引擎输入系统变量（有输入快照时按快照的 `claimed` 判；
    /// 无快照时只认样本 v555 已实证的光标 id）。
    pub(crate) fn is_input_var_id(&self, id: u16) -> bool {
        match &self.input {
            Some(s) => s.is_input_var(id),
            // 不按 0.495 的 133..547 硬判 —— 见 `InputSnapshot::claimed`。
            None => id == 133 || id == 138,
        }
    }

    /// 推进本帧输入快照（播放器在 `pump_frame` 前调用）。
    pub fn set_input_snapshot(&mut self, snapshot: InputSnapshot) {
        // 求值器看到的是同一份数据的纯值视图。脚本写入时经
        // `Arc::make_mut` 更新，故两边不会漂移。
        let mut vars = yuris_script::eval::InputVars::default();
        vars.claimed = snapshot.claimed.clone();
        vars.scalars = snapshot.scalar.clone();
        vars.pad = snapshot.pad.clone();
        vars.inp = snapshot.inp.clone();
        self.input_vars = Some(std::sync::Arc::new(vars));
        self.input = Some(snapshot);
        self.input_overlay.clear();
        self.input_writes.clear();
    }

    /// 移除快照（无头/测试环境；回退到 `set_input_cursor` 注入的坐标）。
    pub fn clear_input_snapshot(&mut self) {
        self.input = None;
        self.input_vars = None;
        self.input_overlay.clear();
        self.input_writes.clear();
    }

    /// 取走本帧脚本写入（`(id, 数组下标, 值)`）；应用后由播放器回灌状态机。
    pub fn take_input_writes(&mut self) -> Vec<(u16, Option<i64>, Value)> {
        std::mem::take(&mut self.input_writes)
    }

    /// `INPUT`/`MOUSE` 命令累计设置。
    pub fn input_settings(&self) -> &EngineInputSettings {
        &self.input_settings
    }

    /// 录一次脚本写输入变量（写 overlay + outbox）。
    ///
    /// 钳制规则与引擎 `FUN_00447014` 一致，**在这里应用**（而不是留给播放器）：
    /// 同一帧内脚本写完立刻读必须看到钳制后的值，否则
    /// `@_INP(@code)=2` 后的 `IF @_INP(@code)>0` 会读到原值。
    pub(crate) fn write_input_var(
        &mut self,
        id: u16,
        indices: &[i64],
        v: Value,
    ) -> Result<()> {
        let Some(snap) = &self.input else {
            return Err(yuris_core::Error::format(format!(
                "写入输入系统变量 @{id} 但未注入输入快照(无输入面)"
            )));
        };
        if !snap.is_writable(id) {
            return Err(yuris_core::Error::format(format!(
                "输入系统变量 @{id} 禁止赋值(引擎手册未标 ○)"
            )));
        }
        let v = match v {
            Value::Int(n) => {
                let clamped = if id == 144 {
                    // `_INP(code) = n`：按该按钮码的物理状态钳制。
                    let code = indices.first().copied().unwrap_or(0);
                    clamp_write(snap.is_down(code.max(0) as u16), n)
                } else {
                    match InputSnapshot::button_of(id) {
                        Some(code) => clamp_write(snap.is_down(code), n),
                        // 非按钮类输入变量（计数/常量）：引擎手册未标可写，
                        // 这里只允许 `_KEYLAST/_MOUSELAST`（上面已放行）。
                        None => n,
                    }
                };
                Value::Int(clamped)
            }
            other => other,
        };
        // 计数类（`_KEYNUM_*` 等）不可写；`writable` 集合已保证只有标量状态。
        let key = (id, indices.first().copied().unwrap_or(-1));
        self.input_overlay.insert(key, v.clone());
        if let Some(vars) = self.input_vars.as_mut() {
            let vars = std::sync::Arc::make_mut(vars);
            if id == 144 {
                let code = indices.first().copied().unwrap_or(0);
                if code >= 0 {
                    let i = code as usize;
                    if vars.inp.len() <= i {
                        vars.inp.resize(i + 1, 0);
                    }
                    vars.inp[i] = v.as_int_opt().unwrap_or(0);
                }
            } else if vars.pad.contains_key(&id) {
                if let Some(i) = InputSnapshot::pad_index(id, indices) {
                    if let Some(slot) = vars.pad.get_mut(&id) {
                        if slot.len() <= i {
                            slot.resize(i + 1, 0);
                        }
                        slot[i] = v.as_int_opt().unwrap_or(0);
                    }
                }
            } else {
                vars.scalars.insert(id, v.as_int_opt().unwrap_or(0));
            }
        }
        self.input_writes.push((id, indices.first().copied(), v));
        Ok(())
    }

    /// `INPUT` / `MOUSE` 命令的子系统状态更新。
    ///
    /// 参数槽一律按**命令自身 YSCM 参数名**解析（跨版本槽号漂移；
    /// `param_slot` 在无参数表时回落 v555 号并计数）。
    pub(crate) fn apply_input_command(
        &mut self,
        name: &str,
        command_index: u8,
        windows: &[CommandSlot],
    ) -> Result<()> {
        if name.eq_ignore_ascii_case("INPUT") {
            if let Some(v) = self.input_bool(command_index, windows, "KEY", 0)? {
                self.input_settings.keyboard = Some(v);
            }
            if let Some(v) = self.input_bool(command_index, windows, "MOUSE", 2)? {
                self.input_settings.mouse = Some(v);
            }
            if let Some(v) = self.input_bool(command_index, windows, "PAD", 1)? {
                self.input_settings.pad = Some(v);
            }
            return Ok(());
        }
        if !name.eq_ignore_ascii_case("MOUSE") {
            return Ok(());
        }
        if let Some(v) = self.input_int(command_index, windows, "MODE", 0)? {
            self.input_settings.mouse_mode = Some(v.clamp(0, 2) as u8);
        }
        if let Some(v) = self.input_int(command_index, windows, "TIME", 1)? {
            self.input_settings.mouse_time_ms = Some(v.max(0).min(u32::MAX as i64) as u32);
        }
        if let Some(v) = self.input_bool(command_index, windows, "MOVE", 2)? {
            self.input_settings.move_reactive = Some(v);
        }
        if let Some(v) = self.input_bool(command_index, windows, "CLICK", 3)? {
            self.input_settings.click_reactive = Some(v);
        }
        if let Some(v) = self.input_bool(command_index, windows, "WHEEL", 4)? {
            self.input_settings.wheel_reactive = Some(v);
        }
        if let Some(v) = self.input_int(command_index, windows, "CURSOR", 7)? {
            self.input_settings.cursor = Some(v.clamp(0, 13) as u8);
        }
        let x = self.input_int(command_index, windows, "X", 5)?;
        let y = self.input_int(command_index, windows, "Y", 6)?;
        if let (Some(x), Some(y)) = (x, y) {
            self.input_settings.warp = Some((x, y));
        }
        Ok(())
    }

    fn input_bool(
        &mut self,
        command_index: u8,
        windows: &[CommandSlot],
        param: &str,
        fallback: u8,
    ) -> Result<Option<bool>> {
        match self.input_param(command_index, windows, param, fallback)? {
            Some(v) => Ok(Some(value_truthy(&v)?)),
            None => Ok(None),
        }
    }

    fn input_int(
        &mut self,
        command_index: u8,
        windows: &[CommandSlot],
        param: &str,
        fallback: u8,
    ) -> Result<Option<i64>> {
        Ok(self
            .input_param(command_index, windows, param, fallback)?
            .and_then(|v| v.as_int_opt()))
    }

    fn input_param(
        &mut self,
        command_index: u8,
        windows: &[CommandSlot],
        param: &str,
        fallback: u8,
    ) -> Result<Option<Value>> {
        let Some(slot) = self.param_slot(command_index, param, fallback) else {
            return Ok(None);
        };
        for w in windows {
            if w.len == 0 {
                continue;
            }
            if (w.tag & 0xff) as u8 != slot {
                continue;
            }
            if let Some(v) = self.eval_window_condition(Some(w))? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// 诊断：把当前输入快照摘要成一行。
    pub fn input_snapshot_summary(&self) -> String {
        match &self.input {
            None => "<none>".to_string(),
            Some(s) => format!(
                "cursor=({},{}) mouseL={} enter={} inp[1]={} scalar={} pad={} writes={}",
                s.read(133, &[]).unwrap_or(0),
                s.read(138, &[]).unwrap_or(0),
                s.read(147, &[]).unwrap_or(0),
                s.read(244, &[]).unwrap_or(0),
                s.inp_at(1),
                s.scalar.len(),
                s.pad.len(),
                self.input_writes.len()
            ),
        }
    }

    /// 诊断：本帧已写入的输入变量摘要（`YURIS_TRACE_INPUT`）。
    pub fn input_write_summary(&self) -> String {
        self.input_writes
            .iter()
            .map(|(id, idx, v)| match idx {
                Some(i) => format!("@{id}[{i}]={}", value_summary(v)),
                None => format!("@{id}={}", value_summary(v)),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yuris_value::VarRef;

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
        let file = yuris_format::ystb::YstbFile::from_bytes(&out, key).expect("ystb");
        GroupVm::load(file).expect("vm")
    }

    fn snapshot() -> InputSnapshot {
        let mut s = InputSnapshot::default();
        s.claimed = (133..=547).collect();
        s.scalar.insert(133, 100);
        s.scalar.insert(138, 200);
        s.scalar.insert(147, 1);
        s.scalar.insert(167, 2);
        s.writable.insert(147);
        s.writable.insert(167);
        s.writable.insert(168);
        s.down.insert(0x01);
        s.down.insert(0x41);
        let mut inp = vec![0i64; 256];
        inp[0x01] = 1;
        inp[0x41] = 3;
        s.inp = inp;
        s
    }

    #[test]
    fn reads_snapshot_before_variable_store() {
        let mut vm = minimal_vm();
        vm.set_input_snapshot(snapshot());
        let at = |id| VarRef {
            space: yuris_value::VarSpace::At,
            id,
        };
        assert_eq!(vm.read_var_value(&at(133), &[]), Some(Value::Int(100)));
        assert_eq!(vm.read_var_value(&at(138), &[]), Some(Value::Int(200)));
        assert_eq!(vm.read_var_value(&at(147), &[]), Some(Value::Int(1)));
        assert_eq!(
            vm.read_var_value(&at(144), &[0x41]),
            Some(Value::Int(3))
        );
        // 未知输入码 = 0（引擎边界外读）
        assert_eq!(vm.read_var_value(&at(144), &[999]), Some(Value::Int(0)));
    }

    #[test]
    fn write_is_clamped_by_physical_state() {
        let mut vm = minimal_vm();
        vm.set_input_snapshot(snapshot());
        // 物理按住 → 写正数保留
        vm.write_input_var(147, &[], Value::Int(5)).unwrap();
        assert_eq!(
            vm.read_var_value(
                &VarRef {
                    space: yuris_value::VarSpace::At,
                    id: 147
                },
                &[]
            ),
            Some(Value::Int(5))
        );
        // 同帧写 0 → 0
        vm.write_input_var(147, &[], Value::Int(0)).unwrap();
        assert_eq!(
            vm.read_var_value(
                &VarRef {
                    space: yuris_value::VarSpace::At,
                    id: 147
                },
                &[]
            ),
            Some(Value::Int(0))
        );
        // 未按住的键（168 = _KEY_B 不在 down 集）写正数 → 0
        vm.write_input_var(168, &[], Value::Int(9)).unwrap();
        assert_eq!(
            vm.read_var_value(
                &VarRef {
                    space: yuris_value::VarSpace::At,
                    id: 168
                },
                &[]
            ),
            Some(Value::Int(0))
        );
    }

    #[test]
    fn writes_are_queued_for_playback() {
        let mut vm = minimal_vm();
        vm.set_input_snapshot(snapshot());
        vm.write_input_var(144, &[0x41], Value::Int(2)).unwrap();
        let writes = vm.take_input_writes();
        assert_eq!(writes, vec![(144u16, Some(0x41), Value::Int(2))]);
        assert!(vm.take_input_writes().is_empty());
    }

    #[test]
    fn write_without_snapshot_errors() {
        let mut vm = minimal_vm();
        let e = vm
            .write_input_var(147, &[], Value::Int(1))
            .unwrap_err();
        assert!(e.to_string().contains("未注入输入快照"), "{e}");
    }

    #[test]
    fn inp_bounds_are_zero() {
        let s = InputSnapshot {
            inp: vec![1, 2, 3],
            ..Default::default()
        };
        assert_eq!(s.inp_at(1), 2);
        assert_eq!(s.inp_at(99), 0);
        assert_eq!(s.inp_at(-1), 0);
    }

    #[test]
    fn pad_array_indexing() {
        let mut s = InputSnapshot::default();
        s.claimed.extend([276u16, 280]);
        s.pad.insert(276, (0..16).map(|i| i as i64).collect());
        s.pad.insert(280, (0..512).map(|i| i as i64).collect());
        assert_eq!(s.read(276, &[3]), Some(3));
        assert_eq!(s.read(280, &[2, 5]), Some(69));
        assert_eq!(s.read(280, &[]), Some(0));
    }

    #[test]
    fn writable_ids() {
        let mut s = InputSnapshot::default();
        s.claimed.insert(147);
        s.scalar.insert(147, 1);
        assert!(s.is_input_var(147));
        assert!(!s.is_writable(147));
        s.writable.insert(147);
        assert!(s.is_writable(147));
        // `_INP` 本身可写，但也必须在 claimed 里（未声明 = 不是输入面）。
        assert!(!s.is_writable(144));
        s.claimed.insert(144);
        assert!(s.is_writable(144));
    }

    #[test]
    fn button_of_maps_engine_ids() {
        assert_eq!(InputSnapshot::button_of(147), Some(0x01)); // _MOUSE_L
        assert_eq!(InputSnapshot::button_of(150), Some(0x01)); // _MOUSE_L3
        assert_eq!(InputSnapshot::button_of(167), Some(0x41)); // _KEY_A
        assert_eq!(InputSnapshot::button_of(244), Some(0x0D)); // _KEY_ENTER
        assert_eq!(InputSnapshot::button_of(275), Some(0)); // _KEY_ANY
    }
}

/// 引擎 `FUN_00447014` 的写回钳制。
fn clamp_write(down: bool, v: i64) -> i64 {
    if !down {
        if v >= 0 {
            0
        } else {
            v
        }
    } else if v > 0 {
        v
    } else {
        0
    }
}

/// `_MOUSE_L/L1/L2/L3 … _MOUSE_D3`（id 147..=166）→ 按钮码。
///
/// 引擎只有一只鼠标、一种按键状态；L1/L2/L3 分级语义 Unknown，
/// 统一映射到基础按钮（`keys` 模块文档）。
const MOUSE_STATE_CODES: [u16; 20] = [
    0x01, 0x01, 0x01, 0x01, // L, L1, L2, L3
    0x02, 0x02, 0x02, 0x02, // R, R1, R2, R3
    0x04, 0x04, 0x04, 0x04, // M, M1, M2, M3
    0x05, 0x05, 0x05, 0x05, // U, U1, U2, U3
    0x06, 0x06, 0x06, 0x06, // D, D1, D2, D3
];

/// `_KEY_A … _KEY_BACKSLASH`（id 167..=274）→ 按钮码。
const KEY_STATE_CODES: [u16; 108] = [
    // A-Z
    0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E,
    0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A,
    // 0-9
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
    // F1-F24
    0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D,
    0x7E, 0x7F, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    // 小键盘 0-9
    0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    // 小键盘 + - * / .
    0x6B, 0x6D, 0x6A, 0x6F, 0x6E,
    // BS, TAB, ENTER, SPACE, SHIFT, CTRL, ALT, LEFT, RIGHT, UP, DOWN,
    // INSERT, DELETE, HELP, SCROLL, NUMLOCK, ESC, PAGEUP, PAGEDOWN,
    // HYPHEN, HOME, END, CIRCUMFLEX, YEN, AT, LEFTBRACKET, SEMICOLON,
    // COLON, RIGHTBRACKET, COMMA, PERIOD, SLASH, BACKSLASH
    0x08, 0x09, 0x0D, 0x20, 0x10, 0x11, 0x12, 0x25, 0x27, 0x26, 0x28, 0x2D, 0x2E, 0x2F,
    0x91, 0x90, 0x1B, 0x21, 0x22, 0xBD, 0x24, 0x23, 0xDE, 0xDC, 0xC0, 0xDB, 0xBA, 0xBA,
    0xDD, 0xBC, 0xBE, 0xBF, 0xE2,
];
