# Win32 DDC/CI: monitor input switching

Sourced facts for switching a monitor's input over DDC/CI (issue #35, SPEC section 11). Each section: the facts, a Do / Don't line, and the sources. Researched 2026-11-27 on branch `research-ddc`. "Not documented" means no primary source was found; the owner checks it on the sender. VESA standards are cited from public copies of the PDFs: [DDC/CI 1.1][ddcci], [MCCS 2.2a][mccs].

## Physical monitors from an HMONITOR

- dxva2 talks to a *physical* monitor handle, not an `HMONITOR`. Steps: `GetNumberOfPhysicalMonitorsFromHMONITOR`, `GetPhysicalMonitorsFromHMONITOR`, and `DestroyPhysicalMonitors` when done. One `HMONITOR` can hold several physical monitors (clone mode). [GetPhysicalMonitorsFromHMONITOR][gpm], [Using the high-level functions][high]
- `PHYSICAL_MONITOR` holds only the handle and a 128-character description, with no device path or EDID. [PHYSICAL_MONITOR][pm]
- The link to cardcast's `Monitor` is the GDI name: `GetMonitorInfo` with `MONITORINFOEX` gives the `HMONITOR`'s `szDevice` (`\\.\DISPLAYn`), and `DisplayConfigGetDeviceInfo` gives each display path's `viewGdiDeviceName`, which `Monitor.gdi_name` already holds. Power Display (PowerToys) matches physical handles to `QueryDisplayConfig` paths the same way. [MONITORINFOEX][mie], [DISPLAYCONFIG_SOURCE_DEVICE_NAME][sdn], [Power Display design][pd-design]
- `\\.\DISPLAYn` changes on replug (`docs/research/ci-second-monitor.md` section 2), so look it up again before every DDC/CI call. Never save it.
- `EnumDisplayMonitors` and `MonitorFromWindow` only return monitors on the desktop. A monitor that has left the desktop has no `HMONITOR`, so dxva2 can't reach it. [EnumDisplayMonitors][edm]
- EDID serial: `DISPLAYCONFIG_TARGET_DEVICE_NAME` has the EDID manufacturer and product code and the friendly name, but no serial. [DISPLAYCONFIG_TARGET_DEVICE_NAME][tdn] `WmiMonitorID.SerialNumberID` (root\wmi) gives the serial from the EDID's vendor/product block. [WmiMonitorID][wmi] In the EDID, bytes 12–15 hold a 32-bit serial number and a display descriptor with tag `0xFF` holds the serial as text (`EDID_DETAIL_MONITOR_SERIAL`). [drm_edid.h][drm-edid] The sender's AW2523HF reports `4GZY5P3`, which is that descriptor text (issue #35).
- Reading the raw EDID from the monitor's device registry key (`SetupDiOpenDevRegKey`, `DIREG_DEV`, value `EDID`) is common but not documented. Microsoft only documents the `EDID_OVERRIDE` keys there. [SetupDiOpenDevRegKey][sdi], [Overriding monitor EDIDs][edid-ovr]

Do: match by GDI name, and use a monitor only when its `HMONITOR` has exactly one physical monitor. Parse the serial in a plain function (descriptor `0xFF` text first). Don't: save a physical handle or a `\\.\DISPLAYn` name, or guess which physical monitor in a clone set is which.

## Capabilities string

- Request it with `GetCapabilitiesStringLength` (length including the null), then `CapabilitiesRequestAndCapabilitiesReply`. It is ASCII. Both "usually return quickly, but sometimes … several seconds". [GetCapabilitiesStringLength][gcsl], [CapabilitiesRequestAndCapabilitiesReply][crcr]
- On the wire the string arrives in fragments of at most 32 bytes, and the host waits at least 50 ms after each one. A 400-byte string is at least 13 round trips, so over 0.6 s before any retry. [DDC/CI 1.1][ddcci] section 4.6. ddcutil calls it "the most expensive … command" and the one most likely to fail. [ddcutil performance][dd-perf]
- Format: `vcp(… 60(0F 11 12) …)`. A non-continuous code lists its allowed values in nested parentheses. Unknown abbreviations must be ignored. [MCCS 2.2a][mccs] section 6. Power Display found that monitors also omit the outer parentheses and that some write values with no spaces (`010203`). [Power Display design][pd-design]
- The string is often wrong. A Dell P2210 with VGA, DVI and DP lists only `60(01 03)`. A Samsung U32H750 lists `60(11 12 0F)` but uses `05` and `06` for HDMI. "The only way to know for sure … is by testing." [ddcutil FAQ][dd-faq], [ddcutil capabilities][dd-caps]
- Microsoft: "Many monitors don't fully implement that standard … might result in undefined monitor behavior." [CapabilitiesRequestAndCapabilitiesReply][crcr] Power Display warns that a malformed capabilities string can trigger a Windows kernel bug (a BSOD) on a few monitors, and keeps an exclusion list for that reason. [Power Display][pd]

Do: read it once per monitor per display change, off the UI thread. Split hex pairs whether or not they are spaced, and take the input list from `60(...)` only. Don't: read it on every button press, fail on unknown sections, or claim an input works only because the list has it.

## VCP 0x60 (Input Select)

- Read: `GetVCPFeatureAndVCPFeatureReply(h, 0x60, …)` ("about 40 milliseconds"). Write: `SetVCPFeature(h, 0x60, value)` ("about 50 milliseconds"). Both are in `windows` 0.62 under the `Win32_Devices_Display` feature, which cardcast already enables. [Get][getvcp], [Set][setvcp]
- MCCS 2.2a values (byte SL; SH, MH and ML are `00`): `0F` DisplayPort 1, `10` DisplayPort 2, `11` HDMI 1, `12` HDMI 2, `03`/`04` DVI, `01`/`02` analog. Values `13` and up are reserved. A compliant monitor ignores an input it doesn't support. [MCCS 2.2a][mccs] table 8-13, table 10-18
- USB-C has no MCCS value. Some makers use `0B`, others other values, and not always the same one across their own models. [ddcutil FAQ][dd-faq]
- Minimum waits from [DDC/CI 1.1][ddcci] sections 4.3–4.6: 40 ms from a Get request to reading the reply, 50 ms after a Set or a capabilities fragment, 200 ms after Save Current Settings. On no answer, retry at least 3 times, 40 ms apart (section 5). The 40 and 50 ms waits are inside dxva2's documented times. Whether dxva2 also waits after a Set before the next call is not documented.
- A successful `SetVCPFeature` means the monitor took the command, not that it switched. Power Display warns: an input with no signal goes black, and "software control may be unavailable until you change inputs again using the monitor's physical buttons". [Power Display][pd]

Do: compare only the low byte (SL) of the current value, and wait 50 ms after a Set before the next DDC/CI call to that monitor. Don't: send a value the monitor didn't list, send `0x60` in a loop, or report "switched" as "picture shown".

## Which calls block, and for how long

- Get about 40 ms, Set about 50 ms, the capabilities string up to several seconds (above). With 3+ retries per DDC/CI section 5, a dead monitor costs several hundred ms per call.
- The graphics driver may block an I2C transfer while it uses the bus itself, including for HDCP. [DXGKDDI_I2C_TRANSMIT_DATA_TO_DISPLAY][ddi-i2c] For an indirect display the I2C callback may take up to 5 s. [EVT_IDD_CX_MONITOR_I2C_TRANSMIT][idd-i2c]
- ddcutil spends about 90% of its time in waits the protocol mandates. [ddcutil FAQ][dd-faq]

Do: make every dxva2 call on a separate thread that only does DDC/CI work, one call at a time per monitor, and report back through the existing status channel. Don't: call dxva2 on the UI thread, the guard thread or the mirror thread (CODING_STANDARDS rule 5), or on the egui repaint path.

## When a switched monitor leaves Windows

- A DisplayPort monitor that drops hot-plug detect looks unplugged to Windows, which then rearranges windows (Microsoft's "Rapid HPD" fix covers sleep). [Avoid unexpected app rearrangement][rapid-hpd]
- Whether a given monitor drops hot-plug when its input switches away is not documented and depends on the monitor. The sender's AW2523HF still answers DDC/CI over DP while it shows HDMI 1 (issue #35, ddcutil on Linux). Whether Windows keeps it on the desktop meanwhile is an owner check.
- If the source monitor leaves, cardcast's existing state is "Waiting for source monitor". If the card display leaves, it is "Waiting for card display" (SPEC section 4, `ui::settings::follow_displays`). Both resume when the monitor comes back.
- A monitor that has left the desktop has no `HMONITOR` (section 1), so cardcast can't switch it back. Only the monitor's own buttons, or a source PC that plugs it back in, can.

Do: let the existing display-change path handle it, with no new state. Don't: promise "toggle back" for a monitor that leaves Windows.

## Multi-GPU, DSC, MST and NVIDIA

- DDC/CI runs through the I2C entry points of the driver that owns the output (a target on one adapter), so a monitor on a second GPU or the iGPU (ADR 0003) is reached through its own `HMONITOR` like any other. [DXGKDDI_I2C_TRANSMIT_DATA_TO_DISPLAY][ddi-i2c]
- DSC (needed by high-bandwidth modes, `docs/research/cross-gpu-mirror.md` section 0): no source says how it affects DDC/CI. Owner check.
- MST hubs, docks and KVMs: Power Display notes that some don't pass DDC/CI and some report a hard-coded capabilities string. [Power Display design][pd-design] ddcutil's MST and NVIDIA findings (no I2C bus behind an MST hub, `RMUseSwI2c`) are about the Linux driver, so they say nothing about Windows. [ddcutil FAQ][dd-faq], [ddcutil NVIDIA][dd-nv]
- DP output to an HDMI monitor through a passive (DP++) adapter or an active converter can carry DDC differently. "Sometimes, DDC works over a HDMI or DVI connection but not over DisplayPort." [ddcutil DisplayPort][dd-dp]

Do: treat "no answer" as normal: the monitor gets no button, and the command exits with code 1. Don't: name a brand or GPU vendor in production code.

## Reporting from a GUI-subsystem exe

- cardcast's release build is `windows_subsystem = "windows"` (`src/main.rs`). A GUI-subsystem program runs with no console, and the subsystem can't be chosen at run time. [The Old New Thing][tont-console] Its standard handles are invalid unless the parent let it inherit them (a redirect or pipe) or it calls `AttachConsole`. [AttachConsole][attach]
- Typed at a cmd prompt, cmd doesn't wait for a GUI program, so `%ERRORLEVEL%` isn't its exit code. In a `.cmd` script it does wait, and `start /wait` waits too. [start][start]
- Stream Deck's System → Open "lets you open an Application, Files, or Folders". Elgato doesn't document command-line arguments, exit codes, or any failure display for it. Website opens URLs. [Stream Deck System Actions][sd]
- Explorer ignores shortcut (`.lnk`) hotkeys while a fullscreen program runs, to protect games. [The Old New Thing][tont-hotkey]
- Power Display's CLI is the closest example: it uses exit codes 0–10 and writes errors to stderr. [Power Display][pd]

Do: return the exit code from `main` and write one plain line to stderr (it shows only if the caller redirected it). Tell Stream Deck users to point Open at a `.lnk` whose target is the copied command. Don't: show a message box (it takes focus from the game) or `AttachConsole` (its output lands after cmd's prompt), and don't recommend shortcut hotkeys for use during games.

## CI

- The CI virtual monitors come from MttVDD (`docs/research/ci-second-monitor.md`). Its driver source registers no IddCx I2C callback (`EVT_IDD_CX_MONITOR_I2C_*`; grep of [Virtual-Display-Driver@d724496](https://github.com/VirtualDrivers/Virtual-Display-Driver/tree/d7244969b2aa8bb38e76d79505eda217996cefea)), so dxva2 there almost certainly gets no DDC/CI. PowerToys built its own IddCx test driver with I2C callbacks to test DDC/CI in CI [fixture README][pd-fixture], which is too much for cardcast.

Do: unit-test parsing, input names, monitor selection, the toggle decision and the command grammar on Linux, and have the owner check real switching. Don't: write a CI test that needs a monitor to answer.

## Proposed tickets (in dependency order)

1. **Input switch decisions, Linux-tested.** Plain functions: input list from a capabilities string, input names ↔ values, EDID serial from EDID bytes, monitor selection by serial or name, the toggle decision, and parsing `cardcast.exe input …` into a request or exit code 2. *Accepted when* unit tests cover: `60(0F 11 12)`, no-space values, a missing outer parenthesis, garbage, a name that fits two monitors, `toggle` from each side, and every grammar error. No Windows calls.
2. **Command mode.** A new `input` module (add it to CODING_STANDARDS rule 2 and SPEC section 8) holding the dxva2 calls, the EDID read and the exit codes; `main` dispatches on arguments. *Accepted when* the Windows clippy check passes, and on the sender `cardcast.exe input 4GZY5P3 toggle dp hdmi1` switches the AW2523HF there and back from a `.lnk` on a Stream Deck Open key while a game has focus. Wrong input gives exit 1 and a wrong grammar gives exit 2. The owner also records whether Windows keeps the AW2523HF on the desktop while it shows HDMI 1.
3. **Settings cog, input switch button and Copy command.** The cog with the one toggle (off by default, saved), probing on an input thread while on, the tile button with ticked inputs, Copy command, and the status texts. *Accepted when* the snapshots show the cog closed and open and a tile with the button, the decision tests show nothing is probed while off, and the SPEC section 9 "Input switch" row passes on the sender.

[ddcci]: https://milek7.pl/ddcbacklight/ddcci.pdf
[mccs]: https://milek7.pl/ddcbacklight/mccs.pdf
[gpm]: https://learn.microsoft.com/en-us/windows/win32/api/physicalmonitorenumerationapi/nf-physicalmonitorenumerationapi-getphysicalmonitorsfromhmonitor
[high]: https://learn.microsoft.com/en-us/windows/win32/monitor/using-the-high-level-monitor-configuration-functions
[pm]: https://learn.microsoft.com/en-us/windows/win32/api/physicalmonitorenumerationapi/ns-physicalmonitorenumerationapi-physical_monitor
[mie]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-monitorinfoexw
[sdn]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_source_device_name
[tdn]: https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_target_device_name
[edm]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-enumdisplaymonitors
[wmi]: https://learn.microsoft.com/en-us/windows/win32/wmicoreprov/wmimonitorid
[drm-edid]: https://github.com/torvalds/linux/blob/master/include/drm/drm_edid.h
[sdi]: https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupdiopendevregkey
[edid-ovr]: https://learn.microsoft.com/en-us/windows-hardware/drivers/display/overriding-monitor-edids
[gcsl]: https://learn.microsoft.com/en-us/windows/win32/api/lowlevelmonitorconfigurationapi/nf-lowlevelmonitorconfigurationapi-getcapabilitiesstringlength
[crcr]: https://learn.microsoft.com/en-us/windows/win32/api/lowlevelmonitorconfigurationapi/nf-lowlevelmonitorconfigurationapi-capabilitiesrequestandcapabilitiesreply
[getvcp]: https://learn.microsoft.com/en-us/windows/win32/api/lowlevelmonitorconfigurationapi/nf-lowlevelmonitorconfigurationapi-getvcpfeatureandvcpfeaturereply
[setvcp]: https://learn.microsoft.com/en-us/windows/win32/api/lowlevelmonitorconfigurationapi/nf-lowlevelmonitorconfigurationapi-setvcpfeature
[ddi-i2c]: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/nc-dispmprt-dxgkddi_i2c_transmit_data_to_display
[idd-i2c]: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/iddcx/nc-iddcx-evt_idd_cx_monitor_i2c_transmit
[rapid-hpd]: https://devblogs.microsoft.com/directx/avoid-unexpected-app-rearrangement/
[pd]: https://learn.microsoft.com/en-us/windows/powertoys/power-display
[pd-design]: https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/powerdisplay/design.md
[pd-fixture]: https://github.com/microsoft/PowerToys/blob/main/.pipelines/powerdisplay-idd-poc/README.txt
[dd-faq]: https://www.ddcutil.com/faq/
[dd-caps]: https://www.ddcutil.com/command_capabilities/
[dd-perf]: https://www.ddcutil.com/performance_options/
[dd-nv]: https://www.ddcutil.com/nvidia/
[dd-dp]: https://www.ddcutil.com/displayport/
[tont-console]: https://devblogs.microsoft.com/oldnewthing/20090101-00/?p=19643
[tont-hotkey]: https://devblogs.microsoft.com/oldnewthing/20130528-00/?p=4233
[attach]: https://learn.microsoft.com/en-us/windows/console/attachconsole
[start]: https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/start
[sd]: https://help.elgato.com/hc/en-us/articles/360028234471-Elgato-Stream-Deck-System-Actions-Hotkey-Open-Website-Multimedia
