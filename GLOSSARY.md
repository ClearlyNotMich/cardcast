# Cardcast

Showing one PC's screen on a second PC so that the second PC can do all the streaming work.

## Language

**Sender**:
The PC whose screen is shown elsewhere. It is the PC the game runs on.
_Avoid_: Source, host, gaming PC, client

**Receiver**:
The PC that shows the sender's screen and does all encoding, recording and streaming.
_Avoid_: Sink, streaming PC, server

**Link**:
The direct connection that carries the screen from the sender to the receiver, whatever its cable or port type.
_Avoid_: USB link, cable, connection

**Card display**:
The extra display that appears on the sender when the receiver's capture card is plugged into it. It exists only to carry the picture and is never meant to be used as a screen.
_Avoid_: Extra screen, second monitor, fake monitor

**Source monitor**:
The sender's monitor whose picture is carried to the receiver.
_Avoid_: Main screen, game screen, captured display

**Mirror**:
The live copy of the source monitor shown fullscreen on the card display.
_Avoid_: Projector, stream, clone, duplicate

**Window guard**:
The rule that no window may stay on the card display; any window that lands there is returned to the "send windows to" monitor, which defaults to the primary monitor.
_Avoid_: Window lock, window mover

**Pointer guard**:
The rule that the mouse pointer may not stay on the card display; a pointer that gets there is put back where it came from.
_Avoid_: Cursor lock, mouse fence

**Audio source**:
The one playback device on the sender whose sound is carried to the receiver.
_Avoid_: Channel, audio track

**Connector**:
The graphics card output the card display is plugged into: the graphics card plus that output's target id in Windows. It stays when the capture card changes its EDID, so cardcast finds the card display by it when the card display's device path changes.
_Avoid_: Port, output, socket

**Sound output**:
The playback device on the sender that the audio source is played to, so the capture card carries it: Automatic (the card display's own sound output), a chosen playback device, or No audio for a capture card without sound.
_Avoid_: Audio sink, target device, card audio

**Sender load**:
The resources the sender spends on sending its screen instead of on the game. Keeping it near zero is the reason the project exists.
_Avoid_: Overhead, CPU usage
