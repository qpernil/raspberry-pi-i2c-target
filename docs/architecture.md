# Architecture and lifecycle

## Components

| Component | Language | Responsibility |
| --- | --- | --- |
| `bcm27xx_bsc_target.ko` | C | MMIO, IRQ/FIFO service, timer, transaction queues, character device |
| Pi 3/Pi 4 overlays | Device Tree | MMIO/IRQ description, model-specific pins, active and idle pinctrl policy |
| `target-driver` | Rust | Self-contained temporary overlay/module lifecycle with echo and receive-only diagnostic modes |
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
- `write()` publishes the response to the last request read by the worker,
  or discards it if a newer request has arrived.
- `poll()` reports readable requests and whether a worker has a response due.
- `BSC_TARGET_IOC_GET_INFO` reports ABI 3; `reserved[0]` is 1 with READY configured.
- `BSC_TARGET_IOC_GET_STATS` and sysfs `stats` report counters. `tx_transactions`
  and `tx_bytes` count published responses, not inferred wire consumption;
  `tx_discarded` counts superseded worker results.
- Transactions are limited to 8192 bytes.

Response mode requires an active-low, open-drain READY GPIO and driver ABI 3.
The HSM frontend and connector require a recovery-capable driver and coordinated
updates; other ABI 3 workers retain their byte interface. Receive-only display workloads do not require READY and retain their 1,024-record receive ring.

The [raw exchange and explicit-recovery contract](response-transport.md) preserves
application bytes and the 8192-byte limit. Normal receives acknowledge acceptance
and keep BSC enabled in I2C mode without purging or reconfiguration. The HSM
connector retains rising-only READY detection and polls the response level.
The shared bus lock covers request acceptance and separately the complete read.

The target application uses `BSC_TARGET_IOC_RECOVER` only for explicit recovery
after an incomplete exchange or reconnect. The generic driver recognizes no
wire commands or checksums. The HSM frontend implements its reserved recovery
operation through this ioctl, outside the HSM core. Existing raw echo clients
and receive-only streams retain their data formats; clients abandoning replies
must arrange explicit recovery or a quiescent target restart. Character-device
ABI 3 gains a recovery capability flag without changing its existing layouts.

One worker executes requests sequentially. With READY, the driver retains only
one pending request, replacing it when newer input arrives. A worker response
is published only if it belongs to the latest receive generation; a superseded
write succeeds but discards its bytes. This cannot undo an executed operation's
side effects. A controller must not automatically replay uncertain commands.
A read and its corresponding write belong to one worker; concurrent worker
reads are not supported. Requests must have a STOP and wait for acknowledgment.
An incomplete exchange requires explicit recovery before another normal request;
arbitrary adjacent writes are unsupported.

The bus lock also covers the entire header/body read. It does not cover HSM
computation, so targets with separate READY lines can progress concurrently.
All controllers sharing the physical bus must cooperate in this lock; other
kernel drivers and raw clients do not do so automatically. Administrative
activation, close and unload still require quiescent controller traffic.

`target-driver --receive-only` opens the character device without writing
responses. The kernel peripheral ACKs controller writes while the application
drains complete transactions promptly and reports compact totals. This is the
appropriate mode for write-only protocols such as an SSD1306 display stream.

`virtual-display` independently loads the target driver, opens the character
device read-only, and interprets the byte stream in userspace. It defaults to
SH1106; `--display=ssd1306` selects the other supported controller. It has no
runtime dependency on `target-driver`. The parser recognizes
controller initialization and address/page commands across arbitrary `read()`
boundaries. SSD1306 payload length follows its declared column/page rectangle.
SH1106 range updates use native ending-column then starting-column commands to
declare the exact small payload length; physical displays execute those same
commands and finish at the starting column. Legacy full-width page writes are
also supported. Unframed variable-length SH1106 data is ambiguous after record
aggregation. The parser and owns the sole 1,024-byte display RAM. SDL
expands that RAM into a streaming ARGB texture. SH1106 presentation occurs on
page 7, a lower-page wrap, or a 75 ms incomplete-frame timeout; SSD1306 presents
after each complete rectangle payload. Partial SH1106 frames render within
75 ms of the first pending update even when further updates keep arriving. Rendering is lossless and remains in
the drain loop. Optional `--vsync` may therefore create receive-queue pressure;
the 1,024-record kernel ring absorbs finite lag and its documented newest-wins
overflow policy handles longer delays.

`virtual-display` defaults to GPIO5/GPIO26 as active-low open-drain outputs.
SDL's left, middle, and right thirds drive left, both, and right states.
`--button-outputs=LEFT,RIGHT` overrides those lines;
`--no-button-outputs` disables them. `--title TEXT` overrides the generic
window title.

## Lifecycle state machine

| State | SDA/SCL GPIO | READY (when configured) | BSC peripheral | IRQ/timer | I²C behavior |
| --- | --- | --- | --- | --- | --- |
| Overlay/module absent | Existing system state | Unmanaged | Unmanaged | None | No target supplied by this project |
| Loaded, never opened | Preserved as found | Released | Disabled | IRQ registered but masked; timer stopped | Address is not acknowledged |
| Character device open, no response | ALT3, no internal pull | Released | Enabled at configured address | FIFO IRQs enabled; timer running | Requests active; reads must await the response READY assertion |
| Complete response queued | ALT3, no internal pull | Asserted low | Enabled at configured address | FIFO IRQs enabled; timer running | Controller may read the response |
| Final close | Input with configured idle pull | Released | Disabled and queues cleared | Masked/stopped | Address is not acknowledged |
| `SIGKILL` after final descriptor | Same as final close | Released | Disabled | Masked/stopped | Module/overlay remain inert |

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
2. A configurable high-resolution timer (300 µs by default) catches receive
   tails below the interrupt threshold, detects receive completion, and refills
   responses without inferring whether the controller consumed their last byte.

The timer exists only while the device is open. Its Device Tree range is 20–500
µs through the `poll_ns` overlay parameter.

FIFO safety comes from the receive threshold interrupt, not from the timer. At
the three-quarter-full threshold, the hard-IRQ handler is notified with four of
the 16 FIFO slots still available. If the timer happens to drain fewer than 12
bytes first, a continuing transfer simply reaches the threshold again after
the next 12 bytes. The timer interval therefore trades receive-tail latency
against callback overhead; its phase relative to a transfer is not a FIFO
safety deadline. An interrupt handler delayed long enough for the remaining
FIFO capacity to fill can still overrun because this peripheral cannot stretch
the controller's clock.

The transmit threshold is seven-eighths full: refill interrupts start with 14
FIFO bytes remaining, providing approximately 315 µs at 400 kHz before the FIFO
empties. The receive threshold remains three-quarters full. Transmit underruns
can corrupt a reply even when the controller reports a full-length read; inspect
the driver counters as well as transport results.

Interrupt bit 2 is the BSC break condition, not a receive-timeout interrupt.
The periodic timer is therefore required even when no FIFO threshold interrupt
occurs. It requires receive-idle and an empty RX FIFO in the same `FR` snapshot before
publishing a receive burst. A final byte arriving after the drain observed an
empty FIFO is retained for the next service pass. A
short idle gap can pass entirely between observations, in which case adjacent
I²C writes are deliberately retained in one record rather than losing bytes.

The BSC can report FIFO empty and TXBUSY clear with a byte still in its
serializer. Direct register tests on Pi 3B+ confirm identical flags after
reading two of three bytes and after reading the third. A fully consumed
response can be followed by a fresh response without resetting the peripheral.
The driver therefore does not infer response completion from FIFO flags.
It purges FIFO and serializer only during explicit recovery, using `TXFLEVEL + 1`
disable/enable transitions; normal receives do not reset BSC. The documented BRK
does not reliably purge them.

Run `python3 tests/tx_serializer.py` to exercise the actual C receive, refill,
and publication functions against the FIFO/serializer model. It covers long
staged reads without a guard or reset, abandoned last bytes, and replacement
of pending requests while an older worker is executing. Hardware qualification
is needed for electrical behavior and GPIO/IRQ timing.

## Profile-supervised target workers

The shared `kernel_target` library exposes both automatic Pi 3/4 selection for
self-contained diagnostics and a configured lifecycle API for privileged
resource supervisors. `usb-gadget-supervisor` uses the configured API: its
root-owned profile supplies module and overlay names, supported model matches,
target pins, address, READY GPIO, and the artifact directory. The supervisor
loads the matching BSC target, passes `/dev/bsc-target0` to the unprivileged
worker, and unloads the resource after closing the worker descriptor.

`target-driver` does not launch profile workers. It retains its original role
as the standalone echo/receive diagnostic that owns the same kernel lifecycle.
