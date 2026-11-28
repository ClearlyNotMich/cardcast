# A second monitor on the `windows-latest` CI runner

Question: can the GitHub-hosted `windows-latest` runner get a second (virtual) monitor, so the
window guard, pointer guard and mirror can be tested across two monitors (issue #13)?

Short answer: yes for monitors. A pinned release of VirtualDrivers/Virtual-Display-Driver installs
without interaction or test-signing, adds a second monitor, and can be unplugged and replugged from
PowerShell. Whether Desktop Duplication works on that monitor is not yet measured.

Spike: branch `ci-second-monitor-spike`, workflow `.github/workflows/spike.yml`, run
<https://github.com/ClearlyNotMich/cardcast/actions/runs/45154457083> (success). "Spike log" below
means that run.

## 1. Which virtual display drivers install non-interactively

| Option | Licence | Signing | Last push | Notes |
|---|---|---|---|---|
| VirtualDrivers/Virtual-Display-Driver ("MttVDD") | MIT [1] | README lists it as signed, via SignPath Foundation [2]; release 24.10.27 zip ships a `.cer` and `mttvdd.cat` [3] | 2026-09 [1] | Maintained, ~10.6k stars [1]; also on winget [2] |
| microsoft/Windows-driver-samples `video/IndirectDisplay` (IddCx sample) | MS-PL [4] | Unsigned source; we would have to build and sign it [5] | 2026-11 [4] | Reference code, not a ready driver |
| ge9/IddSampleDriver, roshkins/IddSampleDriver | no licence detected by GitHub [4b] | self-signed | 2023-12 / 2024-01 [4b] | Unmaintained forks of the sample |

- An IDD is a user-mode (UMDF) driver built on IddCx, for monitors not attached to GPU outputs [6].
- The spike installed MttVDD 24.10.27 with no test-signing: it pinned the zip by sha256, imported
  the bundled `Virtual_Display_Driver.cer` into LocalMachine `Root` and `TrustedPublisher`, created a
  root device `Root\MttVDD` with SetupAPI (`SetupDiCreateDeviceInfo`, `SetupDiCallClassInstaller`)
  and installed `MttVDD.inf` with `UpdateDriverForPlugAndPlayDevices`. Result `ok reboot=False`
  (spike log). Test-signing (`bcdedit /set testsigning on`) needs a reboot [7], which a hosted job
  cannot survive, so a driver that needs it is not an option here.
- The runner is a Hyper-V VM: its only display adapter is "Microsoft Hyper-V Video", one 1024x768
  monitor (spike log). The image ships the WDK Visual Studio extension [8], but we did not need it.
- Prior art on GitHub Actions: code-yeongyu/oh-my-openagent installs the same pinned release on
  `windows-latest` the same way (cert into Root/TrustedPublisher, root device, `UpdateDriver`), and
  records the result as CONFIRMS: "a second monitor through the pinned indirect display driver reads
  144 effective DPI at 150 %" [9][10][11]. zhangyushao0/zpenflow stages VDD in its `windows-latest`
  CI too, but for packaging, not tests [12].

## 2. Add, remove and re-add from a script

Measured in the spike log, using `pnputil /disable-device` and `/enable-device` [13]:

| Step | `Screen.AllScreens` |
|---|---|
| before | `\\.\DISPLAY1` 1024x768 primary |
| after install | `DISPLAY1` + `\\.\DISPLAY2` 800x600 at X=1024 |
| `pnputil /disable-device ROOT\DISPLAY\0000` (unplug) | `DISPLAY1` only; VDD device status `Error` |
| `pnputil /enable-device` (replug) | `DISPLAY1` + `\\.\DISPLAY3` 800x600 at X=1024 |

The GDI device name changes on replug (`DISPLAY2` -> `DISPLAY3`), while the monitor PnP id stays
`DISPLAY\MTT1337\...` (spike log). Code that finds the card display again after a replug must not
key on the `\\.\DISPLAYn` name. `pnputil /remove-device` removes the node outright [13][11].
Resolution and count of virtual monitors come from `vdd_settings.xml` [3][2].

Measured in CI (issue #57, run 46132735433 attempt 2): a disable of `ROOT\DISPLAY\0000` about
150 ms after its re-enable hung 5 s and printed "System reboot is needed to complete configuration
operations!", although `CM_Get_DevNode_Status` [16] read it as Started with no problem (status
`0x180200b`) and all its monitors were back in the display configuration. Afterwards the status
had `DN_NEED_RESTART` (`0x180210b`) and every later disable failed with "Device is pending system
reboot". Disables at least 1 s after a replug never failed. Likely cause, not proved: the 24.10.27
driver has one swap-chain worker for all monitors, which each new swap-chain assignment tears
down (fixed upstream by PR #482 [17], in no release yet), or DWM is still attaching to the new
outputs; no devnode status shows either. So `Unplugged` (`guard::second_monitor`) waits before a
disable until the device is Started, and after a replug until it is Started, its monitors are
back, and Desktop Duplication delivers a frame from each. Duplication is not checked before a
disable: a running mirror in the test process may hold the only duplication of that output.

## 3. Desktop Duplication and D3D11 on that monitor

- Measured since (CI job `second-monitor` in `ci.yml`, tests named `second_monitor_*`): DXGI lists
  the virtual monitors under their own adapter (1), not the Hyper-V one (0). Duplicating a virtual
  monitor works, and the mirror starts and presents with one virtual monitor as source monitor and
  a second (`<count>2</count>` in `vdd_settings.xml`) as card display. Presents are not paced to
  the virtual monitor's mode (47 a second at 30 Hz). The text below is from before that.
- Higher refresh rates (issue #31): the virtual monitors offer 30 to 165 Hz at each resolution
  (`vdd_settings.xml`), and `ChangeDisplaySettingsExW` sets the card display to 120 Hz, which
  `guard::monitors` then reads. The mirror runs on it, but its presents follow the runner's
  64 Hz Hyper-V monitor, not the virtual monitor: 49.8 a second at 30 Hz and 63.9 at 120 Hz
  (run 46056861903). A virtual monitor has no vertical blank, so CI can't prove that the mirror
  follows a 120 Hz refresh, or that nothing caps it at 60. That stays an owner check on a
  capture card that takes 120 Hz (issue #13); `second_monitor_mirror_runs_on_a_card_display_set_to_120_hz` checks the rest.
- Not measured yet. The spike only listed monitors.
- With no GPU, WARP backs D3D11 up to feature level 11_1 through the Microsoft Basic Render
  Adapter, and is present in DXGI adapter enumeration [14]. The runner's display adapter is the
  Hyper-V Video adapter (spike log).
- Desktop Duplication: callers must handle `DXGI_ERROR_ACCESS_LOST` on mode changes and
  desktop switches [15]. Unplug/replug in section 2 is exactly such a change.
- Open: whether `IDXGIOutput1::DuplicateOutput` succeeds on the IDD output, and whether DXGI lists
  that output under the WARP/Hyper-V adapter or under its own. Next spike: run the existing
  `src/mirror/` Windows tests after installing VDD.

## 4. Licence and supply-chain risk

- MttVDD: MIT [1], compatible with using it in CI (we do not ship it). Risk: the job trusts a
  third-party certificate as a root CA and loads third-party code into a driver host. On an
  ephemeral hosted runner that VM is discarded after the job, so the blast radius is that job and
  its secrets; run the VDD steps in a job with no secrets. Pin by release tag and sha256
  (`b7a36f34...42fd` for 24.10.27 x64, spike workflow), as [11] does. The latest release is from
  2024-10 [3]; a newer image may refuse it, which pinning surfaces as a CI failure, not silently.
- IddCx sample: MS-PL [4]; lowest supply-chain risk, but we would own building and signing a
  driver, which is much more work.
- ge9/roshkins forks: no licence detected [4b] and unmaintained; avoid.

## Recommendation

Use VirtualDrivers/Virtual-Display-Driver 24.10.27, pinned by sha256, installed as in the spike,
in a Windows CI job without secrets. Before writing multi-monitor tests, extend the spike to run
Desktop Duplication on the virtual monitor (section 3).

## Sources

1. GitHub API, <https://github.com/VirtualDrivers/Virtual-Display-Driver> (licence MIT, pushed, stars)
2. README, <https://github.com/VirtualDrivers/Virtual-Display-Driver/blob/master/README.md> (winget, signing table, SignPath)
3. Release 24.10.27, <https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/tag/24.10.27> (zip contents)
4. <https://github.com/microsoft/Windows-driver-samples> (MS-PL)
   4b. <https://github.com/ge9/IddSampleDriver>, <https://github.com/roshkins/IddSampleDriver>
5. <https://github.com/microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay>
6. <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview>
7. <https://learn.microsoft.com/en-us/windows-hardware/drivers/install/test-signing>
8. <https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md> (WDK VS extension)
9. <https://github.com/code-yeongyu/oh-my-openagent/blob/HEAD/.github/workflows/computer-live-windows-full.yml>
10. <https://github.com/code-yeongyu/oh-my-openagent/blob/HEAD/crates/omo-computer-backend-win32/probes/hosted-windows-live.md>
11. <https://github.com/code-yeongyu/oh-my-openagent/blob/HEAD/script/qa/computer/probes/hosted-windows-live/legs-display.ps1>
12. <https://github.com/zhangyushao0/zpenflow/blob/HEAD/.github/workflows/ci.yml>
13. <https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/pnputil-command-syntax>
14. <https://learn.microsoft.com/en-us/windows/win32/direct3darticles/directx-warp>
15. <https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api>
16. <https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_get_devnode_status>
17. <https://github.com/VirtualDrivers/Virtual-Display-Driver/pull/482>
