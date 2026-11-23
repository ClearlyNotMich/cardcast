# The mirror captures with Desktop Duplication (provisional)

SPEC section 9 says an OBS capture check on the sender picks between Windows Graphics Capture (WGC) and DXGI Desktop Duplication before the mirror is written. On 2026-11-23 the owner asked for the build to continue without waiting for that check, so we chose Desktop Duplication on the evidence in `docs/win32-mirror-audio.md`.

Desktop Duplication is what OBS's Display Capture uses by default on a desktop PC, so it already runs on the reference sender. It shows no capture border and needs no package identity. WGC was preferred in SPEC because Windows would convert HDR to SDR and draw the pointer, but Microsoft does not document a good HDR-to-SDR result for an 8-bit frame pool (it may clip), and turning its yellow border off needs build 20348+ plus a consent step and a package capability that an unpackaged `.exe` may not get.

## Considered options

- **Windows Graphics Capture**: less code, but the two undocumented risks above can only be ruled out on the sender.
- **Desktop Duplication**: no border and known to work on the sender, but cardcast must convert HDR to SDR itself (using the monitor's SDR white level) and draw the pointer itself.

## Consequences

The mirror owns a HDR-to-SDR shader and pointer drawing, both of which need checks by the owner on the sender. Capture is kept behind one seam in `mirror` so that WGC could replace it. Issue #6 (the OBS check) stays open as a confirmation: if it shows WGC with Force SDR looks right with no border, a later ADR can supersede this one.
