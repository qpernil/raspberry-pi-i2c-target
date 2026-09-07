# Architecture and lifecycle

## Components

| Component | Language | Responsibility |
| --- | --- | --- |
| `bcm27xx_bsc_target.ko` | C | MMIO, IRQ/FIFO service, timer, transaction queues, character device |
| Pi 3/Pi 4 overlays | Device Tree | MMIO/IRQ description, model-specific pins, active and idle pinctrl policy |
| `target-driver` | Rust | Temporary overlay/module lifecycle with echo/receive modes or a profile-supervised worker |
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
Upgrade the controller, driver and HSM frontend together. Receive-only display
workloads do not require READY and retain their 1,024-record receive ring.

The controller holds the physical bus lock while writing a complete request.
The driver invalidates the previous result when new input arrives and clears
old transmit data after the receive burst ends. It then acknowledges cleanup
with a deasserting READY edge (physical rising edge). If READY was already
inactive, a short assertion ensures this edge still exists; the assertion is
held for 20 µs so the controller GPIO can latch it. During this request phase,
that pulse is an acknowledgment, not a response. Arm rising-only GPIO detection before writing and discard stale events. After
acknowledgment, arm falling-only detection and check the current response level.
A reply arriving before reconfiguration stays asserted; a later reply wakes the
waiter. Linux both-edge detection may classify an interrupt by sampling the pin
in its deferred handler, mislabeling a short inactive interval. Single-edge
selection avoids that ambiguity without extending response timing.

After the acknowledgment the controller releases the bus, waits for the next
READY assertion, then reacquires the bus to read the exact response length.
The driver queues only response bytes. READY stays asserted after the read;
there is no guard byte, fallback marker, drain timeout, or post-read reset.
The next request clears any leftovers, including abandoned reads. No peripheral
reset occurs while another target is computing or publishing its response.

One worker executes requests sequentially. With READY, the driver retains only
one pending request, replacing it when newer input arrives. A worker response
is published only if it belongs to the latest receive generation; a superseded
write succeeds but discards its bytes. This cannot undo an executed operation's
side effects. A controller must not automatically replay uncertain commands.
A read and its corresponding write belong to one worker; concurrent worker
reads are not supported. Requests must have a STOP and wait for acknowledgment
before another request; arbitrary adjacent writes can merge into one record.

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
controller initialization, address/page commands, and fixed-size data payloads
across arbitrary `read()` boundaries and owns the sole 1,024-byte display RAM. SDL
expands that RAM into a streaming ARGB texture. SH1106 presentation occurs on
page 7, a lower-page wrap, or a 75 ms incomplete-frame timeout; SSD1306 presents
after its complete framebuffer payload. Rendering is lossless and remains in
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

Interrupt bit 2 is the BSC break condition, not a receive-timeout interrupt.
The periodic timer is therefore required even when no FIFO threshold interrupt
occurs. It observes `RXBUSY` clearing to finish the current receive burst; a
short idle gap can pass entirely between observations, in which case adjacent
I²C writes are deliberately retained in one record rather than losing bytes.

The BSC can report FIFO empty and TXBUSY clear with a byte still in its
serializer. Direct register tests on Pi 3B+ confirm identical flags after
reading two of three bytes and after reading the third. A fully consumed
response can be followed by a fresh response without resetting the peripheral.
The driver therefore does not infer response completion from FIFO flags.
It purges FIFO and serializer only at the next request, using `TXFLEVEL + 1`
disable/enable transitions; the documented BRK does not reliably purge them.

Run `python3 tests/tx_serializer.py` to exercise the actual C receive, refill,
and publication functions against the FIFO/serializer model. It covers long
staged reads without a guard or reset, abandoned last bytes, and replacement
of pending requests while an older worker is executing. Hardware qualification
is needed for electrical behavior and GPIO/IRQ timing.

## Profile-supervised target workers

`target-driver --profile NAME_OR_PATH` uses its existing driver guard while
launching the installed `usb-gadget-supervisor`. The target launcher never
opens `/dev/bsc-target0` in this mode; the supervisor opens it according to its
root-owned device profile and passes the handle to the unprivileged worker.
Stop and reload signals are forwarded, and the supervisor is reaped before
unloading. This path introduces no device-protocol implementation or Rust
crate dependency between the driver project and the worker. Driver loading,
privilege dropping, and HSM behavior remain in their owning executables.
