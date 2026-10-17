//! Fonts with DirectWrite: cell metrics, shaping, fallback and rasterization.
//!
//! Text is shaped one run at a time, so the font's contextual alternates and
//! ligatures (`calt`, `liga`) apply. Characters the configured font lacks go
//! through the system font fallback, the same one Windows uses everywhere.
//! Glyphs are rasterized on the CPU, either as 8-bit coverage masks or, for
//! color fonts like Segoe UI Emoji, as premultiplied RGBA.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::Win32::Foundation::{FALSE, RECT};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{HRESULT, HSTRING, Interface, PCWSTR, Ref, Result, implement, w};

/// Index into [`Font`]'s faces. The first four are the configured family's
/// regular, bold, italic and bold italic; fallback faces follow as found.
pub type FaceId = u16;
pub const BOLD: FaceId = 1;
pub const ITALIC: FaceId = 2;

const LOCALE: PCWSTR = w!("en-us");

/// A run of text in one style, the shaping cache key.
type RunKey = (FaceId, Box<[u16]>);
const DWRITE_E_NOCOLOR: HRESULT = HRESULT(0x8898_500C_u32 as i32);

/// A positioned glyph from [`Font::shape`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub face: FaceId,
    pub glyph: u16,
    /// UTF-16 index (into the shaped text) of the first character of the
    /// cluster this glyph belongs to. The renderer maps it back to a cell.
    pub cluster: u32,
    /// Offset from the cluster's pen position, in pixels; `y` points up.
    pub x: f32,
    pub y: f32,
}

pub enum Pixels {
    /// One coverage byte per pixel, tinted with the text color when drawn.
    Coverage(Vec<u8>),
    /// Premultiplied RGBA, drawn as is.
    Color(Vec<u8>),
}

/// A rasterized glyph positioned relative to the pen on the baseline.
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Offset from the pen position to the bitmap's left edge, in pixels.
    pub left: i32,
    /// Offset from the baseline to the bitmap's top edge; negative is above.
    pub top: i32,
    pub pixels: Pixels,
}

struct Face {
    face: IDWriteFontFace,
    font: IDWriteFont3,
    color: bool,
}

/// One monospace font family at one pixel size, plus whatever fallback
/// fonts its text has needed so far.
pub struct Font {
    factory: IDWriteFactory2,
    analyzer: IDWriteTextAnalyzer,
    fallback: IDWriteFontFallback,
    collection: IDWriteFontCollection,
    family: HSTRING,
    faces: Vec<Face>,
    /// OpenType features from `font-feature`, applied on top of the font's defaults.
    features: Vec<DWRITE_FONT_FEATURE>,
    /// Shaped runs by style and text. Terminal text repeats a lot (prompts,
    /// borders, the same rows redrawn every frame) and shaping is the
    /// expensive part of a frame.
    shaped: HashMap<RunKey, Box<[ShapedGlyph]>>,
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
}

impl Font {
    /// Uses the first installed family in `families` (Consolas if none is) and
    /// tries the installed ones after it, in order, for characters it lacks,
    /// before the system's own fallback. `features` are OpenType features like
    /// `(*b"calt", 0)`.
    pub fn new(families: &[String], size_pt: f32, dpi: u32, features: &[([u8; 4], u32)]) -> Result<Font> {
        unsafe {
            let factory: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut collection: Option<IDWriteFontCollection> = None;
            factory.GetSystemFontCollection(&mut collection, false)?;
            let collection = collection.expect("GetSystemFontCollection returned no collection");

            let installed = |name: &str| -> Option<u32> {
                let (mut index, mut exists) = (0, FALSE);
                collection.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists).ok()?;
                exists.as_bool().then_some(index)
            };
            let mut available =
                families.iter().map(String::as_str).chain(["Consolas"]).filter_map(|n| Some((n, installed(n)?)));
            let (primary, index) = available.next().expect("Consolas ships with every Windows install");
            let family_name = HSTRING::from(primary);
            let extra: Vec<HSTRING> =
                available.map(|(name, _)| HSTRING::from(name)).filter(|n| *n != family_name).collect();

            // Configured fallback families first, then everything Windows would try.
            let system_fallback = factory.GetSystemFontFallback()?;
            let fallback = if extra.is_empty() {
                system_fallback
            } else {
                let builder = factory.CreateFontFallbackBuilder()?;
                let everything = [DWRITE_UNICODE_RANGE { first: 0, last: 0x10FFFF }];
                for name in &extra {
                    builder.AddMapping(
                        &everything,
                        &[name.as_ptr()],
                        &collection,
                        PCWSTR::null(),
                        PCWSTR::null(),
                        1.0,
                    )?;
                }
                builder.AddMappings(&system_fallback)?;
                builder.CreateFontFallback()?
            };
            let family = collection.GetFontFamily(index)?;
            let mut faces = Vec::with_capacity(8);
            for face_id in 0..4 {
                let (weight, style) = weight_and_style(face_id);
                let font = family.GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, style)?;
                faces.push(Face::new(font)?);
            }

            let mut m = DWRITE_FONT_METRICS::default();
            faces[0].face.GetMetrics(&mut m);
            let em_px = size_pt * dpi as f32 / 72.0;
            let px = |design_units: f32| design_units * em_px / m.designUnitsPerEm as f32;

            // Every glyph in a monospace font has the same advance; measure 'M'.
            let mut m_glyph = 0u16;
            faces[0].face.GetGlyphIndices(&('M' as u32), 1, &mut m_glyph)?;
            let mut gm = DWRITE_GLYPH_METRICS::default();
            faces[0].face.GetDesignGlyphMetrics(&m_glyph, 1, &mut gm, false)?;

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
                analyzer: factory.CreateTextAnalyzer()?,
                fallback,
                factory,
                collection,
                family: family_name,
                faces,
                features: features
                    .iter()
                    .map(|&(tag, value)| DWRITE_FONT_FEATURE {
                        nameTag: DWRITE_FONT_FEATURE_TAG(u32::from_le_bytes(tag)),
                        parameter: value,
                    })
                    .collect(),
                shaped: HashMap::new(),
                em_px,
                cell_width: px(gm.advanceWidth as f32).round().max(1.0) as u32,
                cell_height: cell_height as u32,
                baseline: baseline as u32,
                underline_y: (underline_y as u32).min(cell_height as u32 - 1),
                underline_thickness: underline_thickness as u32,
                strikethrough_y: strikethrough_y as u32,
            })
        }
    }

    /// Shapes `text` in the given style (a combination of [`BOLD`] and
    /// [`ITALIC`]), appending the glyphs to `out` in visual order.
    pub fn shape(&mut self, text: &[u16], style: FaceId, out: &mut Vec<ShapedGlyph>) -> Result<()> {
        let key = (style, Box::from(text));
        if let Some(glyphs) = self.shaped.get(&key) {
            out.extend_from_slice(glyphs);
            return Ok(());
        }
        let start = out.len();
        self.shape_uncached(text, style, out)?;
        // ponytail: dropping everything at a size cap is crude; an LRU if memory or hit rate disappoint.
        if self.shaped.len() >= 8192 {
            self.shaped.clear();
        }
        self.shaped.insert(key, out[start..].into());
        Ok(())
    }

    fn shape_uncached(&mut self, text: &[u16], style: FaceId, out: &mut Vec<ShapedGlyph>) -> Result<()> {
        let len = text.len() as u32;
        if len == 0 {
            return Ok(());
        }
        let source: IDWriteTextAnalysisSource = TextSource { text: text.to_vec() }.into();
        let scripts = Rc::new(RefCell::new(Vec::new()));
        let sink: IDWriteTextAnalysisSink = ScriptSink { runs: scripts.clone() }.into();
        unsafe { self.analyzer.AnalyzeScript(&source, 0, len, &sink)? };
        let scripts = scripts.take();

        let (weight, slant) = weight_and_style(style);
        let mut pos = 0;
        while pos < len {
            let (mut mapped_len, mut mapped, mut scale) = (0, None, 1.0);
            unsafe {
                self.fallback.MapCharacters(
                    &source,
                    pos,
                    len - pos,
                    &self.collection,
                    &self.family,
                    weight,
                    slant,
                    DWRITE_FONT_STRETCH_NORMAL,
                    &mut mapped_len,
                    &mut mapped,
                    &mut scale,
                )?;
            }
            let mapped_len = mapped_len.max(1);
            // No installed font covers these characters; the font's "missing
            // glyph" box is the honest thing to draw.
            let face = match mapped {
                Some(font) => self.face_for(font)?,
                None => style,
            };
            for &(start, length, analysis) in &scripts {
                let (from, to) = (start.max(pos), (start + length).min(pos + mapped_len));
                if from < to {
                    self.shape_range(text, from as usize, to as usize, face, analysis, out)?;
                }
            }
            pos += mapped_len;
        }
        Ok(())
    }

    fn shape_range(
        &self,
        text: &[u16],
        from: usize,
        to: usize,
        face_id: FaceId,
        script: DWRITE_SCRIPT_ANALYSIS,
        out: &mut Vec<ShapedGlyph>,
    ) -> Result<()> {
        let chars = &text[from..to];
        let n = chars.len();
        let face = &self.faces[face_id as usize].face;
        // DirectWrite's documented upper bound for the glyph count.
        let max_glyphs = n * 3 / 2 + 16;
        let mut clusters = vec![0u16; n];
        let mut text_props = vec![DWRITE_SHAPING_TEXT_PROPERTIES::default(); n];
        let mut glyphs = vec![0u16; max_glyphs];
        let mut glyph_props = vec![DWRITE_SHAPING_GLYPH_PROPERTIES::default(); max_glyphs];
        let mut count = 0;
        // The font's default features (ligatures and contextual alternates
        // among them) always apply; configured ones are layered on top, over
        // one range covering the whole run.
        let typographic = DWRITE_TYPOGRAPHIC_FEATURES {
            features: self.features.as_ptr().cast_mut(),
            featureCount: self.features.len() as u32,
        };
        let typographic_ptr: *const DWRITE_TYPOGRAPHIC_FEATURES = &typographic;
        let range_length = n as u32;
        let (features, lengths, ranges) = if self.features.is_empty() {
            (None, None, 0)
        } else {
            (Some(&raw const typographic_ptr), Some(&raw const range_length), 1)
        };
        unsafe {
            self.analyzer.GetGlyphs(
                PCWSTR(chars.as_ptr()),
                n as u32,
                face,
                false,
                false,
                &script,
                LOCALE,
                None,
                features,
                lengths,
                ranges,
                max_glyphs as u32,
                clusters.as_mut_ptr(),
                text_props.as_mut_ptr(),
                glyphs.as_mut_ptr(),
                glyph_props.as_mut_ptr(),
                &mut count,
            )?;
        }
        let count = count as usize;
        let mut advances = vec![0f32; count];
        let mut offsets = vec![DWRITE_GLYPH_OFFSET::default(); count];
        unsafe {
            self.analyzer.GetGlyphPlacements(
                PCWSTR(chars.as_ptr()),
                clusters.as_ptr(),
                text_props.as_mut_ptr(),
                n as u32,
                glyphs.as_ptr(),
                glyph_props.as_ptr(),
                count as u32,
                face,
                self.em_px,
                false,
                false,
                &script,
                LOCALE,
                features,
                lengths,
                ranges,
                advances.as_mut_ptr(),
                offsets.as_mut_ptr(),
            )?;
        }

        // A cluster is a run of characters sharing the same first glyph. Its
        // glyphs are laid out left to right from the cluster's own cell.
        let mut c = 0;
        while c < n {
            let first_glyph = clusters[c] as usize;
            let mut end = c + 1;
            while end < n && clusters[end] as usize == first_glyph {
                end += 1;
            }
            let last_glyph = if end < n { clusters[end] as usize } else { count };
            let mut pen = 0.0;
            for g in first_glyph..last_glyph {
                out.push(ShapedGlyph {
                    face: face_id,
                    glyph: glyphs[g],
                    cluster: (from + c) as u32,
                    x: pen + offsets[g].advanceOffset,
                    y: offsets[g].ascenderOffset,
                });
                pen += advances[g];
            }
            c = end;
        }
        Ok(())
    }

    /// The face id for a font the fallback picked, adding it on first use.
    fn face_for(&mut self, font: IDWriteFont) -> Result<FaceId> {
        let font: IDWriteFont3 = font.cast()?;
        if let Some(i) = self.faces.iter().position(|f| unsafe { f.font.Equals(&font) }.as_bool()) {
            return Ok(i as FaceId);
        }
        self.faces.push(Face::new(font.cast()?)?);
        Ok((self.faces.len() - 1) as FaceId)
    }

    /// Rasterizes one glyph. Returns `None` for glyphs with no ink, like space.
    pub fn rasterize(&self, face_id: FaceId, glyph: u16) -> Result<Option<GlyphBitmap>> {
        let face = &self.faces[face_id as usize];
        let advance = 0.0f32;
        let run = DWRITE_GLYPH_RUN {
            // SAFETY: borrows the face for the duration of this call. ManuallyDrop
            // in the struct means the copied pointer is never released.
            fontFace: unsafe { std::mem::transmute_copy(&face.face) },
            fontEmSize: self.em_px,
            glyphCount: 1,
            glyphIndices: &glyph,
            glyphAdvances: &advance,
            glyphOffsets: std::ptr::null(),
            isSideways: FALSE,
            bidiLevel: 0,
        };
        if face.color {
            let layers = unsafe {
                self.factory.TranslateColorGlyphRun(0.0, 0.0, &run, None, DWRITE_MEASURING_MODE_NATURAL, None, 0)
            };
            match layers {
                Ok(layers) => return self.rasterize_layers(&layers),
                Err(e) if e.code() == DWRITE_E_NOCOLOR => {} // this glyph has no color version
                Err(e) => return Err(e),
            }
        }
        Ok(self.coverage(&run, 0.0, 0.0)?.map(|(bounds, coverage)| bitmap(bounds, Pixels::Coverage(coverage))))
    }

    /// Composites the color layers of a glyph (COLR fonts) into one RGBA bitmap.
    fn rasterize_layers(&self, layers: &IDWriteColorGlyphRunEnumerator) -> Result<Option<GlyphBitmap>> {
        let mut rasterized = Vec::new();
        unsafe {
            while layers.MoveNext()?.as_bool() {
                let layer = &*layers.GetCurrentRun()?;
                // Palette index 0xFFFF means "the text color"; emoji don't use it, so white will do.
                let c = layer.runColor;
                let color = if layer.paletteIndex == 0xFFFF { [1.0; 4] } else { [c.r, c.g, c.b, c.a] };
                if let Some(cov) = self.coverage(&layer.glyphRun, layer.baselineOriginX, layer.baselineOriginY)? {
                    rasterized.push((cov, color));
                }
            }
        }
        let Some(bounds) = rasterized.iter().map(|((b, _), _)| *b).reduce(|a, b| RECT {
            left: a.left.min(b.left),
            top: a.top.min(b.top),
            right: a.right.max(b.right),
            bottom: a.bottom.max(b.bottom),
        }) else {
            return Ok(None);
        };
        let width = (bounds.right - bounds.left) as usize;
        let height = (bounds.bottom - bounds.top) as usize;
        let mut rgba = vec![0f32; width * height * 4];
        for ((b, coverage), color) in &rasterized {
            let layer_width = (b.right - b.left) as usize;
            for (i, &cov) in coverage.iter().enumerate() {
                let x = (b.left - bounds.left) as usize + i % layer_width;
                let y = (b.top - bounds.top) as usize + i / layer_width;
                let a = cov as f32 / 255.0 * color[3];
                let px = &mut rgba[(y * width + x) * 4..][..4];
                // Premultiplied "over": layers are painted bottom to top.
                for ch in 0..3 {
                    px[ch] = color[ch] * a + px[ch] * (1.0 - a);
                }
                px[3] = a + px[3] * (1.0 - a);
            }
        }
        let bytes = rgba.iter().map(|v| (v * 255.0).round() as u8).collect();
        Ok(Some(bitmap(bounds, Pixels::Color(bytes))))
    }

    /// Grayscale coverage for a glyph run with its baseline origin at (x, y).
    fn coverage(&self, run: &DWRITE_GLYPH_RUN, x: f32, y: f32) -> Result<Option<(RECT, Vec<u8>)>> {
        unsafe {
            let analysis = self.factory.CreateGlyphRunAnalysis(
                run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                x,
                y,
            )?;
            // With grayscale antialiasing, the "aliased" texture type is the
            // 8-bit-per-pixel coverage mask rather than a 1-bit one.
            let bounds = analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1)?;
            let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
            if width <= 0 || height <= 0 {
                return Ok(None);
            }
            let mut coverage = vec![0u8; (width * height) as usize];
            analysis.CreateAlphaTexture(DWRITE_TEXTURE_ALIASED_1x1, &bounds, &mut coverage)?;
            Ok(Some((bounds, coverage)))
        }
    }
}

impl Face {
    fn new(font: IDWriteFont) -> Result<Face> {
        unsafe {
            let face = font.CreateFontFace()?;
            let color = face.cast::<IDWriteFontFace2>().is_ok_and(|f| f.IsColorFont().as_bool());
            Ok(Face { face, font: font.cast()?, color })
        }
    }
}

fn weight_and_style(face: FaceId) -> (DWRITE_FONT_WEIGHT, DWRITE_FONT_STYLE) {
    let weight = if face & BOLD != 0 { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
    let style = if face & ITALIC != 0 { DWRITE_FONT_STYLE_ITALIC } else { DWRITE_FONT_STYLE_NORMAL };
    (weight, style)
}

fn bitmap(bounds: RECT, pixels: Pixels) -> GlyphBitmap {
    GlyphBitmap {
        width: (bounds.right - bounds.left) as u32,
        height: (bounds.bottom - bounds.top) as u32,
        left: bounds.left,
        top: bounds.top,
        pixels,
    }
}

/// Hands DirectWrite the text being analyzed. Always left to right, US English.
#[implement(IDWriteTextAnalysisSource)]
struct TextSource {
    text: Vec<u16>,
}

impl IDWriteTextAnalysisSource_Impl for TextSource_Impl {
    fn GetTextAtPosition(&self, position: u32, text: *mut *mut u16, length: *mut u32) -> Result<()> {
        let rest = self.text.get(position as usize..).unwrap_or_default();
        unsafe {
            *text = if rest.is_empty() { std::ptr::null_mut() } else { rest.as_ptr().cast_mut() };
            *length = rest.len() as u32;
        }
        Ok(())
    }

    fn GetTextBeforePosition(&self, position: u32, text: *mut *mut u16, length: *mut u32) -> Result<()> {
        let before = &self.text[..(position as usize).min(self.text.len())];
        unsafe {
            *text = if before.is_empty() { std::ptr::null_mut() } else { before.as_ptr().cast_mut() };
            *length = before.len() as u32;
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(&self, position: u32, length: *mut u32, locale: *mut *mut u16) -> Result<()> {
        unsafe {
            *length = (self.text.len() as u32).saturating_sub(position);
            *locale = LOCALE.as_ptr().cast_mut();
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        position: u32,
        length: *mut u32,
        substitution: windows::core::OutRef<IDWriteNumberSubstitution>,
    ) -> Result<()> {
        unsafe { *length = (self.text.len() as u32).saturating_sub(position) };
        substitution.write(None)
    }
}

/// Collects the script ranges `AnalyzeScript` reports.
#[implement(IDWriteTextAnalysisSink)]
struct ScriptSink {
    runs: Rc<RefCell<Vec<(u32, u32, DWRITE_SCRIPT_ANALYSIS)>>>,
}

impl IDWriteTextAnalysisSink_Impl for ScriptSink_Impl {
    fn SetScriptAnalysis(&self, position: u32, length: u32, analysis: *const DWRITE_SCRIPT_ANALYSIS) -> Result<()> {
        self.runs.borrow_mut().push((position, length, unsafe { *analysis }));
        Ok(())
    }

    fn SetLineBreakpoints(&self, _: u32, _: u32, _: *const DWRITE_LINE_BREAKPOINT) -> Result<()> {
        Ok(())
    }

    fn SetBidiLevel(&self, _: u32, _: u32, _: u8, _: u8) -> Result<()> {
        Ok(())
    }

    fn SetNumberSubstitution(&self, _: u32, _: u32, _: Ref<IDWriteNumberSubstitution>) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn font() -> Font {
        Font::new(&["Cascadia Code".into()], 12.0, 96, &[]).unwrap()
    }

    #[test]
    fn cascadia_metrics_are_sane() {
        let font = font();
        // 12pt at 96 DPI is a 16px em; Cascadia's advance is 0.6 em.
        assert!((9..=10).contains(&font.cell_width), "cell width {}", font.cell_width);
        assert!((18..=23).contains(&font.cell_height), "cell height {}", font.cell_height);
        assert!(font.baseline < font.cell_height);
        assert!(font.underline_y < font.cell_height);
    }

    #[test]
    fn metrics_scale_with_dpi() {
        let at_96 = font();
        let at_144 = Font::new(&["Cascadia Code".into()], 12.0, 144, &[]).unwrap();
        let ratio = at_144.cell_height as f32 / at_96.cell_height as f32;
        assert!((1.4..=1.6).contains(&ratio), "150% scaling changed cell height by {ratio}");
    }

    #[test]
    fn missing_family_falls_back() {
        let font = Font::new(&["No Such Font 12345".into()], 12.0, 96, &[]).unwrap();
        assert!(font.cell_width > 0);
    }

    #[test]
    fn plain_text_is_one_glyph_per_cell() {
        let mut font = font();
        let mut glyphs = Vec::new();
        font.shape(&utf16("Wisp"), 0, &mut glyphs).unwrap();
        assert_eq!(glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert!(glyphs.iter().all(|g| g.face == 0 && g.glyph != 0));
    }

    #[test]
    fn ligatures_change_the_glyphs() {
        // Cascadia Code draws "=>" with contextual alternates, so the glyphs
        // differ from those of "=" and ">" shaped on their own.
        let mut font = font();
        let (mut joined, mut apart) = (Vec::new(), Vec::new());
        font.shape(&utf16("=>"), 0, &mut joined).unwrap();
        font.shape(&utf16("="), 0, &mut apart).unwrap();
        font.shape(&utf16(">"), 0, &mut apart).unwrap();
        let ids = |g: &[ShapedGlyph]| g.iter().map(|g| g.glyph).collect::<Vec<_>>();
        assert_ne!(ids(&joined), ids(&apart));
    }

    #[test]
    fn font_features_can_turn_ligatures_off() {
        let ids = |features: &[([u8; 4], u32)]| {
            let mut font = Font::new(&["Cascadia Code".into()], 12.0, 96, features).unwrap();
            let mut glyphs = Vec::new();
            font.shape(&utf16("=>"), 0, &mut glyphs).unwrap();
            glyphs.iter().map(|g| g.glyph).collect::<Vec<_>>()
        };
        let mut plain = Font::new(&["Cascadia Code".into()], 12.0, 96, &[]).unwrap();
        let mut apart = Vec::new();
        plain.shape(&utf16("="), 0, &mut apart).unwrap();
        plain.shape(&utf16(">"), 0, &mut apart).unwrap();
        let apart: Vec<u16> = apart.iter().map(|g| g.glyph).collect();
        assert_ne!(ids(&[]), apart, "ligatures on by default");
        assert_eq!(ids(&[(*b"calt", 0)]), apart, "font-feature = -calt turns them off");
    }

    #[test]
    fn configured_fallback_families_dont_hide_the_system_fallback() {
        // Consolas comes before the system fallback but has no emoji, so the
        // emoji must still come from Segoe UI Emoji and render in color.
        let mut font = Font::new(&["Cascadia Code".into(), "Consolas".into()], 12.0, 96, &[]).unwrap();
        let mut glyphs = Vec::new();
        font.shape(&utf16("😀"), 0, &mut glyphs).unwrap();
        let emoji = font.rasterize(glyphs[0].face, glyphs[0].glyph).unwrap().unwrap();
        assert!(matches!(emoji.pixels, Pixels::Color(_)));
    }

    #[test]
    fn emoji_and_cjk_come_from_fallback_fonts() {
        let mut font = font();
        let mut glyphs = Vec::new();
        font.shape(&utf16("a😀中"), 0, &mut glyphs).unwrap();
        // 😀 is a surrogate pair, so 中 starts at UTF-16 index 3.
        let clusters: Vec<u32> = glyphs.iter().map(|g| g.cluster).collect();
        assert_eq!(clusters, [0, 1, 3]);
        assert_eq!(glyphs[0].face, 0);
        assert!(glyphs[1].face >= 4 && glyphs[2].face >= 4, "{glyphs:?}");
        assert!(glyphs.iter().all(|g| g.glyph != 0));
        let emoji = font.rasterize(glyphs[1].face, glyphs[1].glyph).unwrap().unwrap();
        let Pixels::Color(rgba) = emoji.pixels else { panic!("emoji should rasterize in color") };
        assert!(
            rgba.chunks(4).any(|p| p[3] > 200 && (p[0] as i32 - p[2] as i32).abs() > 60),
            "expected colored pixels"
        );
    }

    #[test]
    fn glyphs_are_antialiased_and_spaces_are_empty() {
        let mut font = font();
        let mut glyphs = Vec::new();
        font.shape(&utf16("A "), 0, &mut glyphs).unwrap();
        let a = font.rasterize(0, glyphs[0].glyph).unwrap().unwrap();
        assert!(a.top < 0, "glyph should sit above the baseline");
        let Pixels::Coverage(coverage) = a.pixels else { panic!("text should rasterize as coverage") };
        assert!(coverage.iter().any(|&c| c > 0 && c < 255), "expected grayscale edges");
        assert!(font.rasterize(0, glyphs[1].glyph).unwrap().is_none());
    }
}
