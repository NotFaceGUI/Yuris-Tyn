//! 字形缓存与纹理图集(fontdue)。
//!
//! 白色 coverage 图集(R8Unorm),上色由顶点色负责;行式装箱,整图放满时
//! 清空重来(调试 HUD 的字符集很小,清空极罕见)。

use fontdue::{Font, FontSettings};
use std::collections::HashMap;

/// 图集尺寸(R8;1024×512 对 13~28px 的 CJK/ASCII 足以容纳数千字形)。
pub const ATLAS_W: u32 = 1024;
pub const ATLAS_H: u32 = 512;

/// 一个已装箱字形的排版信息(像素;屏幕坐标 y 向下)。
#[derive(Debug, Clone, Copy)]
pub struct SlotInfo {
    /// 图集内左上角与尺寸。
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// 相对笔位的横向位移(fontdue `xmin`)。
    pub xmin: i32,
    /// 相对基线的纵向位移(fontdue `ymin`,向上为正)。
    pub ymin: i32,
    /// 水平推进量。
    pub advance: f32,
}

pub struct TextCache {
    font: Option<Font>,
    cache: HashMap<(u32, u16), SlotInfo>,
    pub atlas: Vec<u8>,
    pen_x: u32,
    pen_y: u32,
    row_h: u32,
    pub dirty: bool,
}

impl TextCache {
    pub fn new(font: Option<Font>) -> Self {
        Self {
            font,
            cache: HashMap::new(),
            atlas: vec![0; (ATLAS_W * ATLAS_H) as usize],
            pen_x: 1,
            pen_y: 1,
            row_h: 0,
            dirty: true,
        }
    }

    pub fn has_font(&self) -> bool {
        self.font.is_some()
    }

    /// (ascent, descent):向上为正,随像素尺寸缩放。
    pub fn h_metrics(&self, px: f32) -> (f32, f32) {
        match &self.font {
            Some(f) => f
                .horizontal_line_metrics(px)
                .map(|m| (m.ascent, m.descent))
                .unwrap_or((px * 0.8, -px * 0.2)),
            None => (px * 0.8, -px * 0.2),
        }
    }

    pub fn advance_of(&self, ch: char, px: f32) -> f32 {
        let Some(f) = &self.font else { return px * 0.55 };
        if !f.has_glyph(ch) {
            return px * 0.55;
        }
        f.metrics(ch, px).advance_width
    }

    pub fn measure(&self, s: &str, px: f32) -> f32 {
        s.chars().map(|c| self.advance_of(c, px)).sum()
    }

    /// 取(必要时栅格化装箱)字形;无字体或查无此字形返回 None(调用方
    /// 以等宽占位推进,保证布局不塌)。
    pub fn slot(&mut self, ch: char, px: u16) -> Option<SlotInfo> {
        let key = (ch as u32, px);
        if let Some(s) = self.cache.get(&key) {
            return Some(*s);
        }
        let font = self.font.as_ref()?;
        let idx = font.lookup_glyph_index(ch);
        if idx == 0 && !font.has_glyph(ch) {
            return None;
        }
        let (m, bmp) = font.rasterize_indexed(idx, px as f32);
        let w = m.width as u32;
        let h = m.height as u32;
        if w == 0 || h == 0 {
            // 空白字形(空格等):只记排版信息。
            let s = SlotInfo { x: 0, y: 0, w: 0, h: 0, xmin: m.xmin, ymin: m.ymin, advance: m.advance_width };
            self.cache.insert(key, s);
            return Some(s);
        }
        if self.pen_x + w + 1 > ATLAS_W {
            self.pen_x = 1;
            self.pen_y += self.row_h + 1;
            self.row_h = 0;
        }
        if self.pen_y + h + 1 > ATLAS_H {
            // 图集满:清空重来(字符集小,重启成本可忽略)。
            self.cache.clear();
            self.atlas.fill(0);
            self.pen_x = 1;
            self.pen_y = 1;
            self.row_h = 0;
        }
        for row in 0..h {
            let src = &bmp[(row * w) as usize..((row + 1) * w) as usize];
            let dst = ((self.pen_y + row) * ATLAS_W + self.pen_x) as usize;
            self.atlas[dst..dst + w as usize].copy_from_slice(src);
        }
        let s = SlotInfo {
            x: self.pen_x as u16,
            y: self.pen_y as u16,
            w: w as u16,
            h: h as u16,
            xmin: m.xmin,
            ymin: m.ymin,
            advance: m.advance_width,
        };
        self.pen_x += w + 1;
        self.row_h = self.row_h.max(h);
        self.cache.insert(key, s);
        self.dirty = true;
        Some(s)
    }
}

/// 从候选字体文件装第一个可解析的(与播放器的字体链同优先序:繁中优先)。
pub fn load_font_from_candidates(candidates: &[&str]) -> Option<Font> {
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(f) = Font::from_bytes(
                bytes,
                FontSettings { collection_index: 0, scale: 40.0, load_substitutions: true },
            ) {
                return Some(f);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_font_layout_stays_sane() {
        let mut tc = TextCache::new(None);
        assert!(!tc.has_font());
        // 无字体:measure 走等宽占位,slot 返回 None,布局不塌。
        assert!(tc.measure("VM 執行時序 60", 14.0) > 0.0);
        assert!(tc.slot('V', 14).is_none());
    }
}
