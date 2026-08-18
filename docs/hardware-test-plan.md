# Hardware and oscilloscope validation plan

## Initial wired result — 2026-08-14

A Raspberry Pi 5 controller running Ubuntu communicated through `/dev/i2c-1`
with a Raspberry Pi 3B+ target running kernel `6.18.39+rpt-rpi-v8`. The
controller was configured for 400 kHz. A Siglent SDS824X HD with compensated
10× probes and a 20 MHz bandwidth limit decoded the bus during diagnosis.

The verified responder passed application payload sizes 1, 10, 11, 12, 64,
and 1024 bytes. Their `ACK: ` responses were 6, 15, 16, 17, 69, and 1029 bytes,
covering both sides of the 16-byte hardware FIFO boundary and repeated FIFO
refills. Final counters after that sequence were:

```text
rx_transactions=6 rx_bytes=1122 rx_overruns=0 rx_dropped=0
tx_transactions=6 tx_bytes=1152 tx_underruns=0 tx_short_reads=0
```

## Virtual Trezor receive-only result — 2026-08-18

A Raspberry Pi 4 Virtual Trezor controller was tested against two Pi 3 targets
on the same physical bus, activated one at a time at address `0x3c`. The scope
measured 400 kHz SCL. Each target received 135,218 bytes of SSD1306-compatible
traffic with `rx_overruns=0`, `rx_dropped=0`, and no queued or transmitted
responses. The byte total comprised a 26-byte initialization transaction and
131 pairs of 7-byte address-window plus 1,025-byte framebuffer writes.

The targets exposed 210 and 225 userspace records for that identical byte
total. Back-to-back controller writes may therefore be combined when the BSC
completion timer does not observe their short idle gap. Protocol consumers
must parse the byte stream without assuming one character-device `read()` per
controller `write()`.

This is an initial functional result, not completion of the qualification plan
below. The 100 kHz matrix, electrical measurements, randomized sustained load,
and deliberate CPU/storage/network pressure remain to be run.

## Equipment

- One Raspberry Pi controller with `/dev/i2c-1` enabled
- One Pi 3B/3B+ or Pi 4B target
- Three jumper wires: SDA, SCL, and ground
- Siglent SDS824X HD oscilloscope
- Two passive probes with short ground connections

## Safety and wiring check

1. Power down both boards.
2. Connect only SDA, SCL, and ground according to the root README.
3. Do not connect 3.3 V or 5 V between boards.
4. Confirm that no conflicting SPI, PCM/I²S, or PWM overlay owns the target pins.
5. Attach oscilloscope probe grounds to circuit ground, not to SDA or SCL.
6. Use 10× probe attenuation and high-impedance inputs.

The scope probes add capacitance, so use the shortest practical ground spring or
lead. Start with one probe on SCL and one on SDA.

## Phase 1: idle and lifecycle

1. Leave the target app stopped and verify that the controller pull-ups hold SDA
   and SCL high.
2. Load the module without opening the character device and verify that it does
   not acknowledge address `0x13`, change the timer count, or change pins.
3. Start `target-driver` and confirm that the target acknowledges only while the
   device is open.
4. Stop with Ctrl+C and verify input/idle pull policy.
5. Repeat using `SIGKILL`; verify immediate electrical idle, then remove the inert
   module/overlay with `target-driver --unload`.

## Phase 2: 100 kHz functional transfers

Start conservatively at 100 kHz. Exercise lengths around important boundaries:

```text
1, 2, 7, 8, 15, 16, 17, 31, 32, 255, 256, 1024, 4096, 8187 bytes
```

For every size:

- compare the complete echoed response;
- check for NACKs or Linux I/O errors;
- inspect receive overrun/drop and transmit underrun/short-read counters;
- capture address, ACK, data, STOP, and the controller's processing gap.

## Phase 3: 400 kHz signal integrity

Repeat at the configured 400 kHz controller rate. Record:

- measured SCL frequency;
- SDA/SCL low and high voltage;
- rise and fall times;
- ringing, overshoot, or undershoot;
- setup/hold margins visible to the scope decoder;
- whether adding the second probe materially changes rise time.

Do not add target-side pull-ups unless measured rise time requires them. If extra
pull-up strength is needed, calculate the combined resistance rather than adding
an arbitrary second pair.

### Verify the physical controller clock

Treat the requested adapter rate and the physical SCL rate as separate
measurements. Measure rising edge to rising edge within an uninterrupted clock
burst; do not use SDA transition frequency or average across transaction gaps.
Expected SCL periods are 10 µs at 100 kHz and 2.5 µs at 400 kHz.

A Pi 4 controller test found a controller-side clock mismatch independent of
the target and this target driver. With
`i2c_arm_baudrate=400000`, Linux reported a 400 kHz I²C clock and programmed
the BSC divider to 1250 (`0x4e2`) based on a 500 MHz parent clock. Raspberry Pi
firmware nevertheless reduced the physical core clock to its 200 MHz idle
minimum. The oscilloscope measured 6.25 µs rising edge to rising edge:

```text
500 MHz / 1250 = 400 kHz  (rate assumed when programming the divider)
200 MHz / 1250 = 160 kHz  (physical idle SCL rate)
```

On that controller, setting `core_freq_min=500` under `[all]` in
`/boot/firmware/config.txt` kept the parent clock at 500 MHz and restored the
physical 400 kHz rate. This controller-specific workaround modestly increases
idle power; it is not a target-driver requirement. Useful Pi 4 diagnostics are:

```sh
vcgencmd measure_clock core
sudo grep -E 'fe804000.i2c|i2c_div' /sys/kernel/debug/clk/clk_summary
sudo devmem 0xfe804014 32
```

The final command reads the Pi 4 BSC1 divider register and is not portable to
other controller models. Because the target hardware used here cannot stretch
SCL, a stable discrepancy between the requested and measured rates should be
investigated on the controller before attributing it to the target driver.

## Phase 4: sustained load

Run repeated transfers across small and large sizes while monitoring:

```sh
cat /sys/bus/platform/drivers/bcm27xx-bsc-target/*/stats
```

Include CPU and storage/network activity on the target to create interrupt
latency pressure. A useful qualification run should include millions of bytes,
random payloads, randomized lengths, controller retries, and sequence/CRC
checking. Any change in overrun, drop, underrun, or mismatch counters is a test
failure worth capturing with a triggered scope trace.

## Evidence to retain

- Board models and revisions
- Kernel versions
- Overlay address, idle pull, and timer interval
- Controller bus rate
- Cable length and pull-up arrangement
- Scope probe mode and measured capacitance, if known
- Driver counters before and after each run
- Representative waveform screenshots and decoded transactions
- Payload count, byte count, mismatches, retries, and elapsed time
