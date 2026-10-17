//! Direct3D 11 renderer. A frame is a list of quads drawn in one instanced
//! call: solid rectangles (cell backgrounds, cursor, lines), glyphs cut from
//! one of two atlases, and underline patterns computed in the pixel shader.

use std::collections::HashMap;
use std::ffi::c_void;

use libghostty_vt::render::{CursorVisualStyle, Dirty};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, StyleColor, Underline};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, TRUE};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32_UINT, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32G32B32A32_FLOAT,
    DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain2,
};
use windows::core::{Interface, PCSTR, Result, s};

use crate::font::{BOLD, FaceId, Font, ITALIC, Pixels, ShapedGlyph};
use crate::sprite;
use crate::term::Pane;

const ATLAS_SIZE: u32 = 2048;
/// Glyph cache key face for characters drawn by [`sprite`] rather than a font.
const SPRITE_FACE: FaceId = FaceId::MAX;

// Quad kinds; keep in sync with the shader.
const SOLID: u32 = 0;
const GLYPH: u32 = 1;
const COLOR_GLYPH: u32 = 2;
const CURLY: u32 = 3;
const DOTTED: u32 = 4;
const DASHED: u32 = 5;

const SHADER: &str = r#"
cbuffer Globals : register(b0) { float2 pixel_to_clip; };
Texture2D<float> coverage_atlas : register(t0);
Texture2D<float4> color_atlas : register(t1);

struct Quad {
    float4 rect : RECT;          // x, y, width, height in pixels
    float2 uv : UV;              // top-left texel in the atlas (glyphs)
    float4 color : COLOR;        // straight alpha
    uint kind : KIND;
    float param : PARAM;         // pattern period (underlines)
};
struct Pixel {
    float4 pos : SV_Position;
    float2 uv : UV;
    nointerpolation float4 color : COLOR;
    nointerpolation float4 rect : RECT;
    nointerpolation uint kind : KIND;
    nointerpolation float param : PARAM;
};

Pixel vs_main(Quad q, uint id : SV_VertexID) {
    float2 corner = float2(id & 1, id >> 1);
    float2 p = q.rect.xy + corner * q.rect.zw;
    Pixel o;
    o.pos = float4(p.x * pixel_to_clip.x - 1, 1 - p.y * pixel_to_clip.y, 0, 1);
    o.uv = q.uv + corner * q.rect.zw;
    o.color = q.color;
    o.rect = q.rect;
    o.kind = q.kind;
    o.param = q.param;
    return o;
}

// Coverage is blended in gamma space, which makes light text on a dark
// background look thinner than the same text dark on light. DirectWrite
// counters that with an "enhanced contrast" curve; we scale its strength with
// the text's brightness.
float enhance_contrast(float coverage, float3 color) {
    float k = lerp(0.2, 1.4, dot(color, float3(0.2126, 0.7152, 0.0722)));
    return coverage * (k + 1) / (coverage * k + 1);
}

float4 ps_main(Pixel p) : SV_Target {
    float a = p.color.a;
    if (p.kind == 1) {
        a *= enhance_contrast(coverage_atlas.Load(int3(p.uv, 0)), p.color.rgb);
    } else if (p.kind == 2) {
        return color_atlas.Load(int3(p.uv, 0)) * a;
    } else if (p.kind == 3) {
        // A sine wave through the middle of the band, phased by absolute x so
        // it continues smoothly from one cell to the next.
        float thickness = max(1, p.rect.w / 3);
        float amplitude = (p.rect.w - thickness) / 2;
        float center = p.rect.y + p.rect.w / 2 + amplitude * sin(p.pos.x * 6.2831853 / p.param);
        a *= saturate(thickness / 2 + 0.5 - abs(p.pos.y - center));
    } else if (p.kind == 4) {
        a *= fmod(floor(p.pos.x), 2 * p.param) < p.param ? 1 : 0;
    } else if (p.kind == 5) {
        a *= fmod(floor(p.pos.x), p.param) < p.param * 0.6 ? 1 : 0;
    }
    return float4(p.color.rgb * a, a); // premultiplied
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct Quad {
    rect: [f32; 4],
    uv: [f32; 2],
    color: [f32; 4],
    kind: u32,
    param: f32,
}

/// Where a rasterized glyph lives in its atlas.
#[derive(Clone, Copy)]
struct Slot {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    color: bool,
}

/// Packs glyphs left to right in rows ("shelves"), opening a new row when one fills.
#[derive(Default)]
struct Shelves {
    x: u32,
    y: u32,
    row_height: u32,
}

impl Shelves {
    fn alloc(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if self.x + width > ATLAS_SIZE {
            self.y += self.row_height;
            self.x = 0;
            self.row_height = 0;
        }
        if self.y + height > ATLAS_SIZE || width > ATLAS_SIZE {
            return None;
        }
        let at = (self.x, self.y);
        // One texel of padding keeps neighbours from bleeding into each other.
        self.x += width + 1;
        self.row_height = self.row_height.max(height + 1);
        Some(at)
    }
}

/// A glyph atlas texture and its packing state.
struct Atlas {
    texture: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
    shelves: Shelves,
    bytes_per_pixel: u32,
}

impl Atlas {
    fn new(device: &ID3D11Device, format: DXGI_FORMAT, bytes_per_pixel: u32) -> Result<Atlas> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: ATLAS_SIZE,
            Height: ATLAS_SIZE,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        unsafe {
            let mut texture = None;
            device.CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture = texture.unwrap();
            let mut view = None;
            device.CreateShaderResourceView(&texture, None, Some(&mut view))?;
            Ok(Atlas { texture, view: view.unwrap(), shelves: Shelves::default(), bytes_per_pixel })
        }
    }

    /// Copies a `width` x `height` image in, returning where it went.
    fn upload(&mut self, context: &ID3D11DeviceContext, width: u32, height: u32, data: &[u8]) -> Option<(u32, u32)> {
        let (x, y) = self.shelves.alloc(width, height)?;
        let region = D3D11_BOX { left: x, top: y, front: 0, right: x + width, bottom: y + height, back: 1 };
        unsafe {
            context.UpdateSubresource(
                &self.texture,
                0,
                Some(&region),
                data.as_ptr().cast(),
                width * self.bytes_per_pixel,
                0,
            );
        }
        Some((x, y))
    }
}

/// What a frame needs from one cell after the libghostty walk.
struct CellPaint {
    column: u16,
    /// 2 for wide characters.
    width: u16,
    /// UTF-16 range of this cell's text in the row's text buffer.
    text: std::ops::Range<usize>,
    face: FaceId,
    fg: [f32; 4],
    underline: Underline,
    underline_color: [f32; 4],
    strikethrough: bool,
    /// Box drawing or a block element, drawn by `sprite` to fill the cell exactly.
    sprite: Option<char>,
}

/// What the window needs to know about the frame it just queued.
pub struct FrameInfo {
    pub background: RgbColor,
    /// The cursor is in a blinking mode, so the window should run the blink timer.
    pub cursor_blinks: bool,
}

pub struct Renderer {
    pub device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    globals: ID3D11Buffer,
    coverage_atlas: Atlas,
    color_atlas: Atlas,
    /// `None` marks glyphs with no ink (spaces) so they aren't rasterized again.
    glyphs: HashMap<(FaceId, u16), Option<Slot>>,
    atlas_full: bool,
    quads: Vec<Quad>,
    quad_buffer: Option<(ID3D11Buffer, usize)>,
    // Scratch buffers reused across rows and frames.
    row_cells: Vec<CellPaint>,
    row_text: Vec<u16>,
    unit_cell: Vec<usize>,
    shaped: Vec<ShapedGlyph>,
}

impl Renderer {
    /// Uses the GPU, or WARP (Direct3D's software rasterizer) if there is none.
    pub fn new() -> Result<Renderer> {
        Self::with_driver(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| Self::with_driver(D3D_DRIVER_TYPE_WARP))
    }

    pub fn with_driver(driver: D3D_DRIVER_TYPE) -> Result<Renderer> {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
            let device: ID3D11Device = device.unwrap();

            let vs_code = compile(s!("vs_main"), s!("vs_4_0"))?;
            let ps_code = compile(s!("ps_main"), s!("ps_4_0"))?;
            let mut vertex_shader = None;
            device.CreateVertexShader(blob_bytes(&vs_code), None, Some(&mut vertex_shader))?;
            let mut pixel_shader = None;
            device.CreatePixelShader(blob_bytes(&ps_code), None, Some(&mut pixel_shader))?;

            let element = |name: PCSTR, format: DXGI_FORMAT, offset: u32| D3D11_INPUT_ELEMENT_DESC {
                SemanticName: name,
                SemanticIndex: 0,
                Format: format,
                InputSlot: 0,
                AlignedByteOffset: offset,
                InputSlotClass: D3D11_INPUT_PER_INSTANCE_DATA,
                InstanceDataStepRate: 1,
            };
            let elements = [
                element(s!("RECT"), DXGI_FORMAT_R32G32B32A32_FLOAT, 0),
                element(s!("UV"), DXGI_FORMAT_R32G32_FLOAT, 16),
                element(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, 24),
                element(s!("KIND"), DXGI_FORMAT_R32_UINT, 40),
                element(s!("PARAM"), DXGI_FORMAT_R32_FLOAT, 44),
            ];
            let mut layout = None;
            device.CreateInputLayout(&elements, blob_bytes(&vs_code), Some(&mut layout))?;

            let mut blend_desc = D3D11_BLEND_DESC::default();
            blend_desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: TRUE,
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut blend = None;
            device.CreateBlendState(&blend_desc, Some(&mut blend))?;

            // The y flip in the vertex shader reverses winding; draw both faces.
            let raster_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: TRUE,
                ..Default::default()
            };
            let mut raster = None;
            device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;

            let globals_desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut globals = None;
            device.CreateBuffer(&globals_desc, None, Some(&mut globals))?;

            Ok(Renderer {
                coverage_atlas: Atlas::new(&device, DXGI_FORMAT_R8_UNORM, 1)?,
                color_atlas: Atlas::new(&device, DXGI_FORMAT_R8G8B8A8_UNORM, 4)?,
                device,
                context: context.unwrap(),
                vertex_shader: vertex_shader.unwrap(),
                pixel_shader: pixel_shader.unwrap(),
                layout: layout.unwrap(),
                blend: blend.unwrap(),
                raster: raster.unwrap(),
                globals: globals.unwrap(),
                glyphs: HashMap::new(),
                atlas_full: false,
                quads: Vec::new(),
                quad_buffer: None,
                row_cells: Vec::new(),
                row_text: Vec::new(),
                unit_cell: Vec::new(),
                shaped: Vec::new(),
            })
        }
    }

    /// Forgets every cached glyph, e.g. after the font or DPI changed.
    pub fn clear_glyphs(&mut self) {
        self.glyphs.clear();
        self.coverage_atlas.shelves = Shelves::default();
        self.color_atlas.shelves = Shelves::default();
        self.atlas_full = false;
    }

    /// Queues a pane's visible screen with its top-left corner at `origin`.
    /// `blink_on` is the blink phase; a blinking cursor hides while it's false.
    pub fn draw_pane(
        &mut self,
        pane: &mut Pane,
        font: &mut Font,
        (ox, oy): (f32, f32),
        focused: bool,
        blink_on: bool,
    ) -> std::result::Result<FrameInfo, libghostty_vt::Error> {
        if self.atlas_full {
            // Glyphs queued last frame were dropped; start over with an empty atlas.
            self.clear_glyphs();
        }
        let Pane { terminal, render_state, row_iter, cell_iter, .. } = pane;
        let snapshot = render_state.update(terminal)?;
        let colors = snapshot.colors()?;
        let (cw, ch) = (font.cell_width as f32, font.cell_height as f32);

        let cursor_blinks = snapshot.cursor_blinking()?;
        let cursor_shown = snapshot.cursor_visible()? && (blink_on || !cursor_blinks || !focused);
        let cursor = if cursor_shown { snapshot.cursor_viewport()? } else { None };
        let cursor_style = if focused { snapshot.cursor_visual_style()? } else { CursorVisualStyle::BlockHollow };
        let cursor_color = rgba(colors.cursor.unwrap_or(colors.foreground), 1.0);
        let block_at = cursor.filter(|_| cursor_style == CursorVisualStyle::Block).map(|c| (c.x, c.y));
        if let Some((x, y)) = block_at {
            self.solid(ox + x as f32 * cw, oy + y as f32 * ch, cw, ch, cursor_color);
        }
        // Ligatures don't form across the cursor, so text being edited stays legible.
        let cursor_cell = cursor.map(|c| (c.x, c.y));

        let mut rows = row_iter.update(&snapshot)?;
        let mut y = 0u16;
        let mut graphemes = Vec::with_capacity(4);
        while let Some(row) = rows.next() {
            let py = oy + y as f32 * ch;
            self.row_cells.clear();
            self.row_text.clear();
            self.unit_cell.clear();

            // Pass 1: walk the cells, queue backgrounds, collect text and styles.
            let mut cells = cell_iter.update(row)?;
            let mut column = 0u16;
            while let Some(cell) = cells.next() {
                let col = column;
                column += 1;
                let width = match cell.raw_cell()?.wide()? {
                    CellWide::SpacerTail | CellWide::SpacerHead => continue,
                    CellWide::Wide => 2,
                    _ => 1,
                };
                let style = if cell.has_styling()? { Some(cell.style()?) } else { None };
                let mut fg = cell.fg_color()?.unwrap_or(colors.foreground);
                let mut bg = cell.bg_color()?;
                let inverse = style.is_some_and(|s| s.inverse);
                if inverse != cell.is_selected()? {
                    (fg, bg) = (bg.unwrap_or(colors.background), Some(fg));
                }
                let px = ox + col as f32 * cw;
                if block_at == Some((col, y)) {
                    // The cursor block replaces the background; its text shows in the background color.
                    fg = colors.background;
                } else if let Some(bg) = bg {
                    self.solid(px, py, cw * width as f32, ch, rgba(bg, 1.0));
                }

                let alpha = if style.is_some_and(|s| s.faint) { 0.5 } else { 1.0 };
                let start = self.row_text.len();
                if cell.graphemes_len()? > 0 && !style.is_some_and(|s| s.invisible) {
                    graphemes.clear();
                    graphemes.extend(cell.graphemes()?);
                    let mut buf = [0u16; 2];
                    for c in &graphemes {
                        self.row_text.extend_from_slice(c.encode_utf16(&mut buf));
                    }
                }
                let index = self.row_cells.len();
                self.unit_cell.resize(self.row_text.len(), index);
                let sprite = match graphemes.as_slice() {
                    [c] if self.row_text.len() > start && sprite::is_sprite(*c) => Some(*c),
                    _ => None,
                };
                let underline_color = match style.map(|s| s.underline_color) {
                    Some(StyleColor::Rgb(c)) => c,
                    Some(StyleColor::Palette(i)) => colors.palette[i.0 as usize],
                    _ => fg,
                };
                self.row_cells.push(CellPaint {
                    column: col,
                    width,
                    text: start..self.row_text.len(),
                    face: style.map_or(0, |s| (s.bold as FaceId * BOLD) | (s.italic as FaceId * ITALIC)),
                    fg: rgba(fg, alpha),
                    underline: style.map_or(Underline::None, |s| s.underline),
                    underline_color: rgba(underline_color, alpha),
                    strikethrough: style.is_some_and(|s| s.strikethrough),
                    sprite,
                });
            }
            row.set_dirty(false)?;

            // Pass 2: shape runs of adjacent, same-style text and queue their glyphs.
            let mut i = 0;
            while i < self.row_cells.len() {
                let first = &self.row_cells[i];
                if let Some(c) = first.sprite {
                    let (x, baseline, color) = (ox + first.column as f32 * cw, py + font.baseline as f32, first.fg);
                    self.glyph(font, SPRITE_FACE, c as u16, x, baseline, color);
                    i += 1;
                    continue;
                }
                if first.text.is_empty() {
                    i += 1;
                    continue;
                }
                let mut end = i + 1;
                while end < self.row_cells.len() {
                    let (prev, next) = (&self.row_cells[end - 1], &self.row_cells[end]);
                    let at_cursor = |c: &CellPaint| cursor_cell == Some((c.column, y));
                    let joins = !next.text.is_empty()
                        && next.sprite.is_none()
                        && next.face == first.face
                        && next.column == prev.column + prev.width
                        && !at_cursor(prev)
                        && !at_cursor(next);
                    if !joins {
                        break;
                    }
                    end += 1;
                }
                let text = first.text.start..self.row_cells[end - 1].text.end;
                self.shaped.clear();
                if let Err(e) = font.shape(&self.row_text[text.clone()], first.face, &mut self.shaped) {
                    eprintln!("shaping failed: {e}");
                }
                for k in 0..self.shaped.len() {
                    let g = self.shaped[k];
                    let cell = &self.row_cells[self.unit_cell[text.start + g.cluster as usize]];
                    let (x, baseline, color) =
                        (ox + cell.column as f32 * cw + g.x, py + font.baseline as f32 - g.y, cell.fg);
                    self.glyph(font, g.face, g.glyph, x, baseline, color);
                }
                i = end;
            }

            // Pass 3: lines over the text.
            let line = font.underline_thickness as f32;
            for k in 0..self.row_cells.len() {
                let cell = &self.row_cells[k];
                let (px, w) = (ox + cell.column as f32 * cw, cw * cell.width as f32);
                let under_y = py + font.underline_y as f32;
                let (color, strike_color, strike) = (cell.underline_color, cell.fg, cell.strikethrough);
                match cell.underline {
                    Underline::Single => self.solid(px, under_y, w, line, color),
                    Underline::Double => {
                        self.solid(px, under_y - line, w, line, color);
                        self.solid(px, under_y + line, w, line, color);
                    }
                    Underline::Curly => self.pattern(CURLY, px, under_y - line, w, line * 3.0, color, cw),
                    Underline::Dotted => self.pattern(DOTTED, px, under_y, w, line, color, line),
                    Underline::Dashed => self.pattern(DASHED, px, under_y, w, line, color, cw / 2.0),
                    _ => {}
                }
                if strike {
                    self.solid(px, py + font.strikethrough_y as f32, w, line, strike_color);
                }
            }
            y += 1;
        }

        if let Some(c) = cursor {
            let (px, py) = (ox + c.x as f32 * cw, oy + c.y as f32 * ch);
            let line = (font.underline_thickness as f32).max(1.0);
            match cursor_style {
                CursorVisualStyle::Bar => self.solid(px, py, (line * 2.0).max(2.0), ch, cursor_color),
                CursorVisualStyle::Underline => self.solid(px, py + ch - line * 2.0, cw, line * 2.0, cursor_color),
                CursorVisualStyle::BlockHollow => {
                    self.solid(px, py, cw, line, cursor_color);
                    self.solid(px, py + ch - line, cw, line, cursor_color);
                    self.solid(px, py, line, ch, cursor_color);
                    self.solid(px + cw - line, py, line, ch, cursor_color);
                }
                _ => {}
            }
        }

        snapshot.set_dirty(Dirty::Clean)?;
        Ok(FrameInfo { background: colors.background, cursor_blinks: cursor_blinks && snapshot.cursor_visible()? })
    }

    fn solid(&mut self, x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) {
        self.quads.push(Quad { rect: [x, y, width, height], uv: [0.0; 2], color, kind: SOLID, param: 0.0 });
    }

    /// A line drawn by the shader in a repeating pattern of the given period.
    #[allow(clippy::too_many_arguments, reason = "a rectangle plus how to fill it")]
    fn pattern(&mut self, kind: u32, x: f32, y: f32, width: f32, height: f32, color: [f32; 4], period: f32) {
        self.quads.push(Quad { rect: [x, y, width, height], uv: [0.0; 2], color, kind, param: period });
    }

    /// Queues a glyph with its pen at `x` on the baseline `baseline`.
    fn glyph(&mut self, font: &Font, face: FaceId, glyph: u16, x: f32, baseline: f32, color: [f32; 4]) {
        let Some(slot) = self.slot(font, face, glyph) else { return };
        self.quads.push(Quad {
            rect: [
                (x + slot.left as f32).round(),
                (baseline + slot.top as f32).round(),
                slot.width as f32,
                slot.height as f32,
            ],
            uv: [slot.x as f32, slot.y as f32],
            color,
            kind: if slot.color { COLOR_GLYPH } else { GLYPH },
            param: 0.0,
        });
    }

    /// Finds a glyph in an atlas, rasterizing and uploading it on first use.
    fn slot(&mut self, font: &Font, face: FaceId, glyph: u16) -> Option<Slot> {
        if let Some(slot) = self.glyphs.get(&(face, glyph)) {
            return *slot;
        }
        let bitmap = if face == SPRITE_FACE {
            let c = char::from_u32(glyph as u32).unwrap_or(' ');
            let coverage = sprite::draw(c, font.cell_width, font.cell_height, font.underline_thickness);
            // Positioned so the bitmap's top-left lands on the cell's top-left.
            Ok(coverage.map(|coverage| crate::font::GlyphBitmap {
                width: font.cell_width,
                height: font.cell_height,
                left: 0,
                top: -(font.baseline as i32),
                pixels: Pixels::Coverage(coverage),
            }))
        } else {
            font.rasterize(face, glyph)
        };
        let slot = match bitmap {
            Err(e) => {
                eprintln!("rasterizing glyph {glyph} of face {face} failed: {e}");
                None
            }
            Ok(None) => None,
            Ok(Some(b)) => {
                let (atlas, data, color) = match &b.pixels {
                    Pixels::Coverage(data) => (&mut self.coverage_atlas, data, false),
                    Pixels::Color(data) => (&mut self.color_atlas, data, true),
                };
                let Some((x, y)) = atlas.upload(&self.context, b.width, b.height, data) else {
                    self.atlas_full = true;
                    return None;
                };
                Some(Slot { x, y, width: b.width, height: b.height, left: b.left, top: b.top, color })
            }
        };
        self.glyphs.insert((face, glyph), slot);
        slot
    }

    /// Draws everything queued since the last call into `target`.
    pub fn render(&mut self, target: &ID3D11RenderTargetView, width: u32, height: u32, clear: RgbColor) -> Result<()> {
        unsafe {
            let ctx = &self.context;
            let count = self.quads.len();
            if self.quad_buffer.as_ref().is_none_or(|(_, cap)| *cap < count) {
                let capacity = count.next_power_of_two().max(1024);
                let desc = D3D11_BUFFER_DESC {
                    ByteWidth: (capacity * size_of::<Quad>()) as u32,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                };
                let mut buffer = None;
                self.device.CreateBuffer(&desc, None, Some(&mut buffer))?;
                self.quad_buffer = Some((buffer.unwrap(), capacity));
            }
            let (buffer, _) = self.quad_buffer.as_ref().unwrap();
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped))?;
            std::ptr::copy_nonoverlapping(self.quads.as_ptr(), mapped.pData.cast(), count);
            ctx.Unmap(buffer, 0);

            let globals = [2.0 / width as f32, 2.0 / height as f32, 0.0, 0.0];
            ctx.UpdateSubresource(&self.globals, 0, None, globals.as_ptr().cast(), 0, 0);

            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.ClearRenderTargetView(target, &rgba(clear, 1.0));
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(&self.layout);
            let stride = size_of::<Quad>() as u32;
            let offset = 0;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(buffer.clone())), Some(&stride), Some(&offset));
            ctx.VSSetShader(&self.vertex_shader, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.globals.clone())]));
            ctx.PSSetShader(&self.pixel_shader, None);
            ctx.PSSetShaderResources(
                0,
                Some(&[Some(self.coverage_atlas.view.clone()), Some(self.color_atlas.view.clone())]),
            );
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.RSSetState(&self.raster);
            ctx.DrawInstanced(4, count as u32, 0, 0);
        }
        self.quads.clear();
        Ok(())
    }
}

/// A window's swap chain.
pub struct Surface {
    swap_chain: IDXGISwapChain2,
    target: Option<ID3D11RenderTargetView>,
    /// Signaled when the swap chain can take another frame. Waiting on it
    /// before drawing means we always draw with the latest state.
    pub waitable: HANDLE,
    pub width: u32,
    pub height: u32,
}

const SWAP_FLAGS: DXGI_SWAP_CHAIN_FLAG = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;

impl Surface {
    pub fn new(renderer: &Renderer, hwnd: HWND, width: u32, height: u32) -> Result<Surface> {
        unsafe {
            let factory: IDXGIFactory2 = renderer.device.cast::<IDXGIDevice>()?.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(1),
                Height: height.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_NONE,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: SWAP_FLAGS.0 as u32,
                ..Default::default()
            };
            let swap_chain: IDXGISwapChain2 =
                factory.CreateSwapChainForHwnd(&renderer.device, hwnd, &desc, None, None)?.cast()?;
            factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER)?;
            swap_chain.SetMaximumFrameLatency(1)?;
            let waitable = swap_chain.GetFrameLatencyWaitableObject();
            Ok(Surface { swap_chain, target: None, waitable, width: width.max(1), height: height.max(1) })
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        self.target = None; // the buffers can't be resized while a view holds them
        unsafe { self.swap_chain.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, SWAP_FLAGS)? };
        (self.width, self.height) = (width, height);
        Ok(())
    }

    pub fn target(&mut self, renderer: &Renderer) -> Result<ID3D11RenderTargetView> {
        if let Some(t) = &self.target {
            return Ok(t.clone());
        }
        unsafe {
            let buffer: ID3D11Texture2D = self.swap_chain.GetBuffer(0)?;
            let mut target = None;
            renderer.device.CreateRenderTargetView(&buffer, None, Some(&mut target))?;
            self.target = target;
        }
        Ok(self.target.clone().unwrap())
    }

    pub fn present(&self) -> Result<()> {
        unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)).ok() }
    }
}

fn rgba(c: RgbColor, alpha: f32) -> [f32; 4] {
    [c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, alpha]
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let result = unsafe {
        D3DCompile(
            SHADER.as_ptr() as *const c_void,
            SHADER.len(),
            None,
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let message = errors.map(|b| String::from_utf8_lossy(blob_bytes(&b)).into_owned()).unwrap_or_default();
        return Err(windows::core::Error::new(e.code(), message));
    }
    Ok(code.unwrap())
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use libghostty_vt::terminal::SizeReportSize;

    /// Renders into an offscreen texture and reads the pixels back (BGRA rows).
    pub fn render_offscreen(renderer: &mut Renderer, width: u32, height: u32, clear: RgbColor) -> Vec<[u8; 4]> {
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            let mut texture = None;
            renderer.device.CreateTexture2D(&desc, None, Some(&mut texture)).unwrap();
            let texture = texture.unwrap();
            let mut target = None;
            renderer.device.CreateRenderTargetView(&texture, None, Some(&mut target)).unwrap();
            renderer.render(&target.unwrap(), width, height, clear).unwrap();

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..desc
            };
            let mut staging = None;
            renderer.device.CreateTexture2D(&staging_desc, None, Some(&mut staging)).unwrap();
            let staging = staging.unwrap();
            renderer.context.CopyResource(&staging, &texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            renderer.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).unwrap();
            let mut pixels = Vec::with_capacity((width * height) as usize);
            for row in 0..height {
                let line = mapped.pData.cast::<u8>().add((row * mapped.RowPitch) as usize);
                for col in 0..width {
                    let p = line.add(col as usize * 4);
                    pixels.push([*p, *p.add(1), *p.add(2), *p.add(3)]);
                }
            }
            renderer.context.Unmap(&staging, 0);
            pixels
        }
    }

    /// A pane with `vt` written to it, rendered offscreen. Returns the pixels
    /// and the image width.
    fn render_vt(vt: &[u8], columns: u16, rows: u16) -> (Vec<[u8; 4]>, u32, Font) {
        let mut renderer = Renderer::with_driver(D3D_DRIVER_TYPE_WARP).unwrap();
        let mut font = Font::new("Cascadia Code", 12.0, 96).unwrap();
        let (cw, ch) = (font.cell_width, font.cell_height);
        let size = SizeReportSize { rows, columns, cell_width: cw, cell_height: ch };
        let mut pane = Pane::spawn("cmd.exe /c exit", size, || {}, || {}).unwrap();
        pane.terminal.vt_write(b"\x1b[H\x1b[2J\x1b[?25l");
        pane.terminal.vt_write(vt);
        let frame = renderer.draw_pane(&mut pane, &mut font, (0.0, 0.0), true, true).unwrap();
        let w = cw * columns as u32;
        let pixels = render_offscreen(&mut renderer, w, ch * rows as u32, frame.background);
        (pixels, w, font)
    }

    /// Times queuing a full screen: the libghostty walk, shaping and glyph
    /// lookups, not GPU work. `cargo test --release frame_build_time -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn frame_build_time() {
        let mut renderer = Renderer::with_driver(D3D_DRIVER_TYPE_WARP).unwrap();
        let mut font = Font::new("Cascadia Code", 12.0, 96).unwrap();
        let size =
            SizeReportSize { rows: 40, columns: 120, cell_width: font.cell_width, cell_height: font.cell_height };
        let mut pane = Pane::spawn("cmd.exe /c exit", size, || {}, || {}).unwrap();
        let line =
            "fn main() { let x = a => b != c; println!(\"{x:?}\"); } // \x1b[1mbold\x1b[0m \x1b[32mgreen\x1b[0m ";
        for _ in 0..40 {
            pane.terminal.vt_write(&line.repeat(2).as_bytes()[..118]);
            pane.terminal.vt_write(b"\r\n");
        }
        let frames = 200;
        let start = std::time::Instant::now();
        for _ in 0..frames {
            // A full redraw each time, as if every row changed.
            renderer.draw_pane(&mut pane, &mut font, (0.0, 0.0), true, true).unwrap();
            renderer.quads.clear();
        }
        let per_frame = start.elapsed() / frames;
        eprintln!("120x40 frame build: {per_frame:?}");
    }

    #[test]
    fn shelves_pack_rows_and_report_full() {
        let mut s = Shelves::default();
        assert_eq!(s.alloc(10, 20), Some((0, 0)));
        assert_eq!(s.alloc(10, 5), Some((11, 0)));
        assert_eq!(s.alloc(ATLAS_SIZE, 5), Some((0, 21)), "too wide for this row starts a new one");
        assert_eq!(s.alloc(1, ATLAS_SIZE), None);
    }

    #[test]
    fn terminal_text_and_colors_reach_the_pixels() {
        // Cell 0: a red-background space. Cells 1..: white text on the default background.
        let (px, w, font) = render_vt(b"\x1b[41m \x1b[0mWWWW", 20, 4);
        let (cw, ch) = (font.cell_width, font.cell_height);
        let at = |x: u32, y: u32| px[(y * w + x) as usize];
        let red = at(cw / 2, ch / 2);
        assert!(red[2] > 150 && red[2] > red[1] + 60, "cell 0 should be red, got BGRA {red:?}");
        let corner = at(w - 1, ch * 4 - 1);
        assert_eq!(&corner[..3], &[0x34, 0x2c, 0x28], "untouched pixels are the default background");
        let ink = (cw..cw * 5).flat_map(|x| (0..ch).map(move |y| (x, y))).filter(|&(x, y)| at(x, y)[1] > 200).count();
        assert!(ink > 20, "the W glyphs should put bright pixels on screen, found {ink}");
    }

    #[test]
    fn ligatures_render_differently_from_separate_characters() {
        // "=>" shaped as one run uses Cascadia's arrow alternates. Overlaying
        // "=" and " >" rendered on their own shows what no shaping would draw.
        let (joined, _, _) = render_vt(b"=>", 4, 1);
        let (equals, _, _) = render_vt(b"=", 4, 1);
        let (greater, _, _) = render_vt(b" >", 4, 1);
        let green = |px: &[[u8; 4]]| px.iter().map(|p| p[1]).collect::<Vec<_>>();
        let apart: Vec<u8> = green(&equals).iter().zip(green(&greater)).map(|(a, b)| (*a).max(b)).collect();
        assert_ne!(green(&joined), apart);
    }

    #[test]
    fn box_lines_are_seamless_across_cells_and_rows() {
        let (px, w, font) = render_vt("───\r\n │\r\n │".as_bytes(), 3, 3);
        let (cw, ch) = (font.cell_width, font.cell_height);
        let lit = |x: u32, y: u32| px[(y * w + x) as usize][1] > 200;
        let row = (0..ch).find(|&y| lit(cw / 2, y)).expect("the horizontal line");
        assert!((0..w).all(|x| lit(x, row)), "no gaps between the three cells");
        let col = (cw..cw * 2).find(|&x| lit(x, ch + ch / 2)).expect("the vertical line");
        assert!((ch..ch * 3).all(|y| lit(col, y)), "no gap between the two rows");
    }

    #[test]
    fn emoji_render_in_color() {
        let (px, _, _) = render_vt("😀".as_bytes(), 4, 1);
        // Segoe UI Emoji's grinning face is mostly yellow: red and green high, blue low (BGRA).
        let yellow = px.iter().filter(|p| p[2] > 180 && p[1] > 140 && p[0] < 100).count();
        assert!(yellow > 20, "expected yellow emoji pixels, found {yellow}");
    }

    #[test]
    fn curly_underlines_wave() {
        let (px, w, font) = render_vt(b"\x1b[4:3m          ", 10, 1);
        // Find the top row of underline ink in each column; a wave puts it at
        // different heights across the run.
        let ch = font.cell_height;
        let ink_row = |x: u32| (0..ch).find(|&y| px[(y * w + x) as usize][1] > 120);
        let heights: std::collections::HashSet<_> = (0..w).filter_map(ink_row).collect();
        assert!(heights.len() >= 2, "a curly underline should not be flat: {heights:?}");
    }
}
