//! Direct2D + DirectWrite renderer, using only Windows system components.
//! Device resources live with the window; text/geometry caches are bounded.
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use windows::core::{w, Interface};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub enum Font {
    Caption,
    CompactNumber,
    Title,
    Body,
    Small,
    Header,
}

impl Font {
    fn metrics(self) -> (f32, DWRITE_FONT_WEIGHT) {
        match self {
            Self::Caption => (12.0, DWRITE_FONT_WEIGHT_NORMAL),
            Self::CompactNumber => (19.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Self::Title => (19.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            Self::Body => (16.0, DWRITE_FONT_WEIGHT_NORMAL),
            Self::Small => (13.0, DWRITE_FONT_WEIGHT_NORMAL),
            Self::Header => (14.0, DWRITE_FONT_WEIGHT_MEDIUM),
        }
    }
}

struct ComGuard(bool);
impl ComGuard {
    fn new() -> Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // A host can already have initialized this thread as MTA.
        if status.0 == 0x80010106u32 as i32 {
            return Ok(Self(false));
        }
        status.ok().context("无法初始化系统图形组件")?;
        Ok(Self(true))
    }
}
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

#[derive(Hash, PartialEq, Eq)]
struct TextKey {
    font: Font,
    text: String,
    width: i32,
    height: i32,
    flags: u32,
}

pub struct Graphics {
    window: Option<WindowSurface>,
    software: bool,
    device_failures: u8,
    layouts: RefCell<HashMap<TextKey, IDWriteTextLayout>>,
    formats: HashMap<Font, IDWriteTextFormat>,
    icons: RefCell<HashMap<String, ID2D1PathGeometry>>,
    logo_pixels: Vec<u8>,
    stroke: ID2D1StrokeStyle,
    dwrite: IDWriteFactory,
    d2d: ID2D1Factory,
    // Last field: COM must outlive every interface above.
    _com: ComGuard,
}

struct Surface {
    logo: RefCell<Option<ID2D1Bitmap>>,
    target: ID2D1RenderTarget,
    brush: ID2D1SolidColorBrush,
    failure: RefCell<Option<anyhow::Error>>,
    gradients: RefCell<HashMap<(u32, u32), ID2D1LinearGradientBrush>>,
}
struct WindowSurface {
    surface: Surface,
    window: ID2D1HwndRenderTarget,
    size: (u32, u32),
}

#[derive(Clone, Copy)]
pub struct Canvas<'a> {
    graphics: &'a Graphics,
    surface: &'a Surface,
}

fn color(value: u32, opacity: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: (value & 255) as f32 / 255.0,
        g: ((value >> 8) & 255) as f32 / 255.0,
        b: ((value >> 16) & 255) as f32 / 255.0,
        a: opacity,
    }
}
fn rect(value: &windows_sys::Win32::Foundation::RECT) -> D2D_RECT_F {
    D2D_RECT_F {
        left: value.left as f32,
        top: value.top as f32,
        right: value.right as f32,
        bottom: value.bottom as f32,
    }
}
fn point(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x, y }
}

impl Surface {
    fn new(target: ID2D1RenderTarget) -> Result<Self> {
        unsafe {
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = target.CreateSolidColorBrush(&color(0, 1.0), None)?;
            Ok(Self {
                target,
                logo: RefCell::new(None),
                brush,
                failure: RefCell::new(None),
                gradients: RefCell::new(HashMap::new()),
            })
        }
    }
    fn set_color(&self, value: u32, opacity: f32) -> &ID2D1SolidColorBrush {
        unsafe {
            self.brush.SetColor(&color(value, opacity));
        }
        &self.brush
    }
}

const LOGO_SIZE: u32 = 64;

/// Reuse resource 1, the exact icon embedded for the EXE/taskbar/tray. No extra
/// image file or duplicate embedded asset is required at runtime.
fn load_logo_pixels() -> Result<Vec<u8>> {
    use windows::Win32::Graphics::Imaging::*;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows_sys::Win32::UI::WindowsAndMessaging as wm;
    unsafe {
        let module = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        let icon = wm::LoadImageW(
            module,
            1 as *const u16,
            wm::IMAGE_ICON,
            LOGO_SIZE as i32,
            LOGO_SIZE as i32,
            0,
        );
        if icon.is_null() {
            return Err(std::io::Error::last_os_error()).context("读取内嵌软件 Logo 失败");
        }
        let result = (|| -> Result<Vec<u8>> {
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let source =
                wic.CreateBitmapFromHICON(windows::Win32::UI::WindowsAndMessaging::HICON(icon))?;
            let converter = wic.CreateFormatConverter()?;
            converter.Initialize(
                &source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;
            let mut pixels = vec![0u8; (LOGO_SIZE * LOGO_SIZE * 4) as usize];
            converter.CopyPixels(std::ptr::null(), LOGO_SIZE * 4, &mut pixels)?;
            Ok(pixels)
        })();
        wm::DestroyIcon(icon);
        result
    }
}

impl Graphics {
    pub fn new() -> Result<Self> {
        let com = ComGuard::new()?;
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut formats = HashMap::new();
            for font in [
                Font::Caption,
                Font::CompactNumber,
                Font::Title,
                Font::Body,
                Font::Small,
                Font::Header,
            ] {
                let (size, weight) = font.metrics();
                let family = if matches!(font, Font::CompactNumber) {
                    w!("Segoe UI")
                } else {
                    w!("Microsoft YaHei UI")
                };
                let format = dwrite.CreateTextFormat(
                    family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("zh-CN"),
                )?;
                formats.insert(font, format);
            }
            let stroke = d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 2.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            Ok(Self {
                window: None,
                software: false,
                device_failures: 0,
                layouts: RefCell::new(HashMap::new()),
                formats,
                icons: RefCell::new(HashMap::new()),
                logo_pixels: load_logo_pixels()?,
                stroke,
                dwrite,
                d2d,
                _com: com,
            })
        }
    }

    /// Coordinates use DIPs. Effective scale includes monitor DPI and fit-to-work-area.
    pub fn render_window(
        &mut self,
        hwnd: windows_sys::Win32::Foundation::HWND,
        width: u32,
        height: u32,
        scale: f32,
        draw: impl FnOnce(Canvas<'_>),
    ) -> Result<bool> {
        unsafe {
            let result = (|| -> Result<()> {
                if self.window.is_none() {
                    let mut properties = D2D1_RENDER_TARGET_PROPERTIES {
                        r#type: if self.software {
                            D2D1_RENDER_TARGET_TYPE_SOFTWARE
                        } else {
                            D2D1_RENDER_TARGET_TYPE_DEFAULT
                        },
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_IGNORE,
                        },
                        dpiX: 96.0 * scale,
                        dpiY: 96.0 * scale,
                        ..Default::default()
                    };
                    let properties_hwnd = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                        hwnd: HWND(hwnd),
                        pixelSize: D2D_SIZE_U { width, height },
                        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                    };
                    let target = self
                        .d2d
                        .CreateHwndRenderTarget(&properties, &properties_hwnd)
                        .or_else(|_| {
                            self.software = true;
                            properties.r#type = D2D1_RENDER_TARGET_TYPE_SOFTWARE;
                            self.d2d
                                .CreateHwndRenderTarget(&properties, &properties_hwnd)
                        })?;
                    let surface = Surface::new(target.cast()?)?;
                    self.window = Some(WindowSurface {
                        surface,
                        window: target,
                        size: (width, height),
                    });
                }
                let window = self.window.as_mut().unwrap();
                if window.size != (width, height) {
                    window.window.Resize(&D2D_SIZE_U { width, height })?;
                    window.size = (width, height);
                }
                window.surface.target.SetDpi(scale * 96.0, scale * 96.0);
                Ok(())
            })();
            if let Err(error) = result {
                self.window = None;
                return Err(error);
            }
            let surface = &self.window.as_ref().unwrap().surface;
            surface.target.BeginDraw();
            draw(Canvas {
                graphics: self,
                surface,
            });
            let result = surface.target.EndDraw(None, None);
            let failure = surface.failure.borrow_mut().take();
            if let Err(error) = result {
                self.window = None;
                if error.code().0 == 0x8899000cu32 as i32 {
                    self.device_failures = self.device_failures.saturating_add(1);
                    if self.device_failures >= 2 {
                        self.software = true;
                    }
                    if self.device_failures > 3 {
                        return Err(error.into());
                    }
                    return Ok(false);
                }
                return Err(error.into());
            }
            if let Some(error) = failure {
                return Err(error);
            }
            Ok(true)
        }
    }

    pub fn release_window(&mut self) {
        self.window = None;
    }

    fn layout(
        &self,
        font: Font,
        text: &str,
        area: &windows_sys::Win32::Foundation::RECT,
        flags: u32,
    ) -> Result<IDWriteTextLayout> {
        let key = TextKey {
            font,
            text: text.into(),
            width: area.right - area.left,
            height: area.bottom - area.top,
            flags,
        };
        if let Some(layout) = self.layouts.borrow().get(&key) {
            return Ok(layout.clone());
        }
        unsafe {
            let text: Vec<u16> = text.encode_utf16().collect();
            let format = &self.formats[&font];
            let layout =
                self.dwrite
                    .CreateTextLayout(&text, format, key.width as f32, key.height as f32)?;
            layout.SetTextAlignment(if flags & 1 != 0 {
                DWRITE_TEXT_ALIGNMENT_CENTER
            } else if flags & 2 != 0 {
                DWRITE_TEXT_ALIGNMENT_TRAILING
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            })?;
            layout.SetParagraphAlignment(if flags & 4 != 0 {
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER
            } else {
                DWRITE_PARAGRAPH_ALIGNMENT_NEAR
            })?;
            layout.SetWordWrapping(if flags & 0x10 != 0 {
                DWRITE_WORD_WRAPPING_WRAP
            } else {
                DWRITE_WORD_WRAPPING_NO_WRAP
            })?;
            if flags & 0x8000 != 0 {
                let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(format)?;
                layout.SetTrimming(
                    &DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    },
                    &ellipsis,
                )?;
            }
            let mut layouts = self.layouts.borrow_mut();
            if layouts.len() >= 256 {
                layouts.clear();
            }
            layouts.insert(key, layout.clone());
            Ok(layout)
        }
    }

    #[cfg(test)]
    pub fn render_bitmap(
        &self,
        width: u32,
        height: u32,
        scale: f32,
        draw: impl FnOnce(Canvas<'_>),
    ) -> Result<Vec<u8>> {
        use windows::Win32::Graphics::Imaging::*;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        unsafe {
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let bitmap = wic.CreateBitmap(
                width,
                height,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapCacheOnLoad,
            )?;
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0 * scale,
                dpiY: 96.0 * scale,
                ..Default::default()
            };
            let target = self.d2d.CreateWicBitmapRenderTarget(&bitmap, &properties)?;
            let surface = Surface::new(target)?;
            surface.target.BeginDraw();
            draw(Canvas {
                graphics: self,
                surface: &surface,
            });
            surface.target.EndDraw(None, None)?;
            if let Some(error) = surface.failure.borrow_mut().take() {
                return Err(error);
            }
            let mut bytes = vec![0u8; (width * height * 4) as usize];
            bitmap.CopyPixels(std::ptr::null(), width * 4, &mut bytes)?;
            Ok(bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curved_icons_are_antialiased_and_text_cache_is_bounded() {
        let graphics = Graphics::new().unwrap();
        let area = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 32,
            bottom: 32,
        };
        let pixels = graphics
            .render_bitmap(32, 32, 1.0, |canvas| {
                canvas.fill(&area, 0xffffff, 1.0);
                canvas.icon(&area, "eye", 0);
            })
            .unwrap();
        let antialiased = pixels
            .chunks_exact(4)
            .filter(|p| p[0] > 0 && p[0] < 255)
            .count();
        assert!(
            antialiased > 20,
            "curved icon edges must have fractional pixel coverage"
        );
        let rect = windows_sys::Win32::Foundation::RECT {
            left: 0,
            top: 0,
            right: 200,
            bottom: 28,
        };
        for i in 0..600 {
            graphics
                .layout(
                    Font::Body,
                    &format!("synthetic-{i}@example.com"),
                    &rect,
                    0x8000,
                )
                .unwrap();
        }
        assert!(graphics.layouts.borrow().len() <= 256);
        let before = graphics.layouts.borrow().len();
        for _ in 0..40 {
            graphics
                .layout(Font::Body, "synthetic-599@example.com", &rect, 0x8000)
                .unwrap();
        }
        assert_eq!(
            graphics.layouts.borrow().len(),
            before,
            "unchanged text must reuse its layout"
        );
    }
}

impl Canvas<'_> {
    pub fn fill(self, area: &windows_sys::Win32::Foundation::RECT, value: u32, opacity: f32) {
        unsafe {
            self.surface
                .target
                .FillRectangle(&rect(area), self.surface.set_color(value, opacity));
        }
    }
    pub fn round(
        self,
        area: &windows_sys::Win32::Foundation::RECT,
        radius: f32,
        value: u32,
        border: bool,
    ) {
        if area.right <= area.left || area.bottom <= area.top {
            return;
        }
        let mut area = rect(area);
        if border {
            area.left += 0.5;
            area.top += 0.5;
            area.right -= 0.5;
            area.bottom -= 0.5;
        }
        let radius = radius
            .min((area.right - area.left) / 2.0)
            .min((area.bottom - area.top) / 2.0);
        let rounded = D2D1_ROUNDED_RECT {
            rect: area,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            let brush = self.surface.set_color(value, 1.0);
            if border {
                self.surface
                    .target
                    .DrawRoundedRectangle(&rounded, brush, 1.0, None);
            } else {
                self.surface.target.FillRoundedRectangle(&rounded, brush);
            }
        }
    }
    pub fn text(
        self,
        font: Font,
        value: u32,
        area: &windows_sys::Win32::Foundation::RECT,
        text: &str,
        flags: u32,
    ) {
        if text.is_empty() || area.right <= area.left || area.bottom <= area.top {
            return;
        }
        match self.graphics.layout(font, text, area, flags) {
            Ok(layout) => unsafe {
                self.surface.target.DrawTextLayout(
                    point(area.left as f32, area.top as f32),
                    &layout,
                    self.surface.set_color(value, 1.0),
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
            },
            Err(error) => *self.surface.failure.borrow_mut() = Some(error),
        }
    }
    pub fn clip(self, area: &windows_sys::Win32::Foundation::RECT) {
        unsafe {
            self.surface
                .target
                .PushAxisAlignedClip(&rect(area), D2D1_ANTIALIAS_MODE_ALIASED);
        }
    }
    pub fn unclip(self) {
        unsafe {
            self.surface.target.PopAxisAlignedClip();
        }
    }

    pub fn ellipse(self, x: f32, y: f32, rx: f32, ry: f32, value: u32, filled: bool) {
        let ellipse = D2D1_ELLIPSE {
            point: point(x, y),
            radiusX: rx,
            radiusY: ry,
        };
        unsafe {
            let brush = self.surface.set_color(value, 1.0);
            if filled {
                self.surface.target.FillEllipse(&ellipse, brush);
            } else {
                self.surface
                    .target
                    .DrawEllipse(&ellipse, brush, 1.6, &self.graphics.stroke);
            }
        }
    }

    pub fn logo(self, area: &windows_sys::Win32::Foundation::RECT) {
        let result = (|| -> Result<()> {
            unsafe {
                if self.surface.logo.borrow().is_none() {
                    let bitmap = self.surface.target.CreateBitmap(
                        D2D_SIZE_U {
                            width: LOGO_SIZE,
                            height: LOGO_SIZE,
                        },
                        Some(self.graphics.logo_pixels.as_ptr().cast()),
                        LOGO_SIZE * 4,
                        &D2D1_BITMAP_PROPERTIES {
                            pixelFormat: D2D1_PIXEL_FORMAT {
                                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                            },
                            dpiX: 96.0,
                            dpiY: 96.0,
                        },
                    )?;
                    *self.surface.logo.borrow_mut() = Some(bitmap);
                }
                self.surface.target.DrawBitmap(
                    self.surface.logo.borrow().as_ref().unwrap(),
                    Some(&rect(area)),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
                Ok(())
            }
        })();
        if let Err(error) = result {
            *self.surface.failure.borrow_mut() = Some(error);
        }
    }

    pub fn icon(self, area: &windows_sys::Win32::Foundation::RECT, kind: &str, value: u32) {
        match self.graphics.icon_geometry(kind) {
            Ok(geometry) => unsafe {
                // All paths share a 24x24 artboard, matching stroke weight and optical centers.
                let size = (area.right - area.left).min(area.bottom - area.top).min(24) as f32;
                let scale = size / 24.0;
                let x = (area.left + area.right) as f32 / 2.0 - size / 2.0;
                let y = (area.top + area.bottom) as f32 / 2.0 - size / 2.0;
                self.surface
                    .target
                    .SetTransform(&windows::Foundation::Numerics::Matrix3x2 {
                        M11: scale,
                        M12: 0.0,
                        M21: 0.0,
                        M22: if kind == "undock" { -scale } else { scale },
                        M31: x,
                        M32: if kind == "undock" { y + size } else { y },
                    });
                self.surface.target.DrawGeometry(
                    &geometry,
                    self.surface.set_color(value, 1.0),
                    if kind == "brand" { 2.4 } else { 1.7 },
                    &self.graphics.stroke,
                );
                self.surface
                    .target
                    .SetTransform(&windows::Foundation::Numerics::Matrix3x2 {
                        M11: 1.0,
                        M12: 0.0,
                        M21: 0.0,
                        M22: 1.0,
                        M31: 0.0,
                        M32: 0.0,
                    });
                if kind == "more" {
                    for xoff in [6.0, 12.0, 18.0] {
                        self.ellipse(
                            x + xoff * scale,
                            y + 12.0 * scale,
                            1.15 * scale,
                            1.15 * scale,
                            value,
                            true,
                        );
                    }
                }
            },
            Err(error) => *self.surface.failure.borrow_mut() = Some(error),
        }
    }

    pub fn shadow(self, area: &windows_sys::Win32::Foundation::RECT, radius: f32) {
        // Small, bounded layers approximate a soft ambient shadow without bitmap buffers.
        unsafe {
            for (spread, opacity) in [(8.0, 0.012), (5.0, 0.018), (3.0, 0.025), (1.0, 0.035)] {
                let base = rect(area);
                let shape = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: base.left - spread,
                        top: base.top - spread + 3.0,
                        right: base.right + spread,
                        bottom: base.bottom + spread + 3.0,
                    },
                    radiusX: radius + spread,
                    radiusY: radius + spread,
                };
                self.surface
                    .target
                    .FillRoundedRectangle(&shape, self.surface.set_color(0x00202820, opacity));
            }
        }
    }

    pub fn gradient_round(
        self,
        area: &windows_sys::Win32::Foundation::RECT,
        radius: f32,
        start: u32,
        end: u32,
    ) {
        let result = (|| -> Result<()> {
            unsafe {
                let cached = self.surface.gradients.borrow().get(&(start, end)).cloned();
                let brush = if let Some(brush) = cached {
                    brush
                } else {
                    let stops = self.surface.target.CreateGradientStopCollection(
                        &[
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: color(start, 1.0),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: color(end, 1.0),
                            },
                        ],
                        D2D1_GAMMA_2_2,
                        D2D1_EXTEND_MODE_CLAMP,
                    )?;
                    let brush = self.surface.target.CreateLinearGradientBrush(
                        &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                            startPoint: point(0.0, 0.0),
                            endPoint: point(1.0, 1.0),
                        },
                        None,
                        &stops,
                    )?;
                    let mut cache = self.surface.gradients.borrow_mut();
                    if cache.len() >= 16 {
                        cache.clear();
                    }
                    cache.insert((start, end), brush.clone());
                    brush
                };
                brush.SetStartPoint(point(area.left as f32, area.top as f32));
                brush.SetEndPoint(point(area.right as f32, area.bottom as f32));
                let radius = radius
                    .min((area.right - area.left) as f32 / 2.0)
                    .min((area.bottom - area.top) as f32 / 2.0);
                self.surface.target.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: rect(area),
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &brush,
                );
                Ok(())
            }
        })();
        if let Err(error) = result {
            *self.surface.failure.borrow_mut() = Some(error);
        }
    }
}

impl Graphics {
    fn icon_geometry(&self, kind: &str) -> Result<ID2D1PathGeometry> {
        if let Some(geometry) = self.icons.borrow().get(kind) {
            return Ok(geometry.clone());
        }
        unsafe {
            let geometry = self.d2d.CreatePathGeometry()?;
            let sink = geometry.Open()?;
            let path = |points: &[(f32, f32)]| {
                if let Some((x, y)) = points.first() {
                    sink.BeginFigure(point(*x, *y), D2D1_FIGURE_BEGIN_HOLLOW);
                }
                for (x, y) in points.iter().skip(1) {
                    sink.AddLine(point(*x, *y));
                }
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            };
            let bezier = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: point(a.0, a.1),
                    point2: point(b.0, b.1),
                    point3: point(c.0, c.1),
                });
            };
            match kind {
                "settings" => {
                    let mut points = Vec::new();
                    for tooth in 0..8 {
                        for (step, radius) in [
                            (-0.44, 7.6),
                            (-0.26, 7.6),
                            (-0.20, 10.0),
                            (0.20, 10.0),
                            (0.26, 7.6),
                            (0.44, 7.6),
                        ] {
                            let angle = (tooth as f32 + step) * std::f32::consts::TAU / 8.0;
                            points.push((12.0 + radius * angle.cos(), 12.0 + radius * angle.sin()));
                        }
                    }
                    points.push(points[0]);
                    path(&points);
                    sink.BeginFigure(point(15.0, 12.0), D2D1_FIGURE_BEGIN_HOLLOW);
                    bezier((15.0, 16.0), (9.0, 16.0), (9.0, 12.0));
                    bezier((9.0, 8.0), (15.0, 8.0), (15.0, 12.0));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                }
                "brand" => {
                    sink.BeginFigure(point(3.0, 10.0), D2D1_FIGURE_BEGIN_HOLLOW);
                    bezier((3.0, 6.7), (5.7, 4.0), (9.0, 4.0));
                    sink.AddLine(point(20.0, 4.0));
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    path(&[(16.0, 0.5), (20.0, 4.0), (16.0, 7.5)]);
                    sink.BeginFigure(point(21.0, 14.0), D2D1_FIGURE_BEGIN_HOLLOW);
                    bezier((21.0, 17.3), (18.3, 20.0), (15.0, 20.0));
                    sink.AddLine(point(4.0, 20.0));
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    path(&[(8.0, 16.5), (4.0, 20.0), (8.0, 23.5)]);
                }
                "eye" | "hidden" => {
                    sink.BeginFigure(point(2.0, 12.0), D2D1_FIGURE_BEGIN_HOLLOW);
                    bezier((7.0, 3.0), (17.0, 3.0), (22.0, 12.0));
                    bezier((17.0, 21.0), (7.0, 21.0), (2.0, 12.0));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    sink.BeginFigure(point(15.0, 12.0), D2D1_FIGURE_BEGIN_HOLLOW);
                    bezier((15.0, 16.0), (9.0, 16.0), (9.0, 12.0));
                    bezier((9.0, 8.0), (15.0, 8.0), (15.0, 12.0));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    if kind == "hidden" {
                        path(&[(3.0, 21.0), (21.0, 3.0)]);
                    }
                }
                "refresh" => {
                    sink.BeginFigure(point(20.0, 10.0), D2D1_FIGURE_BEGIN_HOLLOW);
                    bezier((18.0, 2.0), (5.0, 1.0), (3.0, 10.0));
                    bezier((0.0, 21.0), (17.0, 24.0), (20.0, 15.0));
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    path(&[(20.0, 3.0), (20.0, 10.0), (13.0, 10.0)]);
                }
                "expand" => {
                    path(&[(4.0, 14.0), (4.0, 20.0), (10.0, 20.0)]);
                    path(&[(4.0, 20.0), (20.0, 4.0), (14.0, 4.0)]);
                    path(&[(20.0, 4.0), (20.0, 10.0)]);
                }
                "shrink" => {
                    path(&[(4.0, 20.0), (10.0, 14.0)]);
                    path(&[(4.0, 14.0), (10.0, 14.0), (10.0, 20.0)]);
                    path(&[(20.0, 4.0), (14.0, 10.0)]);
                    path(&[(14.0, 4.0), (14.0, 10.0), (20.0, 10.0)]);
                }
                "dock" | "undock" => {
                    path(&[(5.0, 15.0), (12.0, 8.0), (19.0, 15.0)]);
                    path(&[(5.0, 4.0), (19.0, 4.0)]);
                }
                "close" => {
                    path(&[(6.0, 6.0), (18.0, 18.0)]);
                    path(&[(6.0, 18.0), (18.0, 6.0)]);
                }
                "min" => path(&[(5.0, 12.0), (19.0, 12.0)]),
                "check" => path(&[(5.0, 12.0), (10.0, 17.0), (20.0, 7.0)]),
                "arrow" => path(&[(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)]),
                "plus" => {
                    path(&[(5.0, 12.0), (19.0, 12.0)]);
                    path(&[(12.0, 5.0), (12.0, 19.0)]);
                }
                _ => {}
            }
            sink.Close()?;
            if self.icons.borrow().len() < 32 {
                self.icons
                    .borrow_mut()
                    .insert(kind.into(), geometry.clone());
            }
            Ok(geometry)
        }
    }
}
