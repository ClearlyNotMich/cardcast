# Win32 pitfalls

Sourced facts for Win32 code that can't be run on Linux. Each section: the fact, a Do / Don't line, and its source. If a behaviour isn't here, look it up on Microsoft Learn before relying on it; don't guess.

## Calls that block on a hung program

- `SendMessage` to a window on another thread blocks the sender until the receiving thread processes the message; that thread only processes it when it runs message retrieval code. [SendMessage](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendmessagew)
- `SendMessageTimeout` gives up after the timeout; the system treats a thread as not responding if it hasn't called `GetMessage` or similar for five seconds. [SendMessageTimeout](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendmessagetimeoutw)
- `GetWindowTextW` on another process's window reads the stored caption without sending `WM_GETTEXT`, so a hung owner can't hang the caller. On a window in your own process it sends `WM_GETTEXT` and can hang. [GetWindowTextW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowtextw), [The secret life of GetWindowText](https://devblogs.microsoft.com/oldnewthing/20030821-00/?p=42833)
- `InternalGetWindowText` always reads the text from the window structure and never sends `WM_GETTEXT`. [InternalGetWindowText](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-internalgetwindowtext)
- `SetWindowPos` with `SWP_ASYNCWINDOWPOS` posts the request to the owning thread when the two threads have different input queues, so the caller doesn't block. [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)
- `SetWindowPlacement` has the same option: `WPF_ASYNCWINDOWPLACEMENT`. [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement)

Do: use `SWP_ASYNCWINDOWPOS` / `WPF_ASYNCWINDOWPLACEMENT` and `GetWindowTextW` or `InternalGetWindowText` on other programs' windows. Don't: call `SendMessage` (or `SetWindowPos` without the async flag) on a window from the guard thread.

## Stopping a thread with PostThreadMessage

- `PostThreadMessage` fails unless the target thread has created a message queue. The system creates it on the thread's first call to a User or GDI function; calling `PeekMessage` with `PM_NOREMOVE` forces it. [PostThreadMessageW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postthreadmessagew)
- A message queue holds at most 10,000 posted messages; a post to a full queue fails. [PostThreadMessageW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-postthreadmessagew)

Do: stop every message-loop thread with `guard::quit_and_join`, which retries the `WM_QUIT` post until it lands or the thread ended, then joins. Don't: join only when the first post succeeded; a thread still starting has no queue and is left running.

## Window rect versus visible frame

- Since Vista, `GetWindowRect` may include invisible resize borders. `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` gives the visible bounds, in screen space. [GetWindowRect](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect), [DWMWINDOWATTRIBUTE](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute)
- `GetWindowRect` is virtualized for DPI; the extended frame bounds are not. Mixing them is only safe in a per-monitor aware thread, where nothing is virtualized. [GetWindowRect](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindowrect)

Do: decide with the frame bounds, then add the border offsets back before calling `SetWindowPos`. Don't: pass frame bounds straight to `SetWindowPos`.

## WINDOWPLACEMENT

- For a top-level window without `WS_EX_TOOLWINDOW`, `rcNormalPosition`, `ptMinPosition` and `ptMaxPosition` are in **workspace** coordinates: (0,0) is the top-left of the work area, which excludes the taskbar and other app toolbars. Otherwise they are screen coordinates. [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement)
- Passing these to `SetWindowPos` puts the window in the wrong place (it "creeps" when the taskbar is on top). Use them only with `GetWindowPlacement` / `SetWindowPlacement`. [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement)
- `rcNormalPosition` is where the window goes when restored; `showCmd` is its show state (maximised, minimised, normal). [WINDOWPLACEMENT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-windowplacement)
- `length` must be `size_of::<WINDOWPLACEMENT>()` or `SetWindowPlacement` fails. If the placement would be fully off screen, the system moves it onto a visible monitor. [SetWindowPlacement](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowplacement)

- Measured on the CI runner's second monitor (CI job `second-monitor`): `SetWindowPlacement` with `SW_SHOWMAXIMIZED` on a window that is already maximised only changes its restore position; the window stays maximised on its old monitor. Restoring it first and then maximising moves it, but maximising activates it (`SW_SHOWMAXIMIZED` activates). [ShowWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow)

Do: move a maximised or minimised window with `SetWindowPlacement`, so its restore position moves too; to put a maximised window on another monitor, also move its frame onto that monitor's work area with `SetWindowPos`. Don't: mix workspace and screen coordinates.

## Out-of-context SetWinEventHook

- With `WINEVENT_OUTOFCONTEXT`, events are delivered on the thread that called `SetWinEventHook`, and that thread must have a message loop to receive them. [SetWinEventHook](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)
- Events are queued and arrive asynchronously but in order. A slow callback drains USER resources and can make the system fault or crawl. [WINEVENT flags](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook), [Out-of-context hook functions](https://learn.microsoft.com/en-us/windows/win32/winauto/out-of-context-hook-functions)
- The callback can re-enter while it is still handling an event. [SetWinEventHook](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)
- A move or resize fires `EVENT_OBJECT_LOCATIONCHANGE` for the top-level window (not its title bar or menu); a user drag also fires `EVENT_SYSTEM_MOVESIZESTART` / `EVENT_SYSTEM_MOVESIZEEND`. `EVENT_SYSTEM_MINIMIZEEND` fires when a window is about to be restored. `EVENT_OBJECT_LOCATIONCHANGE` also fires for the caret, so filter on `OBJID_WINDOW`. [Event constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)

Do: keep the callback short and defer work to the message loop. Don't: install the hook on a thread without `GetMessage`, or hold a borrow across anything that pumps messages.

## DPI and multi-monitor coordinates

- A per-monitor aware process sees all displays in physical pixels and Windows doesn't bitmap-stretch it. cardcast's manifest declares Per Monitor v2 (Windows 10 1703+). [High DPI desktop apps](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows), [DPI_AWARENESS_CONTEXT](https://learn.microsoft.com/en-us/windows/win32/hidpi/dpi-awareness-context)
- From a DPI-unaware or system-aware thread, API return values may be virtualized. Restore the old context after `SetThreadDpiAwarenessContext`. [High DPI desktop apps](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows)
- The primary monitor holds (0,0) but needn't be top-left: monitors left of or above it have negative coordinates. Coordinates fit in a signed 16-bit range. [The Virtual Screen](https://learn.microsoft.com/en-us/windows/win32/gdi/the-virtual-screen)
- `SM_CXSCREEN` / `SM_CYSCREEN` give the primary monitor only. Use `MonitorFromRect` + `GetMonitorInfo` (`rcMonitor`, `rcWork`), and don't assume a monitor rect starts at (0,0). [Positioning objects on a multiple display setup](https://learn.microsoft.com/en-us/windows/win32/gdi/positioning-objects-on-a-multiple-display-setup)

Do: use `i32` everywhere and test with negative monitor rects. Don't: use `u32` coordinates or screen-size metrics.
Do: swap a `DISPLAYCONFIG_SOURCE_MODE`'s width and height when the path's `targetInfo.rotation` is 90 or 270; measured on the CI runner's virtual monitor turned 90° with `SetDisplayConfig`, its source mode stayed 800x600 while `rcMonitor` was 600x800 ([run](https://github.com/ClearlyNotMich/cardcast/actions/runs/46050167136/job/140679209981)). Don't: take the source mode as the monitor's desktop size.

## Focus and z-order

- `SetWindowPos` without `SWP_NOACTIVATE` activates the window and moves it to the top of its z-order group. `SWP_NOZORDER` keeps the z-order. [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)
- An inactive window can't be activated without also coming to the top of the z-order. [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)
- `SetForegroundWindow` only works under conditions (e.g. the caller is the foreground process or received the last input) and can be denied even then; otherwise Windows flashes the taskbar button. [SetForegroundWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow)
- `EVENT_SYSTEM_FOREGROUND` fires whenever the foreground window changes. [Event constants](https://learn.microsoft.com/en-us/windows/win32/winauto/event-constants)

Do: pass `SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER` when moving someone else's window. Don't: rely on `SetForegroundWindow` succeeding.
