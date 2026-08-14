# Raspberry Pi I²C target

[![CI](https://github.com/qpernil/raspberry-pi-i2c-target/actions/workflows/ci.yml/badge.svg)](https://github.com/qpernil/raspberry-pi-i2c-target/actions/workflows/ci.yml)

An interrupt-driven Linux I²C target driver and dependency-free Rust tools for
Raspberry Pi. A Pi 3B/3B+ or Pi 4B acts as the target; a Pi 4 or Pi 5 running
Linux can act as the controller through the standard `/dev/i2c-1` interface.

The project contains two target implementations:

| Implementation | Transfer size | Timing model | Intended use |
| --- | ---: | --- | --- |
| Linux kernel driver | Up to 8192 bytes | FIFO interrupts plus a high-resolution tail timer | Long transfers and stress testing |
| Direct Rust `/dev/mem` target | 15-byte application payload | Every transaction fits in the 16-byte hardware FIFO | Small, dependency-free demonstration |

The kernel driver is the recommended path for long messages. It exposes
`/dev/bsc-target0`; the Rust responder owns its temporary overlay/module
lifecycle and never changes the boot configuration.

> **Hardware status:** module compilation, MMIO/IRQ discovery, pin multiplexing,
> character-device lifecycle, FIFO queuing, exclusive-open behavior, configurable
> idle pulls, cleanup, and `SIGKILL` final-close handling have been exercised on
> Pi 3 and Pi 4 hardware. Initial Pi 5-controller to Pi 3B+-target wired tests at
> the configured 400 kHz rate passed response sizes from 6 through 1029 bytes
> with no drops, overruns, underruns, or short reads. Sustained-load and complete
> signal-integrity qualification remain pending.

## Supported hardware

| Role | Models | Operating-system interface |
| --- | --- | --- |
| Target | Raspberry Pi 3B/3B+, Raspberry Pi 4B | BCM SPI/BSC target peripheral |
| Controller | Raspberry Pi 4B, Raspberry Pi 5 | Linux `i2c-dev` on `/dev/i2c-1` |

Pi 5 is supported as a controller, not as a target. The target peripheral has a
16-byte FIFO, no DMA, no clock stretching, and only supports 7-bit addresses.

## Wiring

Power down both boards before wiring.

| Signal | Controller | Pi 3 target | Pi 4 target |
| --- | --- | --- | --- |
| SDA | GPIO2, physical pin 3 | GPIO18, pin 12 | GPIO10, pin 19 |
| SCL | GPIO3, physical pin 5 | GPIO19, pin 35 | GPIO11, pin 23 |
| Ground | pin 6 | pin 6 | pin 6 |

Do **not** connect the boards' 3.3 V or 5 V power pins to each other. Both use
3.3 V signalling. GPIO2 and GPIO3 on a standard controller board already have
physical I²C pull-ups, so do not add another set initially.

Target-pin conflicts:

- Pi 3 GPIO18/19 must not simultaneously be assigned to PCM/I²S, PWM, or a
  conflicting SPI overlay.
- Pi 4 GPIO10/11 are shared with SPI0 and must not simultaneously be assigned
  to that peripheral.

## Architecture

```text
controller-long (Rust)                     target-driver (Rust)
          │                                          │
          ▼                                          ▼
    /dev/i2c-1                                /dev/bsc-target0
          │                                          │
 Linux controller driver                 bcm27xx_bsc_target.ko
          │                                          │
          └──────── SDA / SCL / GND ─────────────────┘
```

The controller uses the normal in-kernel Raspberry Pi I²C controller driver.
The target uses the separate BCM SPI/BSC target peripheral. The target driver
services FIFO thresholds in hard-IRQ context; a 100 µs high-resolution timer,
active only while the character device is open, catches short tails and STOP
completion.

See [Architecture and lifecycle](docs/architecture.md) for the driver boundary,
pin states, transactions, and limitations.

## Controller setup

Enable `/dev/i2c-1` on the controller. On Ubuntu, add this beneath `[all]` in
`/boot/firmware/config.txt`:

```ini
dtparam=i2c_arm=on,i2c_arm_baudrate=400000
```

Reboot and verify:

```sh
ls -l /dev/i2c-1
sudo usermod -aG i2c "$USER"
```

Start a new login session after changing group membership. A quick controller
probe is:

```sh
i2cdetect -y 1
```

The target appears only while its character device is open.

## Build

The Rust tools have no Cargo dependencies:

```sh
cargo build --release --locked
```

Building the target module requires a compiler, `make`, `dtc`, and headers that
match the running target kernel:

```sh
test -e "/lib/modules/$(uname -r)/build"
make -C kernel
```

Rebuild the module after every target-kernel update.

### Versioned prebuilt target module

`prebuilt/aarch64/6.18.39+rpt-rpi-v8/` contains a module built on a Raspberry
Pi 4 for the exact Raspberry Pi OS ARM64 kernel named by the directory. Kernel
modules are tied to their kernel release/configuration, so verify an exact
match before using it:

```sh
test "$(uname -r)" = "6.18.39+rpt-rpi-v8"
sudo ./prebuilt/aarch64/target-driver \
  0x13 \
  ./prebuilt/aarch64/6.18.39+rpt-rpi-v8
```

If the test fails, build `kernel/` against headers matching the target instead
of loading this prebuilt module. Checksums and build details accompany the
artifacts in the versioned directory.

## Run the kernel target

Start the target first:

```sh
sudo ./target/release/target-driver
```

The default address is `0x13`. Override the address and, when necessary, the
directory containing the `.ko` and `.dtbo` artifacts:

```sh
sudo ./target/release/target-driver 0x24 ./kernel
```

The Rust application detects Pi 3 versus Pi 4, applies the matching runtime
overlay, loads the module, opens `/dev/bsc-target0`, and removes the module and
overlay on ordinary exit.

### Idle pin policy

Loading but not opening the device leaves the existing GPIO configuration
untouched. Opening selects ALT3 with no internal pulls. Final close selects an
input state whose pull policy comes from the overlay/application:

```sh
sudo ./target/release/target-driver --idle-pull none  # default
sudo ./target/release/target-driver --idle-pull down
sudo ./target/release/target-driver --idle-pull up
```

The C driver contains no hardcoded idle-pull policy. The equivalent direct
overlay parameters are `idle_pull=0`, `1`, and `2`.

### Forced termination and stale cleanup

Linux closes a process's descriptors after `SIGKILL`. The driver's final-close
handler stops its timer, disables BSC and interrupts, clears queued I/O, and
applies the configured idle state. The module and overlay remain registered but
inert because userspace cleanup could not run.

Remove an inert or manually loaded instance with:

```sh
sudo ./target/release/target-driver --unload
```

The command is harmless when nothing is loaded. It refuses if another process
still has the device open. Only one independent application may open the target
device at a time; additional opens return `EBUSY`.

## Send a long transfer

With the target responder running, invoke this on the controller:

```sh
./target/release/controller-long \
  "a message longer than the sixteen byte hardware FIFO" \
  0x13 \
  /dev/i2c-1
```

The controller writes one message, waits 20 ms for userspace to queue a reply,
then reads `ACK: ` followed by the original message. Requests may contain up to
8187 bytes so the prefixed response remains within the driver's 8192-byte limit.

## Direct userspace demonstration

The original Rust-only target maps the hardware through `/dev/mem`. It avoids
active-transfer scheduling risk by keeping each framed transaction within the
16-byte FIFO.

Target:

```sh
sudo ./target/release/target 0x13
```

Controller:

```sh
./target/release/controller "hello Pi 3" 0x13 /dev/i2c-1
```

This mode permits commands and replies of at most 15 payload bytes. Do not run
it at the same time as the kernel target.

## Diagnostics

Inspect the device, IRQ, GPIO state, and cumulative driver counters:

```sh
ls -l /dev/bsc-target0
grep bsc-target /proc/interrupts
cat /sys/bus/platform/drivers/bcm27xx-bsc-target/*/stats

# Pi 3 target
pinctrl get 18-19

# Pi 4 target
pinctrl get 10-11
```

Important counters include receive overruns/drops, transmit underruns/short
reads, hardware interrupts, and timer callbacks. `timer_runs` remains unchanged
while the character device is closed.

## Limitations

- The target hardware cannot stretch SCL and has no DMA. Hard-IRQ servicing
  greatly reduces scheduling risk but cannot create a mathematical no-overrun
  guarantee on a general-purpose Linux kernel.
- A response must be queued before the controller starts reading. The example
  protocol therefore uses separate write and read transactions with a processing
  gap; a repeated START cannot wait for userspace to generate a response.
- This is a single-controller, single-responder demonstration. Production use
  should add framing, lengths, checksums/CRC, sequence numbers, timeouts, retries,
  and idempotency.
- Initial wired functional validation at the configured 400 kHz rate has
  passed. The 100 kHz matrix, sustained load, and full signal-integrity work in
  the [hardware validation plan](docs/hardware-test-plan.md) remain pending.

## Troubleshooting

- `/dev/i2c-1: Permission denied`: add the controller user to the `i2c` group
  and start a new login session.
- `Remote I/O error`: the target did not acknowledge. Check that the responder
  has the device open, verify the address and model-specific target pins, and
  confirm common ground.
- `target module is already loaded`: run `target-driver --unload`; it will refuse
  if an active process owns the device.
- Missing `.ko` or `.dtbo`: run `make -C kernel` on the target with matching
  kernel headers installed.
- After a kernel update: rebuild before attempting `insmod`; modules are tied to
  the kernel version/configuration against which they were compiled.

## Documentation

- [Kernel driver interface and build details](kernel/README.md)
- [Architecture and lifecycle](docs/architecture.md)
- [Hardware and oscilloscope validation plan](docs/hardware-test-plan.md)

## License

The Rust applications and documentation are licensed under either the
[MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your
option. The kernel module and overlays are licensed under GPL-2.0-only; the UAPI
header carries GPL-2.0 WITH Linux-syscall-note. Individual source-file SPDX
identifiers are authoritative.
