//! The mirror window: a borderless, topmost, no-activate window covering the card display,
//! drawn once per refresh through a flip-model swap chain on its own thread.

use super::capture::Desktop;
use super::{Ended, Fit, Mirror, Output, ended, find_output, fit, source_output, start_failed};
use crate::guard::Monitor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use windows::Win32::Foundation::{
    CloseHandle, E_POINTER, HANDLE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM,
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
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory2, IDXGIOutput, IDXGISwapChain2,
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

/// What the mirror window draws once per refresh: the captured source monitor (`super::capture::Desktop`).
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

/// The mirror thread and what tests read from it.
struct Running {
    thread_id: u32,
    /// The HRESULT the mirror ended with on its own; `None` after WM_QUIT.
    thread: JoinHandle<Option<i32>>,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the Windows tests"))]
    window: isize,
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the Windows tests"))]
    presents: Arc<AtomicU64>,
}

pub struct MirrorWindow {
    running: Option<Running>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl MirrorWindow {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            running: None,
            wake: Arc::new(wake),
        }
    }
}

impl Drop for MirrorWindow {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Mirror for MirrorWindow {
    fn start(&mut self, source_monitor: &Monitor, card_display: &Monitor) -> Result<(), Ended> {
        self.stop();
        let (started_sender, started) = mpsc::channel();
        let presents = Arc::new(AtomicU64::new(0));
        let thread = {
            let (source_monitor, card_display) = (source_monitor.clone(), card_display.clone());
            let (presents, wake) = (presents.clone(), self.wake.clone());
            std::thread::spawn(move || {
                let code = run(&source_monitor, &card_display, &presents, &started_sender);
                if code.is_some() {
                    wake();
                }
                code
            })
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
            Ok(Err(failure)) => {
                let _ = thread.join();
                Err(failure)
            }
            // The mirror thread panicked: a bug, so starting again would fail the same way.
            Err(_) => {
                let _ = thread.join();
                Err(Ended::Stop("The mirror could not start.".into()))
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

    fn is_running(&self) -> bool {
        self.running
            .as_ref()
            .is_some_and(|running| !running.thread.is_finished())
    }

    fn take_end(&mut self) -> Option<Ended> {
        if !self.running.as_ref()?.thread.is_finished() {
            return None;
        }
        let running = self.running.take()?;
        // A panicked mirror thread has no HRESULT; E_UNEXPECTED stops the mirror with a code.
        let code = running
            .thread
            .join()
            .unwrap_or(Some(0x8000_FFFF_u32.cast_signed()))?;
        Some(ended(code))
    }
}

/// Mirror thread: opens the window, reports its thread id and window, then draws until WM_QUIT.
/// Returns the HRESULT of a failed draw or present, `None` after WM_QUIT or a failed open.
fn run(
    source_monitor: &Monitor,
    card_display: &Monitor,
    presents: &AtomicU64,
    started: &Sender<Result<(u32, isize), Ended>>,
) -> Option<i32> {
    let opened = Screen::open(source_monitor, card_display).and_then(|(screen, source)| {
        // SAFETY: plain call returning the context's device.
        let device = unsafe { screen.context.GetDevice() }.map_err(|error| {
            start_failed(
                "Could not start the mirror: the graphics card did not respond.",
                "Direct3D device",
                error.code().0,
            )
        })?;
        let desktop = Desktop::open(&device, &source, &source_monitor.device_path)?;
        Ok((screen, desktop))
    });
    let (screen, mut desktop) = match opened {
        Ok(opened) => opened,
        Err(failure) => {
            let _ = started.send(Err(failure));
            return None;
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
        return None;
    }
    screen
        .draw_until_quit(&mut desktop, presents)
        .err()
        .map(|error| error.code().0)
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
    /// Opens the window on the card display and finds the source monitor's output on the same graphics card.
    fn open(
        source_monitor: &Monitor,
        card_display: &Monitor,
    ) -> Result<(Self, IDXGIOutput), Ended> {
        // A monitor missing here may be mid-replug, so each "not connected" is retried.
        let monitors = crate::guard::monitors().map_err(Ended::Restart)?;
        // SAFETY: plain factory creation.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1() }.map_err(|error| {
            start_failed(
                "Could not start the mirror: Windows graphics did not respond.",
                "DXGI factory",
                error.code().0,
            )
        })?;
        let outputs = outputs(&factory);
        let output =
            find_output(&card_display.device_path, &monitors, &outputs).ok_or_else(|| {
                Ended::Restart("The card display is not connected to any graphics card.".into())
            })?;
        let source = source_output(&source_monitor.device_path, output, &monitors, &outputs)?;
        let card_display = monitors
            .iter()
            .find(|monitor| monitor.device_path == card_display.device_path)
            .ok_or_else(|| Ended::Restart("The card display is not connected.".into()))?;
        let failed = |what: &'static str| {
            move |error: windows::core::Error| {
                start_failed(
                    "Could not start the mirror on the card display.",
                    what,
                    error.code().0,
                )
            }
        };
        // SAFETY: the index came from EnumAdapters1 on this factory just above.
        let adapter =
            unsafe { factory.EnumAdapters1(output.adapter) }.map_err(failed("graphics card"))?;
        // SAFETY: the index came from EnumOutputs on this adapter in `outputs`.
        let source = unsafe { adapter.EnumOutputs(source.output) }
            .map_err(|_| Ended::Restart("The source monitor is not connected.".into()))?;
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
        .map_err(failed("Direct3D device"))?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(failed("Direct3D device")(E_POINTER.into()));
        };
        let context: ID3D11DeviceContext1 = context.cast().map_err(|_| {
            Ended::Stop(
                "Could not start the mirror: the graphics driver is too old. Update it. (Direct3D 11.1)"
                    .into(),
            )
        })?;

        // A window that can't be made on the card display is likely one leaving: retried.
        let window = Window(create_window(card_display).ok_or_else(|| {
            Ended::Restart("Could not start the mirror on the card display. (window)".into())
        })?);
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
                .map_err(failed("swap chain"))?;
        // SAFETY: the swap chain was created with the waitable-object flag, which these calls need.
        let waitable = unsafe {
            swap_chain
                .SetMaximumFrameLatency(1)
                .map_err(failed("frame latency"))?;
            Waitable(swap_chain.GetFrameLatencyWaitableObject())
        };
        // SAFETY: buffer 0 of a flip-model swap chain is the current back buffer.
        let back_buffer: ID3D11Texture2D =
            unsafe { swap_chain.GetBuffer(0) }.map_err(failed("back buffer"))?;
        let mut target = None;
        // SAFETY: back_buffer belongs to device; target is a valid out pointer.
        unsafe { device.CreateRenderTargetView(&back_buffer, None, Some(&mut target)) }
            .map_err(failed("render target"))?;
        let target = target.ok_or_else(|| failed("render target")(E_POINTER.into()))?;
        // SAFETY: plain call on the window created above.
        let _ = unsafe { ShowWindow(window.0, SW_SHOWNOACTIVATE) };
        Ok((
            Self {
                waitable,
                target,
                swap_chain,
                context,
                size,
                window,
            },
            source,
        ))
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

/// Top-left corner of the mirror window, if one is open; for the settings window's Windows tests.
#[cfg(test)]
pub fn shown_at() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect};
    // SAFETY: the class name is a static string; no window name is matched.
    let window = unsafe { FindWindowW(w!("cardcast mirror"), None) }.ok()?;
    let mut rect = RECT::default();
    // SAFETY: rect is a valid RECT; a window closed meanwhile only fails the call.
    unsafe { GetWindowRect(window, &mut rect) }.ok()?;
    Some((rect.left, rect.top))
}

#[cfg(test)]
mod tests;
