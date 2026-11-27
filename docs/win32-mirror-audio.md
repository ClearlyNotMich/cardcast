# Win32 mirror and audio

Sourced facts for the mirror and audio code, which can't be run on Linux. Each section: the fact, a Do / Don't line, and its source. Both capture methods are covered until the capture ADR (#6) picks one. If a behaviour isn't here, look it up on Microsoft Learn before relying on it; don't guess.

## Windows Graphics Capture: item and session

- `IGraphicsCaptureItemInterop::CreateForMonitor` makes a `GraphicsCaptureItem` from an `HMONITOR`, with no picker. Minimum: Windows 10 1903 (build 18362). [CreateForMonitor](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createformonitor)
- The system draws a yellow border around every item being captured. [Screen capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)
- `GraphicsCaptureSession.IsCursorCaptureEnabled` chooses whether the pointer is in the frames. Minimum: Windows 10 2004 (build 19041). [IsCursorCaptureEnabled](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.iscursorcaptureenabled)
- `IsBorderRequired` needs build 10.0.20348. Setting it to false only removes the border after the user consents through `GraphicsCaptureAccess.RequestAccessAsync(Borderless)`, which requires the `graphicsCaptureWithoutBorder` capability in the app's **package** manifest. If consent is denied the setter succeeds but is ignored; if another app sets it to true for the same display, the border shows anyway. [IsBorderRequired](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired), [RequestAccessAsync](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscaptureaccess.requestaccessasync)
- `GraphicsCaptureItem.Closed` can fire even though the target seems to still exist, ending the capture. [GraphicsCaptureItem.Closed](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscaptureitem.closed)

Do: check that the border is acceptable in an unpackaged `.exe` before choosing this method, and handle `Closed` by re-creating the item. Don't: assume `IsBorderRequired = false` hides the border.

## Windows Graphics Capture: frame pool

- `Direct3D11CaptureFramePool.CreateFreeThreaded` raises `FrameArrived` on the pool's own worker thread and needs no `DispatcherQueue`; `Create` does. Minimum: Windows 10 1809. [CreateFreeThreaded](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded)
- `TryGetNextFrame` checks a frame out; it returns null when the pool is empty. The buffer goes back to the pool when the frame is closed/dropped. Don't keep a frame or its surface after that. [Screen capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture), [TryGetNextFrame](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.trygetnextframe)
- `numberOfBuffers` is how many captured frames the pool holds. [CreateFreeThreaded](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded)
- A frame has `ContentSize`, `Surface` and `SystemRelativeTime` (a QPC time). [Screen capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)
- `Recreate` changes the device, buffer size or buffer count, and discards all existing frames. Use it when the item resizes or the device is lost. [Screen capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)

Do: at each draw, drain `TryGetNextFrame` until null, keep only the newest and drop the rest at once; call `Recreate` when `ContentSize` changes or after a new D3D device. Don't: hold frames across draws, or the pool runs dry.

## Windows Graphics Capture: HDR to SDR

- With HDR ("Windows HD Color") on, the content format might not be `B8G8R8A8_UNORM`. To avoid overclipping (captured content looks washed out), Microsoft recommends `R16G16B16A16_FLOAT` through the whole capture pipeline, including the frame pool. [Screen capture](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture)
- After capturing HDR content, "additional processing is required", for example HDR-to-SDR tone mapping. [HDR and advanced colour](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range)

Do: compare a `B8G8R8A8` pool against an FP16 pool plus own SDR conversion on the sender with HDR on. Don't: assume Windows tone-maps HDR to correct SDR for an 8-bit pool; Microsoft doesn't document that.

## DXGI Desktop Duplication: frames and timeouts

- `DuplicateOutput` needs a device created from `IDXGIFactory1` or later, on the adapter that owns the output (else `E_INVALIDARG`). It fails if this process already duplicates that output. By default only four processes can duplicate at once in a session. [DuplicateOutput](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput), [DuplicateOutput1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1)
- `AcquireNextFrame(timeout)` waits up to the timeout and returns `DXGI_ERROR_WAIT_TIMEOUT` if no new image came; 0 checks and returns at once. With `INFINITE` it can't be interrupted, so use a finite timeout to check for stop requests. [AcquireNextFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe)
- A frame arrives when the desktop image changes **or** the hardware pointer's shape or position changes; it may carry only the pointer update. [AcquireNextFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe)
- Release the frame before acquiring the next; the surface is invalid after release. Microsoft recommends releasing just before the next `AcquireNextFrame`. [ReleaseFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-releaseframe)
- `AccumulatedFrames` counts desktop updates folded into one image since the last one you processed, so older frames are skipped by design. [DXGI_OUTDUPL_FRAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_outdupl_frame_info)
- In a rotated mode the surface stays un-rotated with the image rotated inside it; the client must rotate it. [Desktop Duplication API](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api)

Do: copy the frame into your own texture, then release it before the next acquire; treat `WAIT_TIMEOUT` as "reuse the last frame". Don't: wait `INFINITE` on the mirror thread.

## DXGI Desktop Duplication: access lost

- `AcquireNextFrame` returns `DXGI_ERROR_ACCESS_LOST` when a different kind of image is shown: a desktop switch (for example the secure desktop of a permission prompt), a mode change, or a switch between DWM on, DWM off or another fullscreen app. The app must release the `IDXGIOutputDuplication` and create a new one. [AcquireNextFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe)
- `GetFramePointerShape` and the other frame calls return the same error. [GetFramePointerShape](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-getframepointershape)
- Re-creating can fail with `E_ACCESSDENIED` (only `LOCAL_SYSTEM` can read the secure desktop) or `DXGI_ERROR_UNSUPPORTED` (unsupported mode); the app can then wait for a mode change and retry. [DuplicateOutput1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1)

- `DuplicateOutput1` also returns `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` when the most processes allowed already duplicate the output, and `DXGI_ERROR_SESSION_DISCONNECTED` while the session is disconnected; both can pass. `E_INVALIDARG` means the device is not on the output's graphics card. [DuplicateOutput1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1)
- No Microsoft page lists every code a monitor or graphics card going away can return. cardcast therefore restarts the mirror on any HRESULT except those of a bug or a missing capability (`E_OUTOFMEMORY`, `E_NOINTERFACE`, `E_NOTIMPL`, `E_POINTER`, `E_UNEXPECTED`); see `mirror::ended` and issue #20.

Do: on `ACCESS_LOST`, drop the duplication and retry creation on a timer until it succeeds. Don't: treat `E_ACCESSDENIED` during a prompt as fatal.

## DXGI Desktop Duplication: pointer

- The pointer is either drawn into the image or separate. If separate, the frame info says it is visible and the app must draw it itself, at `PointerPosition`. [Desktop Duplication API](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api)
- A non-zero `LastMouseUpdateTime` means the position (and maybe the shape) changed; `PointerPosition` is only valid then. A new shape exists only when `PointerShapeBufferSize` is non-zero. `PointerPosition` is the top-left of the shape, not the hotspot. [DXGI_OUTDUPL_FRAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_outdupl_frame_info)
- `GetFramePointerShape` returns `DXGI_ERROR_MORE_DATA` if the buffer is too small and reports the size needed. [GetFramePointerShape](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-getframepointershape)

Do: keep the last shape and position between frames and draw the pointer on top yourself. Don't: read `PointerPosition` when `LastMouseUpdateTime` is 0.

## DXGI Desktop Duplication: HDR format

- `DuplicateOutput` always converts a fullscreen surface to 32-bit BGRA. `IDXGIOutput5::DuplicateOutput1` takes a list of accepted formats and keeps high-colour formats (e.g. `R10G10B10A2`); the list should always include `B8G8R8A8_UNORM`. [DuplicateOutput1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1)
- Capturing HDR via `DuplicateOutput1` keeps the pixel data, but HDR-to-SDR tone mapping is then the app's job. [HDR and advanced colour](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range)
- `DXGI_OUTPUT_DESC1.ColorSpace` tells whether the output is in an advanced-colour (HDR) mode; `BitsPerColor` is the wire bit depth. [DXGI_OUTPUT_DESC1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_6/ns-dxgi1_6-dxgi_output_desc1)
- SDR content is shown at the SDR white level: Windows' nominal white is 80 nits. In linear (scRGB, FP16) values, SDR is scaled by `SdrWhiteLevelInNits / 80`. A desktop app reads it from `DISPLAYCONFIG_SDR_WHITE_LEVEL` via `DisplayConfigGetDeviceInfo`; `SDRWhiteLevel` is a multiple of 80 nits times 1000 (1000 = 80 nits). [HDR and advanced colour](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range), [DISPLAYCONFIG_SDR_WHITE_LEVEL](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_sdr_white_level)

Do: when `ColorSpace` is HDR, request `R16G16B16A16_FLOAT`, divide by `SDRWhiteLevel / 1000` and encode to sRGB. Don't: hard-code 80 nits.

## Mirror window and swap chain

- `WS_EX_NOACTIVATE`: a click doesn't make it the foreground window and it isn't on the taskbar by default. `WS_EX_TOOLWINDOW`: not on the taskbar nor in Alt+Tab. [Extended window styles](https://learn.microsoft.com/en-us/windows/win32/winmsg/extended-window-styles)
- Use a flip-model swap chain (`DXGI_SWAP_EFFECT_FLIP_DISCARD` or `FLIP_SEQUENTIAL`), at least 2 buffers, MSAA not supported; re-bind the back buffer after each `Present`. [Use DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)
- DirectFlip needs buffers matching the screen size and the client area covering the screen (panel fitters or overlays relax this within hardware limits). Then the DWM can sleep and frames go straight to the screen: "Independent Flip". If other desktop content comes on top, DWM falls back to composition or reverse-composes. PresentMon shows which mode was used. [Use DXGI flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model)
- Pacing: create the swap chain with `DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT` (it can't be added later by `ResizeBuffers`), set `IDXGISwapChain2::SetMaximumFrameLatency` (default 1), and wait on `GetFrameLatencyWaitableObject` before drawing every frame, including the first. [Reduce latency with DXGI 1.3 swap chains](https://learn.microsoft.com/en-us/windows/uwp/gaming/reduce-latency-with-dxgi-1-3-swap-chains), [GetFrameLatencyWaitableObject](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgiswapchain2-getframelatencywaitableobject)
- Close the waitable handle with `CloseHandle` when done. [GetFrameLatencyWaitableObject](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgiswapchain2-getframelatencywaitableobject)

Do: size the swap chain to the card display's full rect, wait on the waitable object, then draw the newest frame and `Present(1, 0)`. Don't: let another window sit over the mirror; that drops it to composed mode.

## Across graphics cards

- DXGI applications can present to a window on a monitor driven by another adapter: the runtime copies each present from the render adapter into a cross-adapter resource (in system memory), then the display adapter copies it (two copies) or, with CASO (Windows 11 / Server 2022, WDDM 2.9+ drivers, flip model only), scans it out directly (one copy). [Supporting CASO](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/supporting-caso), [Rendering on a discrete GPU using cross-adapter resources](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/rendering-on-a-discrete-gpu-using-cross-adapter-resources)
- D3D12 cross-adapter heaps (`D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER`) are only supported in system memory, need barriers and cross-adapter fences, and can't be shared with another D3D API version. [Shared heaps](https://learn.microsoft.com/en-us/windows/win32/direct3d12/shared-heaps)
- WGC captures a monitor on another adapter than its device's, but copies through system memory (Microsoft Windows Composition team). [robmikh, Win32CaptureSample #46](https://github.com/robmikh/Win32CaptureSample/issues/46)
- Measured in CI (`research-cross-gpu` spike, run 46039255018): Desktop Duplication on adapter 0 plus a flip swap chain made with that device for a window on adapter 1's monitor starts and keeps presenting. The `second-monitor` job keeps this as `second_monitor_card_display_on_another_graphics_card_is_mirrored` (issue #25).

Do: when the card display is on another graphics card, keep the device, duplication and HDR shader on the source monitor's card and present the SDR result to the card display's window from there. Don't: move duplicated frames between cards yourself (D3D11 shared handles document no cross-adapter mode; a staging readback costs CPU or a frame).

## Excluding the mirror from capture

- `SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)`: the window shows on a monitor and nowhere else. Windows 10 2004+; older versions behave as `WDA_MONITOR`. [SetWindowDisplayAffinity](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity)
- It works only while DWM composes the desktop and is not a security guarantee. [SetWindowDisplayAffinity](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity)

Do: set it on the mirror window so capturing the source monitor can never pick the mirror up; the card display is a monitor, so the capture card still gets the image. Don't: rely on it for anything that must not leak.

## Display change, device removed and monitor matching

- `WM_DISPLAYCHANGE` is sent to all top-level windows when the display resolution changes. [WM_DISPLAYCHANGE](https://learn.microsoft.com/en-us/windows/win32/gdi/wm-displaychange)
- `WM_DEVICECHANGE` with `DBT_DEVNODES_CHANGED` is broadcast to top-level windows when a device is added to or removed from the system. [DBT_DEVNODES_CHANGED](https://learn.microsoft.com/en-us/windows/win32/devio/dbt-devnodes-changed)
- Measured in CI (issue #20, virtual monitor): disabling a monitor's PnP device (`pnputil /disable-device DISPLAY\...`) leaves its display path active at the same position, sends no `WM_DISPLAYCHANGE`, and `monitorDevicePath` comes back empty until the device is enabled again. Duplicating it keeps working.
- `Present` or `ResizeBuffers` returning `DXGI_ERROR_DEVICE_REMOVED` or `DXGI_ERROR_DEVICE_RESET` (adapter removed, driver upgraded) means: release every resource and swap chain from the old device and create a new device. [Handle device removed](https://learn.microsoft.com/en-us/windows/uwp/gaming/handling-device-lost-scenarios)
- `DISPLAYCONFIG_TARGET_DEVICE_NAME.monitorDevicePath` is the monitor's device path. [DISPLAYCONFIG_TARGET_DEVICE_NAME](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_target_device_name)
- `DISPLAYCONFIG_SOURCE_DEVICE_NAME.viewGdiDeviceName` is the GDI name of the source the monitor is on, the same kind of name as `DXGI_OUTPUT_DESC.DeviceName`. [DISPLAYCONFIG_SOURCE_DEVICE_NAME](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_source_device_name), [DXGI_OUTPUT_DESC1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_6/ns-dxgi1_6-dxgi_output_desc1)

Do: on `WM_DISPLAYCHANGE`, re-run `QueryDisplayConfig`, map device path → GDI name → DXGI output, and rebuild the device if the adapter changed. Don't: store the GDI name (`\\.\DISPLAYn`) as the saved setting.

## WASAPI loopback and rendering

- Loopback: get the `IMMDevice` of the **render** endpoint and call `IAudioClient::Initialize` with `AUDCLNT_STREAMFLAGS_LOOPBACK`. Shared mode only. Event-driven loopback works from Windows 10 1703. [Loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- Without a hardware loopback pin, WASAPI copies the engine output to the loopback buffer **in addition to** the hardware, so the sender keeps hearing it. [Loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` inserts a channel matrixer and sample-rate converter between your format and the mix format; add `SRC_DEFAULT_QUALITY` for audio people hear. [AUDCLNT_STREAMFLAGS](https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants)
- `GetCurrentPadding` on a render stream is the unread data in the buffer, which tells how much can be written. [GetCurrentPadding](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getcurrentpadding)

Do: loopback the chosen device and render into the card display's endpoint, both shared mode. Don't: use exclusive mode on either.

## Clock drift between the two devices

- When a renderer runs on one clock and the source on another, the renderer runs faster (gaps) or slower (samples bunch up, then drop); with a live source the renderer must match the source's rate. [Live sources: rate matching](https://learn.microsoft.com/en-us/windows/win32/directshow/live-sources)
- A shared-mode render stream initialised with `AUDCLNT_STREAMFLAGS_RATEADJUST` exposes `IAudioClockAdjustment` via `GetService`. [AUDCLNT_STREAMFLAGS](https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants), [IAudioClockAdjustment](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nn-audioclient-iaudioclockadjustment)
- `SetSampleRate` takes effect after the current frame and stays until called again; it must not be called from a real-time processing thread. [SetSampleRate](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclockadjustment-setsamplerate)

Do: keep render padding near a target and nudge `SetSampleRate` a little above or below nominal to hold it. Don't: let padding grow (latency) or let the buffer run empty (clicks).

## Audio device changes

- Register an `IMMNotificationClient` with `RegisterEndpointNotificationCallback` to get `OnDeviceAdded`, `OnDeviceRemoved`, `OnDeviceStateChanged`, `OnDefaultDeviceChanged` and `OnPropertyValueChanged`, each with the endpoint ID. [IMMNotificationClient](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nn-mmdeviceapi-immnotificationclient)
- Callbacks must not block or wait on a sync object, must not (un)register callbacks, and must not release the last reference to an MMDevice object. [IMMNotificationClient](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/nn-mmdeviceapi-immnotificationclient)
- `AUDCLNT_E_DEVICE_INVALIDATED` means the endpoint was unplugged or reconfigured. For a specific device: release all WASAPI interfaces, activate `IAudioClient` on the same device again; that works if it was reconfigured, not removed. [Recovering from an invalid-device error](https://learn.microsoft.com/en-us/windows/win32/coreaudio/recovering-from-an-invalid-device-error)

Do: in callbacks only post a message to the audio thread, and restart both streams from there. Don't: re-create streams inside the callback.
