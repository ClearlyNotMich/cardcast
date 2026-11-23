//! Checks capture on the runner: its only monitor is the source monitor and the colour comes from a test window.

use super::*;
use crate::mirror::sdr_from_hdr;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11DeviceContext,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R32G32B32A32_FLOAT;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SW_SHOWNOACTIVATE, SetCursorPos, ShowWindow, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

/// A device on the first graphics card and that card's first output, the runner's monitor.
fn device() -> (ID3D11Device, ID3D11DeviceContext1, IDXGIOutput) {
    // SAFETY: plain factory creation and enumeration.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
    // SAFETY: as above.
    let adapter = unsafe { factory.EnumAdapters1(0) }.unwrap();
    // SAFETY: as above.
    let output = unsafe { adapter.EnumOutputs(0) }.unwrap();
    let (mut device, mut context) = (None, None);
    // SAFETY: out pointers are valid locals; an explicit adapter needs D3D_DRIVER_TYPE_UNKNOWN.
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            Default::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .unwrap();
    let context: ID3D11DeviceContext = context.unwrap();
    (device.unwrap(), context.cast().unwrap(), output)
}

/// An 8-bit render target of `size`.
fn render_target(
    device: &ID3D11Device,
    size: (u32, u32),
) -> (ID3D11Texture2D, ID3D11RenderTargetView) {
    let description = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0.cast_unsigned(),
        ..Default::default()
    };
    let (mut texture, mut view) = (None, None);
    // SAFETY: description outlives the calls; out pointers are valid locals.
    unsafe {
        device
            .CreateTexture2D(&description, None, Some(&mut texture))
            .unwrap();
        device
            .CreateRenderTargetView(texture.as_ref().unwrap(), None, Some(&mut view))
            .unwrap();
    }
    (texture.unwrap(), view.unwrap())
}

/// The BGRA pixel at (x, y) of an 8-bit `texture`.
fn pixel(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext1,
    texture: &ID3D11Texture2D,
    (x, y): (u32, u32),
) -> [u8; 4] {
    let mut description = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: description is a valid out pointer.
    unsafe { texture.GetDesc(&mut description) };
    description.Usage = D3D11_USAGE_STAGING;
    description.BindFlags = 0;
    description.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0.cast_unsigned();
    let mut staging = None;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: the staging copy has the texture's size and format; the mapping is read inside its pitch, then unmapped.
    unsafe {
        device
            .CreateTexture2D(&description, None, Some(&mut staging))
            .unwrap();
        let staging = staging.unwrap();
        context.CopyResource(&staging, texture);
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .unwrap();
        let at = mapped.RowPitch as usize * y as usize + x as usize * 4;
        let bytes = std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(at), 4);
        let pixel = bytes.try_into().unwrap();
        context.Unmap(&staging, 0);
        pixel
    }
}

#[test]
fn hdr_shader_matches_the_sdr_maths() {
    let (device, context, _) = device();
    let linear = [0.0f32, 0.01, 0.5, 1.25, 2.5, 6.0, -0.3];
    let scale = white_scale(2500);
    let width = u32::try_from(linear.len()).unwrap();
    let texels: Vec<u8> = linear
        .iter()
        .flat_map(|&value| [value, value, value, 1.0])
        .flat_map(f32::to_ne_bytes)
        .collect();
    let picture = Texture::new(
        &device,
        (width, 1),
        DXGI_FORMAT_R32G32B32A32_FLOAT,
        Some((&texels, width * 16)),
    )
    .unwrap();
    let (texture, target) = render_target(&device, (width, 1));
    let painter = Painter::new(&device).unwrap();
    painter.paint(
        &context,
        &target,
        &picture.view,
        [0.0, 0.0, width as f32, 1.0],
        Paint::Hdr(scale),
    );
    for (x, &value) in (0..width).zip(&linear) {
        let expected = (sdr_from_hdr(value, scale) * 255.0).round();
        let got = pixel(&device, &context, &texture, (x, 0));
        for channel in &got[..3] {
            assert!(
                (f32::from(*channel) - expected).abs() <= 1.0,
                "linear {value}: shader gave {channel}, maths gives {expected}"
            );
        }
    }
}

extern "system" fn colour_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwards the message this procedure received unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// A topmost window filled with `colour` covering `monitor`, destroyed on drop.
struct ColourWindow(HWND);

impl ColourWindow {
    fn cover(monitor: &crate::guard::Monitor, colour: COLORREF) -> Self {
        // SAFETY: a null module name returns this executable's handle.
        let instance = unsafe { GetModuleHandleW(None) }.unwrap();
        let class = WNDCLASSW {
            lpfnWndProc: Some(colour_window_proc),
            hInstance: instance.into(),
            // SAFETY: plain brush creation; the class keeps it for the test's lifetime.
            hbrBackground: unsafe { CreateSolidBrush(colour) },
            lpszClassName: w!("cardcast capture test"),
            ..Default::default()
        };
        // SAFETY: class points at static strings and a valid window procedure.
        unsafe { RegisterClassW(&class) };
        // SAFETY: the class was registered above.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("cardcast capture test"),
                w!("cardcast capture test"),
                WS_POPUP,
                monitor.left,
                monitor.top,
                i32::try_from(monitor.width).unwrap(),
                i32::try_from(monitor.height).unwrap(),
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .unwrap();
        // SAFETY: plain calls on the window created above.
        unsafe {
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            let _ = UpdateWindow(window);
        }
        Self(window)
    }

    fn pump(&self) {
        let mut message = MSG::default();
        // SAFETY: message is a valid MSG; messages are dispatched on the thread that created the window.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: message came from PeekMessageW on this thread.
            unsafe { DispatchMessageW(&message) };
        }
    }
}

impl Drop for ColourWindow {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[test]
fn duplicating_the_monitor_draws_its_colour() {
    let _duplicating = duplicating();
    let monitor = crate::guard::monitors().unwrap().remove(0);
    // Keep the pointer away from the pixel that is checked.
    // SAFETY: plain call.
    let _ = unsafe { SetCursorPos(monitor.left, monitor.top) };
    // RGB(200, 40, 120); COLORREF is 0x00BBGGRR.
    let window = ColourWindow::cover(&monitor, COLORREF(0x0078_28c8));
    let (device, context, output) = device();
    let mut desktop = Desktop::open(&device, &output, &monitor.device_path).unwrap();
    let size = (monitor.width, monitor.height);
    let (texture, target) = render_target(&device, size);
    let centre = (size.0 / 2, size.1 / 2);
    let mut got = [0; 4];
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        window.pump();
        let fit = crate::mirror::fit(desktop.size(), size);
        desktop.draw(&context, &target, fit).unwrap();
        got = pixel(&device, &context, &texture, centre);
        if got[..3] == [120, 40, 200] {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(desktop.size(), size, "captured picture has the wrong size");
    assert_eq!(&got[..3], &[120, 40, 200], "captured colour (BGR)");
}
