# Architecture and lifecycle

## Components

| Component | Language | Responsibility |
| --- | --- | --- |
| `bcm27xx_bsc_target.ko` | C | MMIO, IRQ/FIFO service, timer, transaction queues, character device |
| Pi 3/Pi 4 overlays | Device Tree | MMIO/IRQ description, model-specific pins, active and idle pinctrl policy |
| `target-driver` | Rust | Self-contained temporary overlay/module lifecycle and echo/receive test modes |
| `virtual-display` | Rust | Self-contained target lifecycle, SSD1306/SH1106 parser, SDL viewer, and optional button GPIOs |
| `controller-long` | Rust | Long-message controller through Linux `i2c-dev` |
| `target` / `controller` | Rust | FIFO-bounded direct-MMIO demonstration protocol |

The kernel module is deliberately small. Protocol interpretation remains in
userspace; the driver transports observed receive bursts without assigning
protocol meaning to character-device record boundaries.

## Hardware mapping

| Target | SDA/SCL | BSC physical base | Interrupt description |
| --- | --- | --- | --- |
| Pi 3B/3B+ | GPIO18/19 ALT3 | `0x3f214000` | VC peripheral IRQ 43 through the legacy controller |
| Pi 4B | GPIO10/11 ALT3 | `0xfe214000` | VC peripheral IRQ 43 mapped through GICv2 |

Both overlays describe the bus address as `0x7e214000`; Device Tree address
translation produces the model-specific CPU physical address.

## Character-device contract

`/dev/bsc-target0` permits one independent open file at a time.

- `read()` returns one queued receive record. A record normally corresponds to
  one controller write, but adjacent writes can be aggregated when their
  STOP-to-START gap is shorter than the driver's observation interval.
- `write()` queues one complete response for a later controller read.
- `poll()` reports queued requests and response-slot availability.
- `BSC_TARGET_IOC_GET_INFO` reports ABI/configuration information.
- `BSC_TARGET_IOC_GET_STATS` and the sysfs `stats` attribute report counters.
- Transactions are limited to 8192 bytes.

The receive side holds 1,024 fixed-size records. Each slot contains a 32-bit
length and up to 8,192 bytes, so the dynamically allocated ring occupies about
8 MiB. When the ring is full, the oldest record is evicted and `rx_dropped` is
incremented so the newest device state remains available. `read()` copies and
dequeues one slot under the driver lock before copying it to userspace, preventing
a producer from overwriting a record being read. A controller read with no queued
response cannot wait because the peripheral has no clock stretching; it
underruns and is counted.

`target-driver --receive-only` opens the character device without writing
responses. The kernel peripheral ACKs controller writes while the application
drains complete transactions promptly and reports compact totals. This is the
appropriate mode for write-only protocols such as an SSD1306 display stream.

`virtual-display --display=ssd1306|sh1106` independently loads the target driver,
opens the character device read-only, and interprets the byte stream in
userspace. It has no runtime dependency on `target-driver`. The parser recognizes
controller initialization, address/page commands, and fixed-size data payloads
across arbitrary `read()` boundaries and owns the sole 1,024-byte display RAM. SDL
expands that RAM into a streaming ARGB texture. SH1106 presentation occurs on
page 7, a lower-page wrap, or a 75 ms incomplete-frame timeout; SSD1306 presents
after its complete framebuffer payload. Rendering is lossless and remains in
the drain loop. Optional `--vsync` may therefore create receive-queue pressure;
the 1,024-record kernel ring absorbs finite lag and its documented newest-wins
overflow policy handles longer delays.

With `--button-outputs=LEFT,RIGHT`, `virtual-display` requests the selected GPIOs
as active-low open-drain outputs. SDL's left, middle, and right thirds drive
left, both, and right states. `--title TEXT` overrides the generic window title.

## Lifecycle state machine

| State | GPIO | BSC peripheral | IRQ/timer | I²C behavior |
| --- | --- | --- | --- | --- |
| Overlay/module absent | Existing system state | Unmanaged | None | No target supplied by this project |
| Loaded, never opened | Preserved as found | Disabled | IRQ registered but masked; timer stopped | Address is not acknowledged |
| Character device open | ALT3, no internal pull | Enabled at configured address | FIFO IRQs enabled; timer running | Requests and responses active |
| Final close | Input with configured idle pull | Disabled and queues cleared | Masked/stopped | Address is not acknowledged |
| `SIGKILL` after final descriptor | Same as final close | Disabled | Masked/stopped | Module/overlay remain inert |

The driver does not snapshot and restore an arbitrary prior pin configuration.
It avoids touching a never-opened instance and, after use, selects the explicit
idle state supplied by Device Tree. `target-driver --idle-pull` selects that
outside policy.

A duplicated or inherited descriptor keeps the same open instance alive. The
hardware remains active until the last descriptor referring to it closes.

## FIFO servicing

The BSC target peripheral has a 16-byte FIFO and no DMA or clock stretching.
The driver uses two mechanisms:

1. Receive/transmit FIFO thresholds invoke a hard IRQ handler, which drains or
   refills the FIFO without waiting for userspace scheduling.
2. A configurable high-resolution timer (100 µs by default) catches receive
   tails below the interrupt threshold, detects receive completion, and releases
   fully loaded responses when the transmit FIFO becomes empty.

The timer exists only while the device is open. Its Device Tree range is 20–500
µs through the `poll_ns` overlay parameter.

Interrupt bit 2 is the BSC break condition, not a receive-timeout interrupt.
The periodic timer is therefore required even when no FIFO threshold interrupt
occurs. It observes `RXBUSY` clearing to finish the current receive burst; a
short idle gap can pass entirely between observations, in which case adjacent
I²C writes are deliberately retained in one record rather than losing bytes.

The BSC may preload bytes into its transmit serializer, and `TXBUSY` does not
reliably describe a complete I2C transaction. The driver therefore releases a
queued response only after all of its bytes have been loaded and the transmit
FIFO is empty. It does not reset the peripheral at that boundary because the
final byte may still be shifting onto the wire.

## Request/response boundary

Userspace cannot inspect a controller write and prepare a reply during an
immediate repeated START because the target cannot stretch SCL. The example
controller therefore performs:

1. One controller write.
2. A 20 ms processing interval.
3. One controller read of the known response length.

An application protocol should make this boundary explicit and include error
detection and retry semantics.
