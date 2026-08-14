# BCM27xx BSC target kernel driver

This experimental out-of-tree driver services the BCM2835-family SPI/BSC I2C
target FIFO from interrupt context. It supports Raspberry Pi 3B/3B+ and Pi 4B.

The hardware has a 16-byte FIFO, no DMA, no clock stretching, and only 7-bit
target addresses. The driver combines FIFO threshold interrupts with a 100 us
high-resolution timer. The timer drains sub-threshold receive tails, detects
receive completion, and releases fully loaded responses when the transmit FIFO
becomes empty.

## Character-device interface

The overlay and loaded module create `/dev/bsc-target0` at address `0x13` by
default. They leave the BSC peripheral disabled and its timer stopped, and do
not alter the existing GPIO configuration until an application opens the
character device.

- Each `read()` returns one complete controller-to-target I2C transaction.
- Each `write()` queues one complete response for the next controller read.
- A response must be queued before the controller starts reading because the
  peripheral cannot stretch SCL.
- The maximum transaction is 8192 bytes.
- `poll()` reports readable requests and an available response slot.
- The ioctl ABI in `bsc_target_uapi.h` reports configuration and statistics.
- A text statistics snapshot is exposed as the platform device's `stats`
  sysfs attribute.

The interface intentionally permits one open file at a time. A controller must
use separate write and read transactions with a processing gap; a repeated
START cannot wait for userspace to create a response.

Opening the device selects the target pins, enables the peripheral and starts
the high-resolution timer. The final close reverses those actions and clears
queued I/O, selecting the externally configured idle input state. Consequently,
normal exit and `SIGKILL` both leave the electrical interface idle; `SIGKILL`
merely leaves the inert module and overlay registered.

## Build

```sh
cd ~/raspberry-i2c/kernel
make
cd ..
cargo build --release --bin target-driver
```

Do not copy either overlay into the boot configuration. Run the responder as
root; it detects Pi 3 versus Pi 4, applies the matching runtime overlay, loads
the module, and opens the character device:

```sh
sudo ./target/release/target-driver
```

The default address is `0x13`. An alternative address and kernel artifact
directory can be supplied explicitly:

```sh
sudo ./target/release/target-driver 0x24 ./kernel
```

Idle pull policy belongs to the overlay rather than the C driver. It defaults
to no pull and can be selected by the loading application:

```sh
sudo ./target/release/target-driver --idle-pull none
sudo ./target/release/target-driver --idle-pull down
sudo ./target/release/target-driver --idle-pull up
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

Initial wired tests at 400 kHz have passed, including 1024-byte requests, but
sustained-load qualification remains pending. Retain CRC, timeouts, error
counters, and controller retries even when the kernel driver is used. The
driver substantially reduces scheduling risk by servicing FIFO thresholds in
hard-IRQ context, but this peripheral has no clock stretching or DMA, so a
general-purpose Linux kernel cannot provide a mathematical no-overrun guarantee.
