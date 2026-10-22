//! Native per-pixel-alpha outer shadow for our frameless window.
//! It never receives focus/input and has no timer. Only geometry/DPI changes upload
//! a new bitmap; movement/stacking reuses the compositor's stored image.
use anyhow::{Context, Result};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi as gdi;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

pub(super) const SHADOW_PADDING: f32 = 24.0;
const CORNER_RADIUS: f32 = 12.0;
const SHADOW_CLASS: &str = "CodexSwitchShadowWindow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Geometry {
    pub width: i32,
    pub height: i32,
    pub padding: i32,
    pub radius: i32,
}
impl Geometry {
    pub fn new(body_width: i32, body_height: i32, scale: f32) -> Self {
        let padding = (SHADOW_PADDING * scale).round().max(1.0) as i32;
        Self {
            width: body_width + 2 * padding,
            height: body_height + 2 * padding,
            padding,
            radius: (CORNER_RADIUS * scale).round().max(1.0) as i32,
        }
    }
    fn body_size(self) -> (i32, i32) {
        (
            self.width - 2 * self.padding,
            self.height - 2 * self.padding,
        )
    }
}

/// Signed distance from a pixel center to a round rect; no dependence on HWND/DWM.
pub(super) fn rounded_distance(x: f32, y: f32, width: f32, height: f32, radius: f32) -> f32 {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    let qx = (x - width / 2.0).abs() - (width / 2.0 - radius);
    let qy = (y - height / 2.0).abs() - (height / 2.0 - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// Premultiplied BGRA black with a smooth alpha falloff, symmetric on all sides.
/// The large interior remains entirely zero, so no opaque rectangle can appear.
pub(super) fn shadow_pixels(shape: Geometry) -> Vec<u8> {
    let mut pixels = vec![0u8; shape.width as usize * shape.height as usize * 4];
    let (body_width, body_height) = shape.body_size();
    let radius = shape.radius as f32;
    let scale = shape.padding as f32 / SHADOW_PADDING;
    for y in 0..shape.height {
        let body_y = (y - shape.padding) as f32 + 0.5;
        for x in 0..shape.width {
            let body_x = (x - shape.padding) as f32 + 0.5;
            // Avoid distance/exp work for the completely transparent interior.
            if body_x >= radius
                && body_x < body_width as f32 - radius
                && body_y >= 0.0
                && body_y < body_height as f32
            {
                continue;
            }
            let d = rounded_distance(
                body_x,
                body_y,
                body_width as f32,
                body_height as f32,
                radius,
            );
            if d <= 0.0 {
                continue;
            }
            let d = d / scale;
            let alpha = (255.0
                * (0.13 * (-0.5 * (d / 8.0).powi(2)).exp()
                    + 0.10 * (-0.5 * (d / 2.5).powi(2)).exp()))
            .round() as u8;
            let offset = (y as usize * shape.width as usize + x as usize) * 4;
            pixels[offset + 3] = alpha;
        }
    }
    pixels
}

pub(super) struct WindowShadow {
    hwnd: HWND,
    geometry: Option<Geometry>,
    visible: bool,
}
impl Drop for WindowShadow {
    fn drop(&mut self) {
        unsafe {
            if !self.hwnd.is_null() {
                wm::DestroyWindow(self.hwnd);
            }
        }
    }
}

unsafe extern "system" fn shadow_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        wm::WM_NCHITTEST => wm::HTTRANSPARENT as LRESULT,
        wm::WM_MOUSEACTIVATE => wm::MA_NOACTIVATE as LRESULT,
        wm::WM_ERASEBKGND => 1,
        _ => wm::DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl WindowShadow {
    pub fn new() -> Result<Self> {
        unsafe {
            let instance =
                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            let name: Vec<u16> = SHADOW_CLASS.encode_utf16().chain([0]).collect();
            let mut class: wm::WNDCLASSEXW = std::mem::zeroed();
            class.cbSize = std::mem::size_of::<wm::WNDCLASSEXW>() as u32;
            class.hInstance = instance;
            class.lpszClassName = name.as_ptr();
            class.lpfnWndProc = Some(shadow_proc);
            if wm::RegisterClassExW(&class) == 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error()
                    != Some(windows_sys::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS as i32)
                {
                    return Err(error).context("注册透明阴影窗口失败");
                }
            }
            // No owner: owned windows must be ABOVE their owner. This tool window
            // is explicitly placed immediately BEHIND the body, without activation.
            let hwnd = wm::CreateWindowExW(
                wm::WS_EX_LAYERED
                    | wm::WS_EX_TRANSPARENT
                    | wm::WS_EX_NOACTIVATE
                    | wm::WS_EX_TOOLWINDOW,
                name.as_ptr(),
                std::ptr::null(),
                wm::WS_POPUP,
                0,
                0,
                1,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                return Err(std::io::Error::last_os_error()).context("创建透明阴影窗口失败");
            }
            Ok(Self {
                hwnd,
                geometry: None,
                visible: false,
            })
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                wm::ShowWindow(self.hwnd, wm::SW_HIDE);
            }
            self.visible = false;
        }
    }

    fn release(&mut self) {
        // Destroy the hidden layered surface so the compositor releases its image.
        // No large bitmap remains while minimized, in the tray, or offscreen-hidden.
        if !self.hwnd.is_null() {
            unsafe {
                wm::DestroyWindow(self.hwnd);
            }
        }
        self.hwnd = std::ptr::null_mut();
        self.geometry = None;
        self.visible = false;
    }

    pub unsafe fn sync(&mut self, body: HWND, scale: f32, enabled: bool) -> Result<()> {
        if !enabled || wm::IsWindowVisible(body) == 0 || wm::IsIconic(body) != 0 {
            self.release();
            return Ok(());
        }
        if self.hwnd.is_null() {
            *self = Self::new()?;
        }
        let mut bounds: RECT = std::mem::zeroed();
        if wm::GetWindowRect(body, &mut bounds) == 0 {
            return Err(std::io::Error::last_os_error()).context("读取窗口尺寸失败");
        }
        let shape = Geometry::new(
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            scale,
        );
        if self.geometry != Some(shape) {
            self.hide();
            // Region clipping makes body corners match the shadow on Windows 10/11.
            // It changes no client coordinates or hit targets. Windows owns it on success.
            let (w, h) = shape.body_size();
            let region =
                gdi::CreateRoundRectRgn(0, 0, w + 1, h + 1, shape.radius * 2, shape.radius * 2);
            if region.is_null() {
                return Err(std::io::Error::last_os_error()).context("创建圆角轮廓失败");
            }
            if gdi::SetWindowRgn(body, region, 1) == 0 {
                gdi::DeleteObject(region as _);
                return Err(std::io::Error::last_os_error()).context("设置圆角轮廓失败");
            }
            let pixels = shadow_pixels(shape);
            upload(self.hwnd, shape, &pixels)?;
            self.geometry = Some(shape);
        }
        if wm::SetWindowPos(
            self.hwnd,
            body,
            bounds.left - shape.padding,
            bounds.top - shape.padding,
            shape.width,
            shape.height,
            wm::SWP_NOACTIVATE | wm::SWP_SHOWWINDOW,
        ) == 0
        {
            self.hide();
            return Err(std::io::Error::last_os_error()).context("同步窗口阴影位置失败");
        }
        self.visible = true;
        Ok(())
    }
}

/// The compositor keeps its own image; all temporary DC/bitmap memory is released
/// after upload. Pure window moves do not allocate or upload another image.
unsafe fn upload(hwnd: HWND, shape: Geometry, pixels: &[u8]) -> Result<()> {
    let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
    if dc.is_null() {
        return Err(std::io::Error::last_os_error()).context("创建阴影绘制上下文失败");
    }
    let mut info: gdi::BITMAPINFO = std::mem::zeroed();
    info.bmiHeader.biSize = std::mem::size_of::<gdi::BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = shape.width;
    info.bmiHeader.biHeight = -shape.height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = gdi::BI_RGB;
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap = gdi::CreateDIBSection(
        dc,
        &info,
        gdi::DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if bitmap.is_null() || bits.is_null() {
        let error = std::io::Error::last_os_error();
        if !bitmap.is_null() {
            gdi::DeleteObject(bitmap as _);
        }
        gdi::DeleteDC(dc);
        return Err(error).context("创建透明阴影位图失败");
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast(), pixels.len());
    let previous = gdi::SelectObject(dc, bitmap as _);
    if previous.is_null() || previous as isize == -1 {
        let error = std::io::Error::last_os_error();
        gdi::DeleteObject(bitmap as _);
        gdi::DeleteDC(dc);
        return Err(error).context("选择透明阴影位图失败");
    }
    let size = SIZE {
        cx: shape.width,
        cy: shape.height,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = gdi::BLENDFUNCTION {
        BlendOp: gdi::AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: gdi::AC_SRC_ALPHA as u8,
    };
    let result = wm::UpdateLayeredWindow(
        hwnd,
        std::ptr::null_mut(),
        std::ptr::null(),
        &size,
        dc,
        &source,
        0,
        &blend,
        wm::ULW_ALPHA,
    );
    let error = if result == 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };
    gdi::SelectObject(dc, previous);
    gdi::DeleteObject(bitmap as _);
    gdi::DeleteDC(dc);
    if let Some(error) = error {
        return Err(error).context("提交透明阴影图像失败");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_fades_to_transparent_on_all_four_sides_and_is_empty_inside() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let shape = Geometry::new((350.0 * scale) as i32, (546.0 * scale) as i32, scale);
            let pixels = shadow_pixels(shape);
            let a =
                |x: i32, y: i32| pixels[(y as usize * shape.width as usize + x as usize) * 4 + 3];
            let cx = shape.width / 2;
            let cy = shape.height / 2;
            let pad = shape.padding;
            let near = a(cx, pad - 1);
            let far = a(cx, 0);
            assert!(near > 40 && near < 90);
            assert_eq!(far, 0, "outer edge must be fully transparent");
            assert_eq!(a(pad - 1, cy), near);
            assert_eq!(a(shape.width - pad, cy), near);
            assert_eq!(a(cx, shape.height - pad), near);
            assert_eq!(a(cx, cy), 0, "no opaque image behind the client");
            let mut previous = near;
            for y in (0..pad).rev() {
                let alpha = a(cx, y);
                assert!(alpha <= previous);
                previous = alpha;
            }
            assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]));
            let levels = pixels
                .chunks_exact(4)
                .map(|p| p[3])
                .collect::<std::collections::HashSet<_>>();
            assert!(
                levels.len() >= 35,
                "shadow is a continuous falloff, not a hard outline"
            );
        }
    }
    #[test]
    fn native_alpha_upload_succeeds_without_showing_or_activating_a_window() {
        // A separate, synthetic, permanently hidden HWND exercises the actual
        // UpdateLayeredWindow path. It is not the app and never runs its workers.
        let mut shadow = WindowShadow::new().unwrap();
        let hwnd = shadow.hwnd;
        let shape = Geometry::new(350, 546, 1.0);
        let pixels = shadow_pixels(shape);
        unsafe {
            assert_eq!(wm::IsWindowVisible(hwnd), 0);
            let foreground = wm::GetForegroundWindow();
            upload(hwnd, shape, &pixels).unwrap();
            assert_eq!(wm::IsWindowVisible(hwnd), 0);
            assert_eq!(
                wm::GetForegroundWindow(),
                foreground,
                "upload must never steal focus"
            );
            let mut bounds: RECT = std::mem::zeroed();
            assert_ne!(wm::GetWindowRect(hwnd, &mut bounds), 0);
            assert_eq!(
                (bounds.right - bounds.left, bounds.bottom - bounds.top),
                (shape.width, shape.height)
            );
            let style = wm::GetWindowLongPtrW(hwnd, wm::GWL_EXSTYLE) as u32;
            for flag in [
                wm::WS_EX_LAYERED,
                wm::WS_EX_TRANSPARENT,
                wm::WS_EX_NOACTIVATE,
                wm::WS_EX_TOOLWINDOW,
            ] {
                assert_ne!(style & flag, 0);
            }
        }
        shadow.release();
        assert_eq!(
            unsafe { wm::IsWindow(hwnd) },
            0,
            "hidden surface and image are destroyed"
        );
        assert!(shadow.geometry.is_none());
    }
}
