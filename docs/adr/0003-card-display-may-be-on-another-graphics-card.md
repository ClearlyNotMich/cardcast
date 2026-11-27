# The card display may be on another graphics card

On 2026-11-23 the owner's first test of v0.1.0-alpha.1 found that the card display got no signal on the sender. The RTX 4080 has 4 display heads, and its two high-refresh monitors run with Display Stream Compression, which can take two heads each (NVIDIA KB 5338, cited in `docs/research/cross-gpu-mirror.md`). The owner asked for the card display to be plugged into a second GPU or the CPU's integrated GPU instead. This replaces SPEC section 1's rule that the card display is on the same graphics card and the picture is never copied between graphics cards, and #7's refusal of that setup.

The mirror keeps Desktop Duplication (ADR 0002) and the HDR-to-SDR shader on the source monitor's graphics card. It presents to a flip-model swap chain made with that same device for the window on the card display, and DXGI copies each finished 1080p SDR frame to the other card at present. Microsoft documents this cross-adapter present for DXGI apps since Windows 8.1. It is how hybrid-GPU laptops work.

## Considered options

- **D3D11 shared handles across adapters**: no documented way to do it.
- **D3D12 cross-adapter heaps**: system memory only, and much more code.
- **Staging readback through system memory**: CPU copies, and a frame of extra lag if double-buffered.
- **Windows Graphics Capture with a device on the card display's GPU**: works (OBS relies on it), but copies whole frames through system memory and brings back the capture border that ADR 0002 avoided.
- **Cross-adapter present (chosen)**: no new capture code, the conversion happens before the copy, and only 8-bit 1080p frames (about 0.5 GB/s at 60 Hz) cross between the cards.

## Consequences

The cost and latency of the copy on the sender are unmeasured. The SPEC section 9 sender-load (Forza within 2%) and PresentMon lines decide whether this choice holds. Presents must be paced by the card display's refresh, not the source monitor's. Device loss on either card must restart the mirror. CI can test this path for function, because the `second-monitor` job has its monitors on two adapters. It can't measure cost: both CI adapters are software renderers. A setup with both monitors on one card keeps working as before.
