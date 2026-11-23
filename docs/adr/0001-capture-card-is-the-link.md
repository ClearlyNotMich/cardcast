# The link is a capture card, not a network

The project started as a plan to send raw frames from the sender to the receiver over a USB4 or Ethernet network. We chose instead to show the picture on the card display of a capture card plugged into the sender, and let the receiver read the card.

The goal is near-zero sender load. With a capture card the picture leaves the sender through a video port, so it is never copied into memory or pushed through the network stack, and the receiver needs no software of ours. The sender has no USB4 port, so the network route also needed new hardware, while the capture cards were already owned.

## Considered options

- **Raw frames over a network**: no capture card needed, but the highest sender load of the three and it needs a link of 1.5 Gbit/s or more on both PCs.
- **Hardware-encoded video over the existing network**: no new hardware, but lossy, and it still captures and encodes on the sender.
- **Windows duplicate mode onto the capture card**: no software at all, but it took away the main monitor's high refresh rate and added input lag.

## Consequences

The receiver must have a capture card. The card display stays part of the Windows desktop, so keeping windows and the cursor off it becomes the project's job.
