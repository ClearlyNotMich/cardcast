# Mirroring when the card display is on another graphics card

Question: the sender's RTX 4080 has no display head left for the card display. Can cardcast copy the
source monitor's picture to a card display plugged into a second GPU or the CPU's integrated GPU
(iGPU), and what does that cost the game? Researched 2026-11-27 on branch `research-cross-gpu`.

Short answer: yes. Keep capture and the HDR-to-SDR shader on the source monitor's GPU, create the
mirror's swap chain with that same device for the window on the card display, and let DXGI's
cross-adapter presentation move each finished 1080p SDR frame to the other GPU [4][5]. The CI spike
(section 3) mirrors across two adapters this way. Whether the game stays within SPEC section 9's
lines can only be measured on the sender.

## 0. Why the 4080 has no head for the card display

- NVIDIA: with a display in DSC mode, "the GPU may use two internal heads to drive the display when
  the pixel rate needed to drive the display mode exceeds the GPU's single head limit". Two such
  displays use "all 4 internal heads", leaving none for a third monitor [1].
- NVIDIA's open kernel display code agrees: it uses two heads for one output ("2Heads1OR") only when
  the display can do DSC (or hardware YUV 4:2:0) **and** the mode's pixel clock is above the head's
  maximum, which it reads from the hardware (`nvEvoUse2Heads1OR`, `maxPClkKHz`) [2]. That is the
  Linux driver; the Windows driver is closed, but the head is the same hardware.
- So the owner's explanation holds, with the caveat that NVIDIA publishes no per-head pixel-clock
  limit [1], so which of the AW2523HF (1080p 500 Hz) and MAG 272Q X50 modes take two heads is not
  verified. The S2417DG and the HD60 X being the ones left dark fits [1].
- README workaround, not the fix: a mode whose pixel rate is under the single-head limit frees a head
  [1][2]; so does turning DSC off, which some monitors allow by switching their DisplayPort mode
  from 1.4 to 1.2 [1]. Both cost refresh rate on the high-refresh monitor (without DSC the mode must
  fit the uncompressed link; our inference), and SPEC section 9 needs the source monitor at 500 Hz.

## 1. Options for getting the frames onto the other GPU

Data per copied frame (arithmetic): 1920x1080 at 4 B (8-bit BGRA) is 8.3 MB, so 0.5 GB/s at 60 Hz;
at 8 B (FP16) it is 16.6 MB. Microsoft gives no absolute latency for any option below.

| | (a1) Duplication + D3D11 shared handle | (a2) Duplication + D3D12 cross-adapter heap | (a3) Duplication + staging readback | (b) WGC on the card display's GPU | (c) Duplication + cross-adapter present |
|---|---|---|---|---|---|
| Documented? | No cross-adapter mode documented for D3D11 sharing [6][7] | Yes [8] | Yes (Map) [10][11] | Works, but undocumented on Learn; Microsoft engineer: copies via system memory [12][13] | Yes, DXGI since Windows 8.1 [4][5] |
| Copy done by | n/a | app, on GPUs, through system memory [8] | app: GPU to staging, CPU memcpy, upload | Windows, through system memory [12][13] | Windows: render GPU copies into a cross-adapter resource; display GPU copies or scans it out [4][5] |
| Extra latency | n/a | about one copy, app-synchronised with fences [8] | a `Map` waits for the GPU [10]; double-buffering to avoid that adds one frame (our inference) | not documented | two copies, one with CASO [4] |
| 4080 load at 1080p60 SDR | n/a | shader + copy into L0 system memory [8] | shader + copy to staging | DWM draws the capture on the source GPU [14]; frames cross unscaled | shader + one 8.3 MB copy per present |
| CPU load | n/a | low | 1-2 memcpy of 8.3 MB per frame | low | low |
| HDR-to-SDR on | n/a | 4080 | 4080, before readback | the card display's GPU, after an FP16 copy (HDR on) | 4080, before the copy (today's shader) |
| Code change | n/a | new D3D12 path; no sharing across D3D11/D3D12 for cross-adapter [8] | readback + upload loop | new capture path; superseding ADR 0002 | device on the source GPU, swap chain unchanged (spike: 10 lines) |

Notes:
- (a1) Microsoft's D3D11 sharing pages (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE`,
  `OpenSharedResource1`) say nothing about another adapter [6][7]; the D3D12 page says
  cross-adapter sharing needs `D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER` and "there is no mechanism to
  share cross-adapter resources across D3D API versions" [8]. Not an option on the evidence.
- (a2) "Cross-adapter shared resources are only supported in system memory" (`D3D12_MEMORY_POOL_L0`),
  which "is not efficient for discrete/NUMA adapter architectures", and need barriers and cross-adapter fences
  [8]. The Desktop Duplication frame is D3D11, so it would first need a D3D11-to-D3D12 share
  on the 4080 [12].
- (b) Desktop Duplication insists on a device on the output's adapter [3][12]. WGC "will allow the
  capture to happen but will spill the bits into system memory" (Microsoft Windows Composition team
  [12][13]); "things will be faster if you use the GPU the DWM is using to scan out" [13]. OBS relies
  on this: Automatic display capture switches to WGC when the monitor is not on OBS's adapter
  [15][16][17]. WGC also brings back the yellow border ADR 0002 avoided [18], and its frame rate
  follows the source monitor, so unscaled FP16 frames could cross far more often than 60 times a
  second (not documented; our inference).
- (c) is what Windows does for hybrid laptops: the app renders on the discrete GPU and, at
  `Present`, the runtime copies the back buffer to a cross-adapter resource that the integrated GPU's
  DWM composes or flips [5][9]. Microsoft: "Starting in Windows 8.1 (WDDM 1.3), D3D9 and DXGI
  applications were able to use cross-adapter presentation support on multi-adapter configurations
  such as hybrid systems", with two copies; Cross-Adapter Scan-Out (CASO, Windows 11 / Server 2022,
  WDDM 2.9+, flip model) drops it to one when the display GPU's driver supports it [4][19]. The
  Desktop Duplication device stays on the source's adapter, as [3] requires.

## 2. SPEC section 9: which option protects the game most

- All options duplicate (or WGC-capture) the same source monitor, so the game's PresentMon
  presentation mode should be affected the same way by capture itself (our inference; both capture
  paths are drawn by DWM [14]).
- None changes the source monitor's mode, so 500 Hz holds as long as the card display gets its head
  elsewhere (section 0).
- What differs is extra 4080 work. (c) adds the current shader plus one 8.3 MB copy per card display
  refresh, with no CPU in the loop. (a3) adds the same copy plus a CPU wait or one frame of lag.
  (b) moves the conversion off the 4080 but makes Windows copy whole frames through system memory
  [12][13]. (a2) is close to (c) in GPU work but with much more code [8].
- Recommendation for the Forza 2% line: (c). It also keeps the existing capture and shader code,
  and is the path Microsoft has optimised (CASO) [4][19]. Measure it with the section 9 benchmark;
  no source gives a number for a 4080.

## 3. Can CI test it

Yes, for function. The `second-monitor` job has the Hyper-V monitor on one DXGI adapter and the
virtual monitors on another (docs/research/ci-second-monitor.md). Spike on this branch: `Screen::open`
creates the device on the source monitor's adapter instead of the card display's, and the old
refusal test now starts the mirror from the Hyper-V monitor onto a virtual monitor. Run
<https://github.com/ClearlyNotMich/cardcast/actions/runs/46039255018> (commit `73db1d4`): source
monitor on adapter 0, card display on adapter 1, both named "Microsoft Basic Render Driver"; the
mirror started and kept presenting, 44.8 presents a second on the 30 Hz virtual card display (unpaced,
as ci-second-monitor.md found on one adapter: 47 at 30 Hz). This proves DXGI presents across two adapters
from cardcast's code, not the cost: both adapters are software, so the 4080-to-iGPU copy, CASO and
pacing can only be checked on the sender. The spike is reverted on this branch; a real change would
keep the cross-adapter test in the `second-monitor` job.

## Recommendation and risks

Option (c): duplicate on the source monitor's GPU, convert HDR to SDR and scale there, present to a
flip-model swap chain on the card display's window made with that device; DXGI does the cross-adapter
copy [4][5]. This contradicts SPEC section 1 ("the picture is never copied between graphics cards"),
which the owner must change; ADR 0002 (Desktop Duplication) still holds.

Risks: (1) the copy's cost on the 4080 and the latency are unmeasured; (2) presents may be paced
by the wrong monitor's refresh, as CI already saw on a virtual monitor; (3) without CASO the
display GPU does a second copy per frame [4], and CASO is announced for hybrid laptops with recent
Intel/AMD iGPUs, not desktops [19]; (4) device-removed handling must cover both GPUs; (5)
whether the DSC monitors really take two heads at their current modes is not verified [1].

## Sources

1. NVIDIA KB 5338, "Using high resolution/refresh rate displays with VESA DSC support on NVIDIA GeForce GPUs" (updated 2024-05-23), archived <https://web.archive.org/web/20260306190358/https://nvidia.custhelp.com/app/answers/detail/a_id/5338/kw/DSC> (live site blocks scripts)
2. NVIDIA/open-gpu-kernel-modules @a4b95f6a, `src/nvidia-modeset/src/nvkms-evo.c` (`nvEvoUse2Heads1OR`) and `nvkms-evo3.c` (`maxPClkKHz`), <https://github.com/NVIDIA/open-gpu-kernel-modules/blob/a4b95f6a2db0bba10ccaccef79190a3c18dcf14a/src/nvidia-modeset/src/nvkms-evo.c>
3. <https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1>
4. Supporting cross-adapter resource scan-out (CASO), <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/supporting-caso>
5. Rendering on a discrete GPU using cross-adapter resources, <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/rendering-on-a-discrete-gpu-using-cross-adapter-resources>
6. <https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_resource_misc_flag>
7. <https://learn.microsoft.com/en-us/windows/win32/api/d3d11_1/nf-d3d11_1-id3d11device1-opensharedresource1>
8. Shared heaps (D3D12), <https://learn.microsoft.com/en-us/windows/win32/direct3d12/shared-heaps>
9. Using cross-adapter resources in a hybrid system, <https://learn.microsoft.com/en-us/windows-hardware/drivers/display/using-cross-adapter-resources-in-a-hybrid-system>
10. <https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map>
11. <https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_map_flag>
12. robmikh (Microsoft, Windows Composition team, <https://github.com/robmikh>), 2022-06-16, <https://github.com/robmikh/Win32CaptureSample/issues/46>
13. robmikh, 2020-06-08 and 2024-05-07, <https://github.com/microsoft/Windows.UI.Composition-Win32-Samples/issues/61>
14. robmikh, 2022-10-16, "both WGC and DDA are drawn by the system compositor (DWM)", <https://github.com/robmikh/Win32CaptureSample/issues/48>
15. OBS @60a50017, `plugins/win-capture/duplicator-monitor-capture.c` (`choose_method`), <https://github.com/obsproject/obs-studio/blob/60a50017d70136375c78da5e15433597c9f3af7c/plugins/win-capture/duplicator-monitor-capture.c>
16. OBS @60a50017, `libobs-d3d11/d3d11-duplicator.cpp` (`device_duplicator_get_monitor_index` searches only OBS's adapter), <https://github.com/obsproject/obs-studio/blob/60a50017d70136375c78da5e15433597c9f3af7c/libobs-d3d11/d3d11-duplicator.cpp>
17. OBS KB, Laptop Troubleshooting, <https://obsproject.com/kb/laptop-troubleshooting>
18. <https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture>
19. DirectX blog, "Optimizing hybrid laptop performance with Cross Adapter Scan-Out (CASO)", 2023-09-25, <https://devblogs.microsoft.com/directx/optimizing-hybrid-laptop-performance-with-cross-adapter-scan-out-caso/>
