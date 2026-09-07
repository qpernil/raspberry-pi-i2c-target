# BCM27xx BSC target kernel driver

This experimental out-of-tree driver services the BCM2835-family SPI/BSC I2C
target FIFO from interrupt context. It supports Raspberry Pi 3B/3B+ and Pi 4B.

The hardware has a 16-byte FIFO, no DMA, no clock stretching, and only 7-bit
target addresses. The driver combines a three-quarter-full receive threshold
interrupt with a configurable high-resolution timer (300 µs by default). The
timer drains sub-threshold receive tails, detects receive completion, and
refills pending responses from the same locked context. The
Device Tree `poll_ns` parameter accepts intervals from 20 to 500 µs.

## Character-device interface

The overlay and loaded module create `/dev/bsc-target0` at address `0x13` by
default. They leave the BSC peripheral disabled and its timer stopped, and do
not alter the existing GPIO configuration until an application opens the
character device.

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

The character device permits one open at a time. `read()` dequeues a request;
`write()` publishes or discards its result atomically against new input. See
[`bsc_target_uapi.h`](bsc_target_uapi.h) for ABI 3 configuration and counters.
`reserved[0]` in GET_INFO reports whether READY is configured. Responses cannot
be written without READY. Published-byte counters do not prove wire delivery.

Opening the device selects the target pins, enables the peripheral and starts
the high-resolution timer. The final close reverses those actions and clears
queued I/O, selecting the externally configured idle input state. Consequently,
normal exit and `SIGKILL` both leave the electrical interface idle; `SIGKILL`
merely leaves the inert module and overlay registered.

## Build

```sh
git clone https://github.com/qpernil/raspberry-pi-i2c-target.git
cd raspberry-pi-i2c-target
test -e "/lib/modules/$(uname -r)/build"
make -C kernel
```

This project intentionally does not distribute prebuilt kernel modules. The
Makefile uses `/lib/modules/$(uname -r)/build`, ensuring that the module is built
against the running target's headers. Rebuild after every kernel update.

Do not copy either overlay into the boot configuration. Run the responder as
root; it detects Pi 3 versus Pi 4, applies the matching runtime overlay, loads
the module, and opens the character device:

```sh
sudo ./prebuilt/aarch64/target-driver --ready-gpio 17  # Raspberry Pi OS ARM64
# or: sudo ./target/release/target-driver --ready-gpio 17  # locally built Rust executable
```

The default address is `0x13`. An alternative address and kernel artifact
directory can be supplied explicitly:

```sh
sudo ./target/release/target-driver --ready-gpio 17 0x24 ./kernel
```

Use receive-only mode for controllers that only write, such as an OLED display
driver. The BSC target ACKs and the application drains each record without
queueing an unused response:

```sh
sudo ./target/release/target-driver --receive-only 0x3c ./kernel
```

`--no-answer` is an equivalent alias.

A READY input is required for request/response controllers. GPIO numbers use
BCM numbering; GPIO17 is an example:

```sh
sudo ./target/release/target-driver --ready-gpio 17 0x24 ./kernel
```

Connect it to a pulled-up controller input. The SDA/SCL target pins and GPIO
controllers requiring sleeping operations cannot be used as READY.

The independent `virtual-display` application loads the same kernel target
itself, decodes SSD1306 or SH1106 streams into one canonical 128x64 monochrome
framebuffer, and renders it through SDL2. It does not invoke `target-driver`:

```sh
sudo -E ./target/release/virtual-display --display=ssd1306 0x3c ./kernel
sudo -E ./target/release/virtual-display --display=sh1106 \
  --button-outputs=5,26 0x3c ./kernel
```

Display mode defaults to address `0x3c`. Add `--vsync` to request synchronized
SDL presentation; it remains off by default and never enables intentional frame
skipping. A slow synchronized renderer instead uses the kernel receive ring as
its finite backlog.

Idle pull policy belongs to the overlay rather than the C driver. It defaults
to no pull and can be selected by the loading application:

```sh
sudo ./target/release/target-driver --ready-gpio 17 --idle-pull none
sudo ./target/release/target-driver --ready-gpio 17 --idle-pull down
sudo ./target/release/target-driver --ready-gpio 17 --idle-pull up
```

The equivalent overlay parameter is `idle_pull=0`, `1`, or `2`, respectively.
This setting is applied only after a device that was actually opened closes;
loading and unloading a never-opened instance preserves the pins as found.

Ctrl+C, SIGTERM, and ordinary application errors unload the module and remove
the overlay. The driver disables the peripheral and returns its pins to the
configured idle input state. Because no files or boot settings are installed,
a reboot also starts with the driver unloaded.

If the process is forcibly killed, Linux closes its descriptor and the kernel
driver idles the hardware. Ask the app to remove the remaining inert module and
overlay:

```sh
sudo ./target/release/target-driver --unload
```

This command is also safe when nothing is loaded. If another process has
`/dev/bsc-target0` open, module removal fails and the overlay is left in place.

Wired qualification at the configured 400 kHz rate covers long randomized
responses, delayed staged reads, and simultaneous exchanges to two targets
under CPU load. Retain CRC, timeouts, error counters, and controller retries
even when the kernel driver is used. The driver substantially reduces
scheduling risk by servicing FIFO thresholds in hard-IRQ context, but this
peripheral has no clock stretching or DMA, so a general-purpose Linux kernel
cannot provide a mathematical no-overrun guarantee.
