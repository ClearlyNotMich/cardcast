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
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};
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
    device_on(&adapter, output)
}

/// The output showing the monitor with GDI name `gdi_name`, on whichever graphics card owns it.
fn output_named(gdi_name: &str) -> (IDXGIAdapter1, IDXGIOutput) {
    // SAFETY: plain factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.unwrap();
    for adapter_index in 0.. {
        // SAFETY: plain enumeration; it fails past the last graphics card, and so does the test.
        let adapter = unsafe { factory.EnumAdapters1(adapter_index) }
            .unwrap_or_else(|_| panic!("no output shows {gdi_name}"));
        for output_index in 0.. {
            // SAFETY: as above, per output.
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                break;
            };
            // SAFETY: plain description read.
            let name = unsafe { output.GetDesc() }.unwrap().DeviceName;
            if crate::guard::wide_to_string(&name).eq_ignore_ascii_case(gdi_name) {
                return (adapter, output);
            }
        }
    }
    unreachable!()
}

/// A device on `adapter`, returned with `output`.
fn device_on(
    adapter: &IDXGIAdapter1,
    output: IDXGIOutput,
) -> (ID3D11Device, ID3D11DeviceContext1, IDXGIOutput) {
    let (mut device, mut context) = (None, None);
    // SAFETY: out pointers are valid locals; an explicit adapter needs D3D_DRIVER_TYPE_UNKNOWN.
    unsafe {
        D3D11CreateDevice(
            adapter,
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
        1,
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

#[test]
fn shaders_compile_once_per_process() {
    let (device, _, _) = device();
    // Each painter stands for a mirror start or restart.
    for _ in 0..3 {
        Painter::new(&device).unwrap();
    }
    let compiled = COMPILES.load(std::sync::atomic::Ordering::Relaxed);
    println!("{compiled} shader compiles");
    // Three shaders: the vertex shader, copy and HDR.
    assert_eq!(compiled, 3, "shaders compiled more than once");
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
    let (device, context, output) = device();
    let size = (monitor.width, monitor.height);
    duplicate_and_check_colour(&monitor, size, &device, &context, &output);
}

#[test]
fn second_monitor_duplicating_it_draws_its_colour() {
    let Some(monitors) = crate::guard::second_monitor::monitors(2) else {
        return;
    };
    let second = &monitors[1];
    let _duplicating = duplicating();
    let (adapter, output) = output_named(&second.gdi_name);
    let (device, context, output) = device_on(&adapter, output);
    let size = (second.width, second.height);
    duplicate_and_check_colour(second, size, &device, &context, &output);
}

#[test]
fn second_monitor_rotated_monitor_mirrors_upright_and_fitted() {
    use crate::guard::second_monitor::{Rotated, wait_for};
    let Some(monitors) = crate::guard::second_monitor::monitors(2) else {
        return;
    };
    // The rightmost, so turning it leaves no gap between monitors.
    let turned = monitors.iter().max_by_key(|monitor| monitor.left).unwrap();
    let _duplicating = duplicating();
    let _rotated = Rotated::portrait(&turned.device_path);
    let mut portrait = None;
    wait_for(
        "the monitor list to show the monitor in portrait",
        Duration::from_secs(10),
        || {
            portrait = crate::guard::monitors().ok().and_then(|monitors| {
                monitors.into_iter().find(|monitor| {
                    monitor.device_path == turned.device_path && monitor.height > monitor.width
                })
            });
            portrait.is_some()
        },
    );
    let portrait = portrait.unwrap();
    let (adapter, output) = output_named(&portrait.gdi_name);
    let (device, context, output) = device_on(&adapter, output);
    // A landscape card display, so the portrait picture is pillarboxed.
    let card_display = (portrait.height, portrait.width);
    duplicate_and_check_colour(&portrait, card_display, &device, &context, &output);
}

/// Covers the top-left quarter of `monitor` with a colour, duplicates `output` (that monitor's)
/// into a `card_display` of that size, and checks the picture has the monitor's size and shows
/// the colour upright: in the top-left quarter of the fitted picture and no other.
fn duplicate_and_check_colour(
    monitor: &crate::guard::Monitor,
    card_display: (u32, u32),
    device: &ID3D11Device,
    context: &ID3D11DeviceContext1,
    output: &IDXGIOutput,
) {
    // Keep the pointer away from the pixels that are checked.
    // SAFETY: plain call.
    let _ = unsafe { SetCursorPos(monitor.left, monitor.top) };
    let quarter = crate::guard::Monitor {
        width: monitor.width / 2,
        height: monitor.height / 2,
        layout_size: (monitor.width / 2, monitor.height / 2),
        ..monitor.clone()
    };
    // RGB(200, 40, 120); COLORREF is 0x00BBGGRR.
    let window = ColourWindow::cover(&quarter, COLORREF(0x0078_28c8));
    let colour = [120, 40, 200];
    let mut desktop = Desktop::open(device, output, &monitor.device_path).unwrap();
    let (texture, target) = render_target(device, card_display);
    // The centres of the top-left, top-right, bottom-left and bottom-right quarters.
    let quarters = |fit: crate::mirror::Fit| {
        [(1, 1), (3, 1), (1, 3), (3, 3)]
            .map(|(x, y)| (fit.left + fit.width * x / 4, fit.top + fit.height * y / 4))
    };
    let expected = [true, false, false, false];
    let (mut fit, mut got) = (crate::mirror::fit((0, 0), card_display), [[0; 4]; 4]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        window.pump();
        fit = crate::mirror::fit(desktop.size(), card_display);
        desktop.draw(context, &target, fit).unwrap();
        got = quarters(fit).map(|at| pixel(device, context, &texture, at));
        if got.map(|pixel| pixel[..3] == colour) == expected {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    crate::guard::second_monitor::log(&format!(
        "{} {}x{}: picture {:?} (rotation {}), fit {fit:?}, quarters (BGRA) {got:?}",
        monitor.gdi_name,
        monitor.width,
        monitor.height,
        desktop.size(),
        desktop.rotation
    ));
    assert_eq!(
        desktop.size(),
        (monitor.width, monitor.height),
        "captured picture has the wrong size"
    );
    assert_eq!(
        got.map(|pixel| pixel[..3] == colour),
        expected,
        "colour (BGR {colour:?}) in the top-left, top-right, bottom-left and bottom-right quarter: {got:?}"
    );
}
