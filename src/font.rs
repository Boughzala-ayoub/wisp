//! Font loading, cell metrics and glyph rasterization with DirectWrite.
//!
//! Glyphs are rasterized on the CPU into 8-bit coverage masks with
//! `IDWriteGlyphRunAnalysis`, the same grayscale antialiasing Windows uses for
//! text, then uploaded into the renderer's GPU atlas.

use std::collections::HashMap;

use windows::Win32::Foundation::{FALSE, RECT};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_METRICS, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE,
    DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_GLYPH_METRICS, DWRITE_GLYPH_RUN, DWRITE_GRID_FIT_MODE_DEFAULT,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    DWRITE_TEXTURE_ALIASED_1x1, DWriteCreateFactory, IDWriteFactory2, IDWriteFontCollection, IDWriteFontFace,
};
use windows::core::{HSTRING, Result};

/// Index into [`Font`]'s faces: bit 0 is bold, bit 1 is italic.
pub type FaceStyle = usize;
pub const BOLD: FaceStyle = 1;
pub const ITALIC: FaceStyle = 2;

/// A grayscale glyph mask positioned relative to the pen on the baseline.
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Offset from the pen position to the bitmap's left edge, in pixels.
    pub left: i32,
    /// Offset from the baseline to the bitmap's top edge; negative is above.
    pub top: i32,
    /// `width * height` coverage values.
    pub coverage: Vec<u8>,
}

/// One monospace font family at one pixel size, with its four styles.
pub struct Font {
    factory: IDWriteFactory2,
    faces: [IDWriteFontFace; 4],
    /// The em size in pixels: point size scaled by DPI.
    pub em_px: f32,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Distance from the top of a cell to the baseline.
    pub baseline: u32,
    /// Distance from the top of a cell to the top of the underline.
    pub underline_y: u32,
    pub underline_thickness: u32,
    /// Distance from the top of a cell to the top of the strikethrough.
    pub strikethrough_y: u32,
    glyph_ids: HashMap<(char, FaceStyle), u16>,
}

impl Font {
    /// Loads `family` from the installed fonts, falling back to Consolas.
    pub fn new(family: &str, size_pt: f32, dpi: u32) -> Result<Font> {
        unsafe {
            let factory: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut collection: Option<IDWriteFontCollection> = None;
            factory.GetSystemFontCollection(&mut collection, false)?;
            let collection = collection.expect("GetSystemFontCollection returned no collection");

            let mut index = 0;
            let mut exists = FALSE;
            for name in [family, "Consolas"] {
                collection.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists)?;
                if exists.as_bool() {
                    break;
                }
            }
            let family = collection.GetFontFamily(index)?;
            let face = |weight: DWRITE_FONT_WEIGHT, style: DWRITE_FONT_STYLE| -> Result<IDWriteFontFace> {
                family.GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style)?.CreateFontFace()
            };
            let faces = [
                face(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL)?,
                face(DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL)?,
                face(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC)?,
                face(DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_ITALIC)?,
            ];

            let mut m = DWRITE_FONT_METRICS::default();
            faces[0].GetMetrics(&mut m);
            let em_px = size_pt * dpi as f32 / 72.0;
            let px = |design_units: f32| design_units * em_px / m.designUnitsPerEm as f32;

            // Every glyph in a monospace font has the same advance; measure 'M'.
            let mut m_glyph = 0u16;
            faces[0].GetGlyphIndices(&('M' as u32), 1, &mut m_glyph)?;
            let mut gm = DWRITE_GLYPH_METRICS::default();
            faces[0].GetDesignGlyphMetrics(&m_glyph, 1, &mut gm, false)?;

            let ascent = px(m.ascent as f32);
            let descent = px(m.descent as f32);
            let line_gap = px(m.lineGap as f32);
            let cell_height = (ascent + descent + line_gap).round().max(1.0);
            let baseline = (ascent + line_gap / 2.0).round();
            // DirectWrite measures these upward from the baseline.
            let underline_thickness = px(m.underlineThickness as f32).round().max(1.0);
            let underline_y = (baseline - px(m.underlinePosition as f32)).round();
            let strikethrough_y = (baseline - px(m.strikethroughPosition as f32)).round();

            Ok(Font {
                factory,
                faces,
                em_px,
                cell_width: px(gm.advanceWidth as f32).round().max(1.0) as u32,
                cell_height: cell_height as u32,
                baseline: baseline as u32,
                underline_y: (underline_y as u32).min(cell_height as u32 - 1),
                underline_thickness: underline_thickness as u32,
                strikethrough_y: strikethrough_y as u32,
                glyph_ids: HashMap::new(),
            })
        }
    }

    /// The glyph index for `ch` in the given style, or 0 (the font's "missing
    /// glyph" box) when the font has no glyph for it.
    pub fn glyph_index(&mut self, ch: char, style: FaceStyle) -> u16 {
        let face = &self.faces[style];
        *self.glyph_ids.entry((ch, style)).or_insert_with(|| {
            let mut glyph = 0u16;
            unsafe { face.GetGlyphIndices(&(ch as u32), 1, &mut glyph) }.map_or(0, |()| glyph)
        })
    }

    /// Rasterizes one glyph. Returns `None` for glyphs with no ink, like space.
    pub fn rasterize(&self, glyph: u16, style: FaceStyle) -> Result<Option<GlyphBitmap>> {
        let advance = 0.0f32;
        let run = DWRITE_GLYPH_RUN {
            // SAFETY: borrows the face for the duration of this call. ManuallyDrop
            // in the struct means the copied pointer is never released.
            fontFace: unsafe { std::mem::transmute_copy(&self.faces[style]) },
            fontEmSize: self.em_px,
            glyphCount: 1,
            glyphIndices: &glyph,
            glyphAdvances: &advance,
            glyphOffsets: std::ptr::null(),
            isSideways: FALSE,
            bidiLevel: 0,
        };
        unsafe {
            let analysis = self.factory.CreateGlyphRunAnalysis(
                &run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                0.0,
                0.0,
            )?;
            // With grayscale antialiasing, the "aliased" texture type is the
            // 8-bit-per-pixel coverage mask rather than a 1-bit one.
            let bounds: RECT = analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)?;
            let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
            if width <= 0 || height <= 0 {
                return Ok(None);
            }
            let mut coverage = vec![0u8; (width * height) as usize];
            analysis.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &bounds, &mut coverage)?;
            Ok(Some(GlyphBitmap {
                width: width as u32,
                height: height as u32,
                left: bounds.left,
                top: bounds.top,
                coverage,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascadia_metrics_are_sane() {
        let font = Font::new("Cascadia Code", 12.0, 96).unwrap();
        // 12pt at 96 DPI is a 16px em; Cascadia's advance is 0.6 em.
        assert!((9..=10).contains(&font.cell_width), "cell width {}", font.cell_width);
        assert!((18..=23).contains(&font.cell_height), "cell height {}", font.cell_height);
        assert!(font.baseline < font.cell_height);
        assert!(font.underline_y < font.cell_height);
    }

    #[test]
    fn glyphs_are_antialiased_and_spaces_are_empty() {
        let mut font = Font::new("Cascadia Code", 12.0, 96).unwrap();
        let a = font.glyph_index('A', 0);
        assert_ne!(a, 0);
        let bmp = font.rasterize(a, 0).unwrap().unwrap();
        assert!(bmp.top < 0, "glyph should sit above the baseline");
        assert!(bmp.coverage.iter().any(|&c| c > 0 && c < 255), "expected grayscale edges");
        let space = font.glyph_index(' ', 0);
        assert!(font.rasterize(space, 0).unwrap().is_none());
    }

    #[test]
    fn metrics_scale_with_dpi() {
        let at_96 = Font::new("Cascadia Code", 12.0, 96).unwrap();
        let at_144 = Font::new("Cascadia Code", 12.0, 144).unwrap();
        let ratio = at_144.cell_height as f32 / at_96.cell_height as f32;
        assert!((1.4..=1.6).contains(&ratio), "150% scaling changed cell height by {ratio}");
    }

    #[test]
    fn missing_family_falls_back() {
        let font = Font::new("No Such Font 12345", 12.0, 96).unwrap();
        assert!(font.cell_width > 0);
    }
}
