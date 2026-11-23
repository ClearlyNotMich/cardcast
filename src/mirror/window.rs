//! The mirror window: a borderless, topmost, no-activate window covering the card display,
//! drawn once per refresh through a flip-model swap chain on its own thread.

use super::{Fit, Mirror, Output, find_output, fit};
use crate::guard::Monitor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11DeviceContext1, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGISwapChain2,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MA_NOACTIVATE, MSG,
    MsgWaitForMultipleObjects, PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW,
    QS_ALLINPUT, RegisterClassW, SW_SHOWNOACTIVATE, SetWindowDisplayAffinity, ShowWindow,
    WDA_EXCLUDEFROMCAPTURE, WM_MOUSEACTIVATE, WM_QUIT, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Interface, w};

/// What the mirror window draws once per refresh. #7 swaps the test pattern for captured frames.
pub trait Picture {
    /// Size of the newest picture in pixels, for the aspect fit.
    fn size(&self) -> (u32, u32);
    /// Draws the newest picture into `fit` on `target`, the swap chain's back buffer;
    /// the rest of the back buffer is already black.
    fn draw(
        &mut self,
        context: &ID3D11DeviceContext1,
        target: &ID3D11RenderTargetView,
        fit: Fit,
    ) -> windows::core::Result<()>;
}

/// Colour bars, so the owner can check the card display end to end before capture exists.
struct TestPattern;

impl Picture for TestPattern {
    fn size(&self) -> (u32, u32) {
        (1920, 1080)
    }

    fn draw(
        &mut self,
        context: &ID3D11DeviceContext1,
        target: &ID3D11RenderTargetView,
        fit: Fit,
    ) -> windows::core::Result<()> {
        const BARS: [[f32; 4]; 8] = [
            [1.0, 1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0, 1.0],
            [0.0, 1.0, 1.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [1.0, 0.0, 1.0, 1.0],
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [0.25, 0.25, 0.25, 1.0],
        ];
        let edge = |bar: usize| {
            let bar = u64::try_from(bar).unwrap_or(0);
            let offset = u64::from(fit.width) * bar / 8;
            i32::try_from(u64::from(fit.left) + offset).unwrap_or(i32::MAX)
        };
        let top = i32::try_from(fit.top).unwrap_or(0);
        let bottom = i32::try_from(fit.top + fit.height).unwrap_or(0);
        for (bar, colour) in BARS.iter().enumerate() {
            let rect = RECT {
                left: edge(bar),
                top,
                right: edge(bar + 1),
                bottom,
            };
            // SAFETY: target is a live render target view of this context's device.
            unsafe { context.ClearView(target, colour, Some(&[rect])) };
        }
        Ok(())
    }
}

/// The mirror thread and what tests read from it.
struct Running {
    thread_id: u32,
    thread: JoinHandle<()>,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the Windows tests"))]
    window: isize,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the Windows tests"))]
    presents: Arc<AtomicU64>,
}

#[derive(Default)]
pub struct MirrorWindow {
    running: Option<Running>,
}

impl Drop for MirrorWindow {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Mirror for MirrorWindow {
    fn start(&mut self, _source_monitor: &Monitor, card_display: &Monitor) -> Result<(), String> {
        self.stop();
        let (started_sender, started) = mpsc::channel();
        let presents = Arc::new(AtomicU64::new(0));
        let thread = {
            let card_display = card_display.clone();
            let presents = presents.clone();
            std::thread::spawn(move || run(&card_display, &presents, &started_sender))
        };
        match started.recv() {
            Ok(Ok((thread_id, window))) => {
                self.running = Some(Running {
                    thread_id,
                    thread,
                    window,
                    presents,
                });
                Ok(())
            }
            Ok(Err(message)) => {
                let _ = thread.join();
                Err(message)
            }
            Err(_) => {
                let _ = thread.join();
                Err("The mirror could not start.".into())
            }
        }
    }

    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            // SAFETY: posting to a thread id has no memory effects; a dead thread just fails the post.
            let posted =
                unsafe { PostThreadMessageW(running.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            if posted.is_ok() {
                let _ = running.thread.join();
            }
        }
    }
}

/// Mirror thread: opens the window, reports its thread id and window, then draws until WM_QUIT.
fn run(
    card_display: &Monitor,
    presents: &AtomicU64,
    started: &Sender<Result<(u32, isize), String>>,
) {
    let screen = match Screen::open(card_display) {
        Ok(screen) => screen,
        Err(message) => {
            let _ = started.send(Err(message));
            return;
        }
    };
    let mut message = MSG::default();
    // SAFETY: message is a valid MSG; PM_NOREMOVE only creates this thread's queue so WM_QUIT can be posted.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: plain call returning this thread's id.
    let thread_id = unsafe { GetCurrentThreadId() };
    if started
        .send(Ok((thread_id, screen.window.0.0 as isize)))
        .is_err()
    {
        return;
    }
    // shortcut: a lost device or failed present just closes the window; #7 adds recovery and reporting.
    let _ = screen.draw_until_quit(&mut TestPattern, presents);
}

/// Destroys the mirror window when dropped.
struct Window(HWND);

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// Closes the swap chain's frame latency handle when dropped.
struct Waitable(HANDLE);

impl Drop for Waitable {
    fn drop(&mut self) {
        // SAFETY: the handle came from GetFrameLatencyWaitableObject and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The window and swap chain on the card display. Fields drop in order, so the window goes last.
struct Screen {
    waitable: Waitable,
    target: ID3D11RenderTargetView,
    swap_chain: IDXGISwapChain2,
    context: ID3D11DeviceContext1,
    size: (u32, u32),
    window: Window,
}

impl Screen {
    fn open(card_display: &Monitor) -> Result<Self, String> {
        let monitors = crate::guard::monitors()?;
        // SAFETY: plain factory creation.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1() }
            .map_err(|_| "Could not start DirectX (DXGI factory).".to_owned())?;
        let outputs = outputs(&factory);
        let output = find_output(&card_display.device_path, &monitors, &outputs)
            .ok_or("The card display is not connected to any graphics card.")?;
        let card_display = monitors
            .iter()
            .find(|monitor| monitor.device_path == card_display.device_path)
            .ok_or("The card display is not connected.")?;
        let failed =
            |what: &str| format!("Could not start the mirror on the card display ({what}).");
        // SAFETY: the index came from EnumAdapters1 on this factory just above.
        let adapter = unsafe { factory.EnumAdapters1(output.adapter) }
            .map_err(|_| failed("graphics card"))?;
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
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
        .map_err(|_| failed("Direct3D device"))?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(failed("Direct3D device"));
        };
        let context: ID3D11DeviceContext1 = context.cast().map_err(|_| failed("Direct3D 11.1"))?;

        let window = Window(create_window(card_display).ok_or_else(|| failed("window"))?);
        let size = (card_display.width, card_display.height);
        let description = DXGI_SWAP_CHAIN_DESC1 {
            Width: size.0,
            Height: size.1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
                .0
                .cast_unsigned(),
            ..Default::default()
        };
        // SAFETY: device and window are live; description outlives the call.
        let swap_chain: IDXGISwapChain2 =
            unsafe { factory.CreateSwapChainForHwnd(&device, window.0, &description, None, None) }
                .and_then(|swap_chain| swap_chain.cast())
                .map_err(|_| failed("swap chain"))?;
        // SAFETY: the swap chain was created with the waitable-object flag, which these calls need.
        let waitable = unsafe {
            swap_chain
                .SetMaximumFrameLatency(1)
                .map_err(|_| failed("frame latency"))?;
            Waitable(swap_chain.GetFrameLatencyWaitableObject())
        };
        // SAFETY: buffer 0 of a flip-model swap chain is the current back buffer.
        let back_buffer: ID3D11Texture2D =
            unsafe { swap_chain.GetBuffer(0) }.map_err(|_| failed("back buffer"))?;
        let mut target = None;
        // SAFETY: back_buffer belongs to device; target is a valid out pointer.
        unsafe { device.CreateRenderTargetView(&back_buffer, None, Some(&mut target)) }
            .map_err(|_| failed("render target"))?;
        let target = target.ok_or_else(|| failed("render target"))?;
        // SAFETY: plain call on the window created above.
        let _ = unsafe { ShowWindow(window.0, SW_SHOWNOACTIVATE) };
        Ok(Self {
            waitable,
            target,
            swap_chain,
            context,
            size,
            window,
        })
    }

    /// Pumps messages and draws a frame each time the swap chain is ready for one, until WM_QUIT.
    fn draw_until_quit(
        &self,
        picture: &mut dyn Picture,
        presents: &AtomicU64,
    ) -> windows::core::Result<()> {
        let mut message = MSG::default();
        loop {
            // Drain first: MsgWaitForMultipleObjects only wakes for messages that arrived after the last check.
            // SAFETY: message is a valid MSG.
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                if message.message == WM_QUIT {
                    return Ok(());
                }
                // SAFETY: message came from PeekMessageW on this thread.
                unsafe { DispatchMessageW(&message) };
            }
            // SAFETY: the waitable handle stays open while self lives.
            let woke = unsafe {
                MsgWaitForMultipleObjects(Some(&[self.waitable.0]), false, INFINITE, QS_ALLINPUT)
            };
            if woke == WAIT_OBJECT_0 {
                self.draw(picture)?;
                presents.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn draw(&self, picture: &mut dyn Picture) -> windows::core::Result<()> {
        // SAFETY: target is the back buffer's view on this context's device.
        unsafe {
            self.context
                .ClearView(&self.target, &[0.0, 0.0, 0.0, 1.0], None);
        }
        picture.draw(&self.context, &self.target, fit(picture.size(), self.size))?;
        // SAFETY: plain present; sync interval 1 waits for the card display's next refresh.
        unsafe { self.swap_chain.Present(1, DXGI_PRESENT(0)) }.ok()
    }
}

/// Every DXGI output on every graphics card, with its GDI name.
fn outputs(factory: &IDXGIFactory2) -> Vec<Output> {
    let mut outputs = Vec::new();
    for adapter_index in 0.. {
        // SAFETY: enumeration ends at the first index that fails (DXGI_ERROR_NOT_FOUND).
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(adapter_index) }) else {
            break;
        };
        for output_index in 0.. {
            // SAFETY: as above, per output.
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                break;
            };
            // SAFETY: plain description read.
            if let Ok(description) = unsafe { output.GetDesc() } {
                outputs.push(Output {
                    adapter: adapter_index,
                    output: output_index,
                    gdi_name: crate::guard::wide_to_string(&description.DeviceName),
                });
            }
        }
    }
    outputs
}

/// The borderless, topmost, no-activate tool window covering `card_display` exactly.
fn create_window(card_display: &Monitor) -> Option<HWND> {
    // SAFETY: a null module name returns this executable's handle.
    let instance = unsafe { GetModuleHandleW(None) }.ok()?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("cardcast mirror"),
        ..Default::default()
    };
    // SAFETY: class points at static strings and a valid window procedure; registering twice only fails.
    unsafe { RegisterClassW(&class) };
    // SAFETY: the class was registered above; the window is shown later without activation.
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            w!("cardcast mirror"),
            w!("cardcast mirror"),
            WS_POPUP,
            card_display.left,
            card_display.top,
            i32::try_from(card_display.width).ok()?,
            i32::try_from(card_display.height).ok()?,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .ok()?;
    // SAFETY: plain call on the window created above. Failing only means older Windows (shows as black in captures).
    let _ = unsafe { SetWindowDisplayAffinity(window, WDA_EXCLUDEFROMCAPTURE) };
    Some(window)
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_MOUSEACTIVATE {
        // A click on the card display must not take focus from the game.
        return LRESULT(MA_NOACTIVATE as isize);
    }
    // SAFETY: forwards the message this procedure received unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

#[cfg(test)]
mod tests;
