//! Direct3D 11 renderer. A frame is a list of quads drawn in one instanced
//! call: solid rectangles (cell backgrounds, cursor, underlines) and glyphs
//! cut from a coverage atlas and tinted with their cell's color.

use std::collections::HashMap;
use std::ffi::c_void;

use libghostty_vt::render::{CursorVisualStyle, Dirty};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{RgbColor, Underline};
use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, TRUE};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R32_UINT,
    DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain2,
};
use windows::core::{Interface, PCSTR, Result, s};

use crate::font::{BOLD, FaceStyle, Font, ITALIC};
use crate::term::Pane;

const ATLAS_SIZE: u32 = 2048;
const SOLID: u32 = 0;
const GLYPH: u32 = 1;

const SHADER: &str = r#"
cbuffer Globals : register(b0) { float2 pixel_to_clip; };
Texture2D<float> atlas : register(t0);

struct Quad {
    float4 rect : RECT;          // x, y, width, height in pixels
    float2 uv : UV;              // top-left texel in the atlas
    float4 color : COLOR;        // straight alpha
    uint kind : KIND;            // 0 = solid, 1 = glyph
};
struct Pixel {
    float4 pos : SV_Position;
    float2 uv : UV;
    nointerpolation float4 color : COLOR;
    nointerpolation uint kind : KIND;
};

Pixel vs_main(Quad q, uint id : SV_VertexID) {
    float2 corner = float2(id & 1, id >> 1);
    float2 p = q.rect.xy + corner * q.rect.zw;
    Pixel o;
    o.pos = float4(p.x * pixel_to_clip.x - 1, 1 - p.y * pixel_to_clip.y, 0, 1);
    o.uv = q.uv + corner * q.rect.zw;
    o.color = q.color;
    o.kind = q.kind;
    return o;
}

float4 ps_main(Pixel p) : SV_Target {
    float a = p.color.a;
    if (p.kind == 1) a *= atlas.Load(int3(p.uv, 0));
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
}

/// Where a rasterized glyph lives in the atlas.
#[derive(Clone, Copy)]
struct Slot {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    left: i32,
    top: i32,
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

pub struct Renderer {
    pub device: ID3D11Device,
    context: ID3D11DeviceContext,
    vertex_shader: ID3D11VertexShader,
    pixel_shader: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    globals: ID3D11Buffer,
    atlas: ID3D11Texture2D,
    atlas_view: ID3D11ShaderResourceView,
    shelves: Shelves,
    /// `None` marks glyphs with no ink (spaces) so they aren't rasterized again.
    glyphs: HashMap<(FaceStyle, u16), Option<Slot>>,
    atlas_full: bool,
    quads: Vec<Quad>,
    quad_buffer: Option<(ID3D11Buffer, usize)>,
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

            let atlas_desc = D3D11_TEXTURE2D_DESC {
                Width: ATLAS_SIZE,
                Height: ATLAS_SIZE,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                ..Default::default()
            };
            let mut atlas = None;
            device.CreateTexture2D(&atlas_desc, None, Some(&mut atlas))?;
            let atlas = atlas.unwrap();
            let mut atlas_view = None;
            device.CreateShaderResourceView(&atlas, None, Some(&mut atlas_view))?;

            Ok(Renderer {
                device,
                context: context.unwrap(),
                vertex_shader: vertex_shader.unwrap(),
                pixel_shader: pixel_shader.unwrap(),
                layout: layout.unwrap(),
                blend: blend.unwrap(),
                raster: raster.unwrap(),
                globals: globals.unwrap(),
                atlas,
                atlas_view: atlas_view.unwrap(),
                shelves: Shelves::default(),
                glyphs: HashMap::new(),
                atlas_full: false,
                quads: Vec::new(),
                quad_buffer: None,
            })
        }
    }

    /// Forgets every cached glyph, e.g. after the font or DPI changed.
    pub fn clear_glyphs(&mut self) {
        self.glyphs.clear();
        self.shelves = Shelves::default();
        self.atlas_full = false;
    }

    /// Queues a pane's visible screen with its top-left corner at `origin`.
    /// Returns the background color, for clearing around the grid.
    pub fn draw_pane(
        &mut self,
        pane: &mut Pane,
        font: &mut Font,
        (ox, oy): (f32, f32),
        focused: bool,
    ) -> std::result::Result<RgbColor, libghostty_vt::Error> {
        if self.atlas_full {
            // Glyphs queued last frame were dropped; start over with an empty atlas.
            self.clear_glyphs();
        }
        let Pane { terminal, render_state, row_iter, cell_iter, .. } = pane;
        let snapshot = render_state.update(terminal)?;
        let colors = snapshot.colors()?;
        let (cw, ch) = (font.cell_width as f32, font.cell_height as f32);

        let cursor = if snapshot.cursor_visible()? { snapshot.cursor_viewport()? } else { None };
        let mut cursor_style = snapshot.cursor_visual_style()?;
        if !focused {
            cursor_style = CursorVisualStyle::BlockHollow;
        }
        let cursor_color = colors.cursor.unwrap_or(colors.foreground);
        let block_at = cursor.filter(|_| cursor_style == CursorVisualStyle::Block).map(|c| (c.x, c.y));
        if let Some((x, y)) = block_at {
            self.solid(ox + x as f32 * cw, oy + y as f32 * ch, cw, ch, rgba(cursor_color, 1.0));
        }

        let mut rows = row_iter.update(&snapshot)?;
        let mut y = 0u16;
        let mut text = Vec::with_capacity(4);
        while let Some(row) = rows.next() {
            let mut cells = cell_iter.update(row)?;
            let py = oy + y as f32 * ch;
            let mut x = 0u16;
            while let Some(cell) = cells.next() {
                let px = ox + x as f32 * cw;
                let col = x;
                x += 1;
                let width = match cell.raw_cell()?.wide()? {
                    CellWide::SpacerTail | CellWide::SpacerHead => continue,
                    CellWide::Wide => 2.0,
                    _ => 1.0,
                };
                let style = if cell.has_styling()? { Some(cell.style()?) } else { None };
                let mut fg = cell.fg_color()?.unwrap_or(colors.foreground);
                let mut bg = cell.bg_color()?;
                let inverse = style.is_some_and(|s| s.inverse);
                if inverse != cell.is_selected()? {
                    (fg, bg) = (bg.unwrap_or(colors.background), Some(fg));
                }
                let under_cursor = block_at == Some((col, y));
                if under_cursor {
                    // The cursor block replaces the background; text shows through in the background color.
                    fg = colors.background;
                } else if let Some(bg) = bg {
                    self.solid(px, py, cw * width, ch, rgba(bg, 1.0));
                }

                let alpha = if style.is_some_and(|s| s.faint) { 0.5 } else { 1.0 };
                if cell.graphemes_len()? > 0 && !style.is_some_and(|s| s.invisible) {
                    text.clear();
                    text.extend(cell.graphemes()?);
                    let face = style.map_or(0, |s| (s.bold as FaceStyle * BOLD) | (s.italic as FaceStyle * ITALIC));
                    // ponytail: first codepoint only; shaping of clusters and ligatures comes in M2.
                    let glyph = font.glyph_index(text[0], face);
                    self.glyph(font, face, glyph, px, py + font.baseline as f32, rgba(fg, alpha));
                }
                if let Some(s) = style {
                    let line = font.underline_thickness as f32;
                    let line_color = match s.underline_color {
                        libghostty_vt::style::StyleColor::Rgb(c) => c,
                        libghostty_vt::style::StyleColor::Palette(i) => colors.palette[i.0 as usize],
                        _ => fg,
                    };
                    match s.underline {
                        Underline::None => {}
                        Underline::Double => {
                            self.solid(px, py + font.underline_y as f32, cw * width, line, rgba(line_color, alpha));
                            self.solid(
                                px,
                                py + font.underline_y as f32 + 2.0 * line,
                                cw * width,
                                line,
                                rgba(line_color, alpha),
                            );
                        }
                        // ponytail: curly, dotted and dashed draw as single lines until M2.
                        _ => self.solid(px, py + font.underline_y as f32, cw * width, line, rgba(line_color, alpha)),
                    }
                    if s.strikethrough {
                        self.solid(px, py + font.strikethrough_y as f32, cw * width, line, rgba(fg, alpha));
                    }
                }
            }
            row.set_dirty(false)?;
            y += 1;
        }

        if let Some(c) = cursor {
            let (px, py) = (ox + c.x as f32 * cw, oy + c.y as f32 * ch);
            let line = (font.underline_thickness as f32).max(1.0);
            match cursor_style {
                CursorVisualStyle::Bar => self.solid(px, py, (line * 2.0).max(2.0), ch, rgba(cursor_color, 1.0)),
                CursorVisualStyle::Underline => {
                    self.solid(px, py + ch - line * 2.0, cw, line * 2.0, rgba(cursor_color, 1.0))
                }
                CursorVisualStyle::BlockHollow => {
                    self.solid(px, py, cw, line, rgba(cursor_color, 1.0));
                    self.solid(px, py + ch - line, cw, line, rgba(cursor_color, 1.0));
                    self.solid(px, py, line, ch, rgba(cursor_color, 1.0));
                    self.solid(px + cw - line, py, line, ch, rgba(cursor_color, 1.0));
                }
                _ => {}
            }
        }

        snapshot.set_dirty(Dirty::Clean)?;
        Ok(colors.background)
    }

    fn solid(&mut self, x: f32, y: f32, width: f32, height: f32, color: [f32; 4]) {
        self.quads.push(Quad { rect: [x, y, width, height], uv: [0.0; 2], color, kind: SOLID });
    }

    /// Queues a glyph with its pen at `x` on the baseline `baseline`.
    fn glyph(&mut self, font: &Font, face: FaceStyle, glyph: u16, x: f32, baseline: f32, color: [f32; 4]) {
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
            kind: GLYPH,
        });
    }

    /// Finds a glyph in the atlas, rasterizing and uploading it on first use.
    fn slot(&mut self, font: &Font, face: FaceStyle, glyph: u16) -> Option<Slot> {
        if let Some(slot) = self.glyphs.get(&(face, glyph)) {
            return *slot;
        }
        let bitmap = font.rasterize(glyph, face).ok()?;
        let slot = match bitmap {
            None => None,
            Some(b) => {
                let Some((x, y)) = self.shelves.alloc(b.width, b.height) else {
                    self.atlas_full = true;
                    return None;
                };
                let region = D3D11_BOX { left: x, top: y, front: 0, right: x + b.width, bottom: y + b.height, back: 1 };
                unsafe {
                    self.context.UpdateSubresource(
                        &self.atlas,
                        0,
                        Some(&region),
                        b.coverage.as_ptr().cast(),
                        b.width,
                        0,
                    );
                }
                Some(Slot { x, y, width: b.width, height: b.height, left: b.left, top: b.top })
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
            let c = rgba(clear, 1.0);
            ctx.ClearRenderTargetView(target, &c);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.IASetInputLayout(&self.layout);
            let stride = size_of::<Quad>() as u32;
            let offset = 0;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(buffer.clone())), Some(&stride), Some(&offset));
            ctx.VSSetShader(&self.vertex_shader, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.globals.clone())]));
            ctx.PSSetShader(&self.pixel_shader, None);
            ctx.PSSetShaderResources(0, Some(&[Some(self.atlas_view.clone())]));
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
        let mut renderer = Renderer::with_driver(D3D_DRIVER_TYPE_WARP).unwrap();
        let mut font = Font::new("Cascadia Code", 12.0, 96).unwrap();
        let (cw, ch) = (font.cell_width, font.cell_height);
        let size = SizeReportSize { rows: 4, columns: 20, cell_width: cw, cell_height: ch };
        let mut pane = Pane::spawn("cmd.exe /c exit", size, || {}, || {}).unwrap();
        // Cell 0: a red-background space. Cells 1..: white text on the default background.
        pane.terminal.vt_write(b"\x1b[H\x1b[2J\x1b[41m \x1b[0mWWWW\x1b[?25l");

        let bg = renderer.draw_pane(&mut pane, &mut font, (0.0, 0.0), true).unwrap();
        assert_eq!(bg, crate::term::DEFAULT_BACKGROUND);
        let (w, h) = (cw * 20, ch * 4);
        let px = render_offscreen(&mut renderer, w, h, bg);
        let at = |x: u32, y: u32| px[(y * w + x) as usize];

        let red = at(cw / 2, ch / 2);
        assert!(red[2] > 150 && red[2] > red[1] + 60, "cell 0 should be red, got BGRA {red:?}");
        let corner = at(w - 1, h - 1);
        assert_eq!(&corner[..3], &[0x34, 0x2c, 0x28], "untouched pixels are the default background");
        let ink = (cw..cw * 5).flat_map(|x| (0..ch).map(move |y| (x, y))).filter(|&(x, y)| at(x, y)[1] > 200).count();
        assert!(ink > 20, "the W glyphs should put bright pixels on screen, found {ink}");
    }
}
