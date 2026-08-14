# Architecture and lifecycle

## Components

| Component | Language | Responsibility |
| --- | --- | --- |
| `bcm27xx_bsc_target.ko` | C | MMIO, IRQ/FIFO service, timer, transaction queues, character device |
| Pi 3/Pi 4 overlays | Device Tree | MMIO/IRQ description, model-specific pins, active and idle pinctrl policy |
| `target-driver` | Rust | Temporary overlay/module lifecycle and example request responder |
| `controller-long` | Rust | Long-message controller through Linux `i2c-dev` |
| `target` / `controller` | Rust | FIFO-bounded direct-MMIO demonstration protocol |

The kernel module is deliberately small. Protocol interpretation remains in
userspace; the driver transports complete I²C transactions.

## Hardware mapping

| Target | SDA/SCL | BSC physical base | Interrupt description |
| --- | --- | --- | --- |
| Pi 3B/3B+ | GPIO18/19 ALT3 | `0x3f214000` | VC peripheral IRQ 43 through the legacy controller |
| Pi 4B | GPIO10/11 ALT3 | `0xfe214000` | VC peripheral IRQ 43 mapped through GICv2 |

Both overlays describe the bus address as `0x7e214000`; Device Tree address
translation produces the model-specific CPU physical address.

## Character-device contract

`/dev/bsc-target0` permits one independent open file at a time.

- `read()` returns one complete controller-to-target write transaction.
- `write()` queues one complete response for a later controller read.
- `poll()` reports queued requests and response-slot availability.
- `BSC_TARGET_IOC_GET_INFO` reports ABI/configuration information.
- `BSC_TARGET_IOC_GET_STATS` and the sysfs `stats` attribute report counters.
- Transactions are limited to 8192 bytes.

The receive side holds four completed transactions. Additional completed writes
are drained and counted as dropped. A controller read with no queued response
cannot wait because the peripheral has no clock stretching; it underruns and is
counted.

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
