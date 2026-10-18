//! 引擎按键/鼠标/手柄的逻辑键与按钮码。
//!
//! ## 代码空间（Confirmed，结构级）
//!
//! 引擎的输入状态是一张 **按 Windows 虚拟键码（VK）索引的 64 位计数表**
//! （`0x00685800`，见 `FUN_0041a4b8` 的 `[eax*8 + 0x685800]` 与
//! `FUN_00427350` 的 `_INP` 分支 `[param_2 * 8 + 0x685800]`）。
//! 因此脚本里的 `@_CODE_*` 常量就是 VK：
//!
//! - 鼠标：`VK_LBUTTON=0x01`、`VK_RBUTTON=0x02`、`VK_MBUTTON=0x04`；
//!   滚轮上/下与“鼠标移动”在引擎里复用 VK 5/6/7
//!   （`FUN_0041a4b8`：`DAT_00685005/06 = 0x80` 表示滚轮，`DAT_00685007`
//!   取 `DAT_00801910` 表示移动）。
//! - 键盘：标准 VK（A-Z 0x41..0x5A、0-9 0x30..0x39、F1-F24 0x70..0x87、
//!   小键盘 0x60..、Enter 0x0D、Space 0x20……）。
//! - 手柄：`0x180` 起的独立区（`FUN_00427350` case 0x114..0x117 / 0x19a：
//!   `index = 0x180 + (id - 0x114)`，即方向键 384..387）。
//!
//! 证据等级：
//! - **Confirmed（结构）**：`_CODE_*` 与 `@_INP(n)` 同处一张 VK 索引表；
//!   字母/数字/F 键/主键区/鼠标按钮的 VK 值是 Windows 规范值。
//! - **Likely（`UNVERIFIED`）**：日文键盘符号键的 VK 选择
//!   （`HYPHEN`/`CIRCUMFLEX`/`YEN`/`AT`/`COLON`/`BACKSLASH`）。
//!   引擎对 `SEMICOLON`/`COLON` 使用同一 VK（0xBA）还是不同伪码，
//!   当前证据只能确认“符号名存在”，不能确认逐值。
//! - **Unknown**：`_PAD_*` 的编号与官方示例脚本 `_strtocode` 的
//!   `PAD1_LEFT=384` 不一致（引擎表顺序是 UP/DOWN/LEFT/RIGHT = 384..387）。
//!   本模块按**引擎表顺序**输出，见 [`PAD_UP`] 等常量。

use std::fmt;

/// 引擎按钮码（VK 空间 0..=255；手柄 384..）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ButtonCode(pub u16);

impl ButtonCode {
    /// VK 空间大小（`DAT_00685000` 的 256 字节）。
    pub const VK_COUNT: usize = 256;

    /// 手柄方向/按钮区的起始下标（`0x180`）。
    pub const PAD_BASE: u16 = 0x180;

    /// 由 VK 构造（调试断言范围内）。
    pub const fn vk(v: u16) -> Self {
        Self(v)
    }

    /// 原始值。
    pub const fn raw(self) -> u16 {
        self.0
    }

    /// 是否属于 VK 空间。
    pub const fn is_vk(self) -> bool {
        self.0 < Self::VK_COUNT as u16
    }
}

impl fmt::Display for ButtonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:02X}", self.0)
    }
}

// ---- 鼠标（Confirmed：WM_LBUTTONDOWN→_MOUSELAST=1、WM_RBUTTONDOWN→2、
//      WM_MBUTTONDOWN→4；滚轮/移动为 VK 5/6/7）----
/// 鼠标左键（`VK_LBUTTON`）。
pub const MOUSE_LEFT: ButtonCode = ButtonCode(0x01);
/// 鼠标右键（`VK_RBUTTON`）。
pub const MOUSE_RIGHT: ButtonCode = ButtonCode(0x02);
/// 鼠标中键（`VK_MBUTTON`）。
pub const MOUSE_MIDDLE: ButtonCode = ButtonCode(0x04);
/// 滚轮上（引擎复用 VK 5）。
pub const WHEEL_UP: ButtonCode = ButtonCode(0x05);
/// 滚轮下（引擎复用 VK 6）。
pub const WHEEL_DOWN: ButtonCode = ButtonCode(0x06);
/// 鼠标移动脉冲（引擎复用 VK 7；`DAT_00685007`）。
pub const MOUSE_MOVE: ButtonCode = ButtonCode(0x07);

// ---- 主键区（Windows VK 规范值）----
/// Backspace。
pub const BS: ButtonCode = ButtonCode(0x08);
/// Tab。
pub const TAB: ButtonCode = ButtonCode(0x09);
/// Enter。
pub const ENTER: ButtonCode = ButtonCode(0x0D);
/// Shift。
pub const SHIFT: ButtonCode = ButtonCode(0x10);
/// Ctrl。
pub const CTRL: ButtonCode = ButtonCode(0x11);
/// Alt。
pub const ALT: ButtonCode = ButtonCode(0x12);
/// Esc。
pub const ESC: ButtonCode = ButtonCode(0x1B);
/// Space。
pub const SPACE: ButtonCode = ButtonCode(0x20);
/// PageUp。
pub const PAGEUP: ButtonCode = ButtonCode(0x21);
/// PageDown。
pub const PAGEDOWN: ButtonCode = ButtonCode(0x22);
/// End。
pub const END: ButtonCode = ButtonCode(0x23);
/// Home。
pub const HOME: ButtonCode = ButtonCode(0x24);
/// 方向键左。
pub const LEFT: ButtonCode = ButtonCode(0x25);
/// 方向键上。
pub const UP: ButtonCode = ButtonCode(0x26);
/// 方向键右。
pub const RIGHT: ButtonCode = ButtonCode(0x27);
/// 方向键下。
pub const DOWN: ButtonCode = ButtonCode(0x28);
/// Insert。
pub const INSERT: ButtonCode = ButtonCode(0x2D);
/// Delete。
pub const DELETE: ButtonCode = ButtonCode(0x2E);
/// Help。
pub const HELP: ButtonCode = ButtonCode(0x2F);
/// NumLock。
pub const NUMLOCK: ButtonCode = ButtonCode(0x90);
/// ScrollLock。
pub const SCROLL: ButtonCode = ButtonCode(0x91);

// ---- 符号键（Likely/UNVERIFIED：日文键盘布局的 VK 选择）----
/// `;`/`:` 键（`VK_OEM_1`）。
pub const SEMICOLON: ButtonCode = ButtonCode(0xBA);
/// `:`（与分号同 VK；引擎是否另有伪码 Unknown）。
pub const COLON: ButtonCode = ButtonCode(0xBA);
/// `,`（`VK_OEM_COMMA`）。
pub const COMMA: ButtonCode = ButtonCode(0xBC);
/// `-`（`VK_OEM_MINUS`）。
pub const HYPHEN: ButtonCode = ButtonCode(0xBD);
/// `.`（`VK_OEM_PERIOD`）。
pub const PERIOD: ButtonCode = ButtonCode(0xBE);
/// `/`（`VK_OEM_2`）。
pub const SLASH: ButtonCode = ButtonCode(0xBF);
/// `@`（日文键盘 `VK_OEM_3`）。
pub const AT: ButtonCode = ButtonCode(0xC0);
/// `[`（`VK_OEM_4`）。
pub const LEFTBRACKET: ButtonCode = ButtonCode(0xDB);
/// `\`/`¥`（日文键盘 `VK_OEM_5`）。
pub const YEN: ButtonCode = ButtonCode(0xDC);
/// `]`（`VK_OEM_6`）。
pub const RIGHTBRACKET: ButtonCode = ButtonCode(0xDD);
/// `^`（日文键盘 `VK_OEM_7`）。
pub const CIRCUMFLEX: ButtonCode = ButtonCode(0xDE);
/// `\`（`VK_OEM_102`；与 `YEN` 区分，UNVERIFIED）。
pub const BACKSLASH: ButtonCode = ButtonCode(0xE2);

// ---- 手柄（引擎表顺序；见模块文档的 Unknown 说明）----
/// 手柄上。
pub const PAD_UP: ButtonCode = ButtonCode(384);
/// 手柄下。
pub const PAD_DOWN: ButtonCode = ButtonCode(385);
/// 手柄左。
pub const PAD_LEFT: ButtonCode = ButtonCode(386);
/// 手柄右。
pub const PAD_RIGHT: ButtonCode = ButtonCode(387);
/// 手柄按钮 0..7 的起始码（`@_PAD_BUTTON(n,m)` 的二维引用在 VM 层）。
pub const PAD_BUTTON_BASE: ButtonCode = ButtonCode(388);

/// 平台无关的逻辑键。
///
/// 只表达引擎有名字的键；引擎没有的键（Pause/PrintScreen/Meta 等）
/// 映射为 `None`（不猜）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// 字符键（小写字母、数字、ASCII 符号；Shift 后的字符由平台给出）。
    Char(char),
    /// Enter。
    Enter,
    /// Esc。
    Escape,
    /// Space。
    Space,
    /// Tab。
    Tab,
    /// Backspace。
    Backspace,
    /// Shift（左右均可）。
    Shift,
    /// Ctrl（左右均可）。
    Control,
    /// Alt（左右均可）。
    Alt,
    /// 方向键。
    Left,
    /// 方向键。
    Right,
    /// 方向键。
    Up,
    /// 方向键。
    Down,
    /// Insert。
    Insert,
    /// Delete。
    Delete,
    /// Home。
    Home,
    /// End。
    End,
    /// PageUp。
    PageUp,
    /// PageDown。
    PageDown,
    /// F1..=F24。
    F(u8),
    /// 小键盘数字 0..=9。
    Numpad(u8),
    /// 小键盘 `+`。
    NumpadAdd,
    /// 小键盘 `-`。
    NumpadSubtract,
    /// 小键盘 `*`。
    NumpadMultiply,
    /// 小键盘 `/`。
    NumpadDivide,
    /// 小键盘 `.`。
    NumpadDecimal,
    /// NumLock。
    NumLock,
    /// ScrollLock。
    ScrollLock,
    /// Help。
    Help,
}

impl Key {
    /// 逻辑键 → 引擎按钮码。引擎没有对应键 → `None`。
    pub fn to_button(self) -> Option<ButtonCode> {
        Some(match self {
            Key::Char(c) => char_to_button(c)?,
            Key::Enter => ENTER,
            Key::Escape => ESC,
            Key::Space => SPACE,
            Key::Tab => TAB,
            Key::Backspace => BS,
            Key::Shift => SHIFT,
            Key::Control => CTRL,
            Key::Alt => ALT,
            Key::Left => LEFT,
            Key::Right => RIGHT,
            Key::Up => UP,
            Key::Down => DOWN,
            Key::Insert => INSERT,
            Key::Delete => DELETE,
            Key::Home => HOME,
            Key::End => END,
            Key::PageUp => PAGEUP,
            Key::PageDown => PAGEDOWN,
            Key::F(n) if (1..=24).contains(&n) => ButtonCode(0x70 + (n as u16 - 1)),
            Key::Numpad(n) if n <= 9 => ButtonCode(0x60 + n as u16),
            Key::NumpadAdd => ButtonCode(0x6B),
            Key::NumpadSubtract => ButtonCode(0x6D),
            Key::NumpadMultiply => ButtonCode(0x6A),
            Key::NumpadDivide => ButtonCode(0x6F),
            Key::NumpadDecimal => ButtonCode(0x6E),
            Key::NumLock => NUMLOCK,
            Key::ScrollLock => SCROLL,
            Key::Help => HELP,
            Key::F(_) | Key::Numpad(_) => return None,
        })
    }
}

/// 字符 → 引擎按钮码（字母/数字/符号）。
///
/// 字母只接受 ASCII 大小写；符号按日文键盘可产生的字符映射。
/// `:` 与 `;` 同码（见模块文档的 UNVERIFIED）。
pub fn char_to_button(c: char) -> Option<ButtonCode> {
    Some(match c {
        'A'..='Z' => ButtonCode(0x41 + (c as u16 - 'A' as u16)),
        'a'..='z' => ButtonCode(0x41 + (c as u16 - 'a' as u16)),
        '0'..='9' => ButtonCode(0x30 + (c as u16 - '0' as u16)),
        ';' => SEMICOLON,
        ':' => COLON,
        ',' => COMMA,
        '-' => HYPHEN,
        '.' => PERIOD,
        '/' => SLASH,
        '@' => AT,
        '[' => LEFTBRACKET,
        ']' => RIGHTBRACKET,
        '^' => CIRCUMFLEX,
        '¥' => YEN,
        '\\' => BACKSLASH,
        _ => return None,
    })
}

/// 按钮码 → 引擎**变量名后缀**（`@_KEY_*` / `@_MOUSE_*` / `@_PAD_*` 去掉前缀）。
///
/// 用于诊断（`YURIS_TRACE_INPUT`）与 `inject_input_key` 风格的字符串路径。
pub fn button_name(code: ButtonCode) -> Option<&'static str> {
    ENGINE_BUTTON_NAMES
        .iter()
        .find_map(|(name, c)| (*c == code.0).then_some(*name))
}

/// 由引擎后缀名（大小写不敏感）解析按钮码。
///
/// 接受 `"MOUSE_L"`、`"ENTER"`、`"F5"`、`"A"` 等；也接受已带
/// `_KEY_`/`_MOUSE_`/`_PAD_` 前缀的完整名。
pub fn button_from_name(name: &str) -> Option<ButtonCode> {
    let upper = name.trim().to_ascii_uppercase();
    let lookup = |needle: &str| {
        ENGINE_BUTTON_NAMES
            .iter()
            .find_map(|(n, c)| (*n == needle).then_some(ButtonCode(*c)))
    };
    // 完整名（`MOUSE_L` / `F5` / `ENTER`）。
    if let Some(code) = lookup(&upper) {
        return Some(code);
    }
    // 引擎变量名去掉前导下划线（`_MOUSE_L` → `MOUSE_L`）。
    let bare = upper.strip_prefix('_').unwrap_or(&upper);
    if let Some(code) = lookup(bare) {
        return Some(code);
    }
    // 通用键盘前缀（`KEY_ENTER` → `ENTER`）。
    lookup(bare.strip_prefix("KEY_").unwrap_or(bare))
}

/// 引擎变量名（含前缀）→ 按钮码。
///
/// `ysc.ybn` 的参数与脚本里看到的是 `_KEY_ENTER` / `_MOUSE_L` 这种完整名；
/// [`button_from_name`] 两者都接受，本函数保留给调用方需要显式区分时用。
pub fn button_from_sysvar_name(name: &str) -> Option<ButtonCode> {
    button_from_name(name)
}

/// 引擎按钮名 → 码表。
///
/// 覆盖 `ysc.ybn`（YSCM）可见的全部 `_KEY_*` / `_MOUSE_*` / `_PAD_*` 名字
/// （不含 `_CODE_*` 前缀与 `_KEYNUM_*` 计数族）。顺序 = SDK 0.495 的
/// 系统变量表顺序（`YSCom.ycd`），便于与 YSCD 对照。
pub const ENGINE_BUTTON_NAMES: &[(&str, u16)] = &[
    ("MOUSE_L", 0x01),
    ("MOUSE_R", 0x02),
    ("MOUSE_M", 0x04),
    ("MOUSE_U", 0x05),
    ("MOUSE_D", 0x06),
    ("A", 0x41),
    ("B", 0x42),
    ("C", 0x43),
    ("D", 0x44),
    ("E", 0x45),
    ("F", 0x46),
    ("G", 0x47),
    ("H", 0x48),
    ("I", 0x49),
    ("J", 0x4A),
    ("K", 0x4B),
    ("L", 0x4C),
    ("M", 0x4D),
    ("N", 0x4E),
    ("O", 0x4F),
    ("P", 0x50),
    ("Q", 0x51),
    ("R", 0x52),
    ("S", 0x53),
    ("T", 0x54),
    ("U", 0x55),
    ("V", 0x56),
    ("W", 0x57),
    ("X", 0x58),
    ("Y", 0x59),
    ("Z", 0x5A),
    ("0", 0x30),
    ("1", 0x31),
    ("2", 0x32),
    ("3", 0x33),
    ("4", 0x34),
    ("5", 0x35),
    ("6", 0x36),
    ("7", 0x37),
    ("8", 0x38),
    ("9", 0x39),
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("F12", 0x7B),
    ("F13", 0x7C),
    ("F14", 0x7D),
    ("F15", 0x7E),
    ("F16", 0x7F),
    ("F17", 0x80),
    ("F18", 0x81),
    ("F19", 0x82),
    ("F20", 0x83),
    ("F21", 0x84),
    ("F22", 0x85),
    ("F23", 0x86),
    ("F24", 0x87),
    ("NUM0", 0x60),
    ("NUM1", 0x61),
    ("NUM2", 0x62),
    ("NUM3", 0x63),
    ("NUM4", 0x64),
    ("NUM5", 0x65),
    ("NUM6", 0x66),
    ("NUM7", 0x67),
    ("NUM8", 0x68),
    ("NUM9", 0x69),
    ("NUMADD", 0x6B),
    ("NUMSUB", 0x6D),
    ("NUMMUL", 0x6A),
    ("NUMDIV", 0x6F),
    ("NUMDEC", 0x6E),
    ("BS", 0x08),
    ("TAB", 0x09),
    ("ENTER", 0x0D),
    ("SPACE", 0x20),
    ("SHIFT", 0x10),
    ("CTRL", 0x11),
    ("ALT", 0x12),
    ("LEFT", 0x25),
    ("RIGHT", 0x27),
    ("UP", 0x26),
    ("DOWN", 0x28),
    ("INSERT", 0x2D),
    ("DELETE", 0x2E),
    ("HELP", 0x2F),
    ("SCROLL", 0x91),
    ("NUMLOCK", 0x90),
    ("ESC", 0x1B),
    ("PAGEUP", 0x21),
    ("PAGEDOWN", 0x22),
    ("HYPHEN", 0xBD),
    ("HOME", 0x24),
    ("END", 0x23),
    ("CIRCUMFLEX", 0xDE),
    ("YEN", 0xDC),
    ("AT", 0xC0),
    ("LEFTBRACKET", 0xDB),
    ("SEMICOLON", 0xBA),
    ("COLON", 0xBA),
    ("RIGHTBRACKET", 0xDD),
    ("COMMA", 0xBC),
    ("PERIOD", 0xBE),
    ("SLASH", 0xBF),
    ("BACKSLASH", 0xE2),
    ("PAD_UP", 384),
    ("PAD_DOWN", 385),
    ("PAD_LEFT", 386),
    ("PAD_RIGHT", 387),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_and_digit_codes_are_vk() {
        assert_eq!(Key::Char('a').to_button(), Some(ButtonCode(0x41)));
        assert_eq!(Key::Char('Z').to_button(), Some(ButtonCode(0x5A)));
        assert_eq!(Key::Char('0').to_button(), Some(ButtonCode(0x30)));
        assert_eq!(Key::Char('9').to_button(), Some(ButtonCode(0x39)));
    }

    #[test]
    fn function_and_numpad_codes() {
        assert_eq!(Key::F(1).to_button(), Some(ButtonCode(0x70)));
        assert_eq!(Key::F(24).to_button(), Some(ButtonCode(0x87)));
        assert_eq!(Key::Numpad(0).to_button(), Some(ButtonCode(0x60)));
        assert_eq!(Key::Numpad(9).to_button(), Some(ButtonCode(0x69)));
        assert_eq!(Key::NumpadAdd.to_button(), Some(ButtonCode(0x6B)));
        assert_eq!(Key::NumpadSubtract.to_button(), Some(ButtonCode(0x6D)));
    }

    #[test]
    fn names_roundtrip() {
        for (name, code) in ENGINE_BUTTON_NAMES {
            assert_eq!(
                button_from_name(name),
                Some(ButtonCode(*code)),
                "name {name}"
            );
            // 反查只需能解析回同一码；`COLON`/`SEMICOLON` 同码，首个名字胜出。
            let back = button_name(ButtonCode(*code)).expect("reverse lookup");
            assert_eq!(button_from_name(back), Some(ButtonCode(*code)));
        }
    }

    #[test]
    fn prefixed_names_accepted() {
        assert_eq!(button_from_name("_MOUSE_L"), Some(MOUSE_LEFT));
        assert_eq!(button_from_name("MOUSE_L"), Some(MOUSE_LEFT));
        assert_eq!(button_from_name("key_enter"), Some(ENTER));
        assert_eq!(button_from_name("F5"), Some(ButtonCode(0x74)));
    }

    #[test]
    fn unknown_keys_are_none() {
        assert_eq!(Key::Char('é').to_button(), None);
        assert_eq!(Key::F(0).to_button(), None);
        assert_eq!(Key::Numpad(10).to_button(), None);
        assert_eq!(Key::Char('(').to_button(), None);
    }
}
