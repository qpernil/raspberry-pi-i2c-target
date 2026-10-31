# Hardware and oscilloscope validation

Hostnames and device/account identifiers below are illustrative; replace them with your own configured values.

This plan qualifies the current Raspberry Pi BSC target driver as a long-lived
character-device endpoint, a receive-only display target, and a queued-response
target with required active-low READY signaling.

## Shared-HSM qualification limit

Concurrent full HSM/PQC operation on a shared BSC bus remains unqualified.
The finite echo and vendor checks below do not establish reliability for that
workload. The RX completion check addresses a final-byte race; earlier TX
refill improves interrupt-latency tolerance. Neither independently resolves the
remaining NACK, arbitration-loss and MAC-verification failures. See the
[controller qualification record](https://github.com/qpernil/pkcs11rs/blob/master/docs/i2c-pqc-qualification.json)
for the tested configurations and limits.

## Verified behavior

On build-host with the Pi 3B+ targets `i2c-target-1` and `i2c-target-2`
(1 GiB RAM each), driver ABI 3 passes 1,000 concurrent
randomized exchanges per target, with delayed header/body reads and every tenth
response deliberately left one byte short. The run includes bounded four-core
load; neither target reports hardware overruns or underruns.

All 31 supported vendor YubiHSM cases pass on each target while the peer serves
continuous byte-exact echo traffic. A two-second injected worker-write delay
also verifies that two newer requests replace the pending slot and suppress
the old result: the counters record one pending replacement and one discarded
worker response. The C model covers the same ownership and FIFO transitions.

The Linux controller arms rising-only GPIO detection for cleanup, then falling-only
for the response. This avoids the deferred both-edge handler classifying an
interrupt using a pin level that has already changed again. No post-read
completion inference, fallback stream, guard byte or drain delay is used.

The receive-only display parser and launcher tests cover partial updates,
merged records, and arbitrary read splits. Producer/consumer qualification in
`display-backends` also checks exact pixel equivalence with native SH1106
command execution and verifies one-byte pixel changes remain one-byte payloads.
Linux tests run the actual SDL render path with `SDL_VIDEODRIVER=dummy`, including
bounded rendering under continuous partial updates. The C driver model checks
ordered receive-only backlog retention and merged writes without READY resets.
On ARM64 Linux, isolated SDL pixel readback matches the expected 1,024-byte
framebuffer for the continuous-partial-update regression. On the real bus,
build-host drives the Pi 3B+ target `i2c-target-2` at address `0x3c`. Its ordinary
graphical viewer passes both labelled controller runs:

| Controller | Application updates | Sent/received bytes | Decoded updates | Overruns/drops/discarded bytes |
| --- | --- | --- | --- | --- |
| SH1106 | 201 | 2,708 | 208 (eight initial pages plus 200 partial writes) | 0 / 0 / 0 |
| SSD1306 | 201 | 2,858 | 201 | 0 / 0 / 0 |

Five consecutive runs of each controller also pass with the ordinary viewer:
SH1106 receives all 13,540 bytes and decodes 1,040 updates; SSD1306 receives all
14,290 bytes and decodes 1,005 updates. Both record zero overruns, drops, or
parser-discarded bytes. These are finite transport checks, not an exhaustive
proof of every rendered pixel or of unbounded IRQ latency.

The last 200 updates in each run contain one pixel byte. The labelled picture is
visually confirmed, and its identical saved 1,024-byte framebuffer is also
displayed through gadget-host's ST7789 SPI backend, aspect-fitted and centred.
This compares framebuffer layout and orientation; it does not qualify native
SH1106 hardware's I2C command interpretation.

These real-bus runs use the ordinary viewer without GPU readback
instrumentation. Synchronous GPU readback changes the workload and can cause
FIFO overruns on this non-clock-stretching target; use it only for isolated
pixel diagnostics. HSM and Virtual YubiKey services are restored after the
display tests. Electrical margin and unbounded interrupt
latency remain outside the finite lab qualification.

## Equipment

- One Raspberry Pi controller with `/dev/i2c-1` enabled
- One or two Pi 3B/3B+ or Pi 4B targets
- SDA, SCL, and ground connections for each target
- For response mode, one wire per target from READY to a controller GPIO input
- An oscilloscope with two compensated 10× probes

## Safety and wiring

1. Power down the boards before changing wiring.
2. Connect SDA, SCL, and ground according to the root README.
3. Do not connect the boards' 3.3 V or 5 V rails.
4. Confirm that no SPI, PCM/I²S, or PWM overlay owns the target pins.
5. Attach oscilloscope probe grounds to circuit ground and use high-impedance
   10× inputs with short ground leads.

READY is open-drain. Configure a pull-up on the controller input and never drive
the line high from either endpoint.

## Lifecycle and READY

Use ABI 3 on both ends, with a separate READY input per target.

1. Check exclusive open, activation, final close and unload.
2. Capture the request-cleanup acknowledgment edge before the response edge,
   including requests sent while READY is already inactive.
3. Read three-byte headers separately from payloads, including a one-byte
   payload and long pauses. READY remains asserted after complete reads.
4. Abandon a payload's final byte, send another request, and verify that the
   new response contains no stale prefix.
5. Send replacement requests during a slow operation. Only the newest pending
   request may receive a response; the previous operation can retain side effects.
6. Restart the controller while a response is pending and while computation is
   ongoing. It must resynchronize without replaying the uncertain command.

## Functional transfers

Exercise lengths around hardware and protocol boundaries at 100 kHz, then at
the intended 400 kHz rate:

```text
1, 2, 7, 8, 15, 16, 17, 31, 32, 255, 256, 1024, 4096, 8187 bytes
```

For every size, compare the complete response, record controller I/O errors,
and confirm that the receive overrun/drop and transmit underrun
counters do not increase.

For receive-only qualification, send SSD1306 or SH1106 initialization and
framebuffer traffic repeatedly. Adjacent electrical writes may be merged into
one character-device record, so the consumer must parse a byte stream rather
than assume one `read()` per controller `write()`.

## Virtual display qualification

For a visible real-bus check, use a connected Pi 3B/3B+ or Pi 4B target at
`0x3c`. Run `virtual-display --no-button-outputs` from its graphical login;
no READY wire is required. If an I2C HSM service owns the BSC target, stop that
service for the display test and restore it afterward. Only one target on the
shared bus may use the display address at a time.

On the controller, run `display-backends`' ignored
`physical_i2c_display_updates` test with the qualification feature, explicitly
setting `DISPLAY_TEST_BUS`, `DISPLAY_TEST_CONTROLLER`, and
`DISPLAY_TEST_EXPECTED`. Test both SSD1306 and SH1106. Require all 201
application updates, matching sent/received byte counts, zero parser-discarded
bytes, and no receive overrun/drop increase. The last 200 updates retain
one-byte pixel payloads. The expected picture is a border, `I2C TEST` heading,
checkerboard, cross, controller label, and four final dotted footer markers.
Stop the viewer normally and confirm its module/overlay and descriptor are
released before restoring the HSM service.

The saved native framebuffer can also be sent to a physical SPI display using
`display_backends_write_frame` with `Mono1MsbReversePage`, width 128, height 64,
and stride 128. ST7789 aspect-fits and centres it. Pause any worker owning
that display and restore it after the comparison.

`tests/sdl_capture.c` is an optional GPU-readback diagnostic. Build it as a
shared library with SDL2 and `-ldl`; use `SDL_VIDEODRIVER=dummy`, `LD_PRELOAD`,
and `VIRTUAL_DISPLAY_CAPTURE` with the isolated SDL render regression to
compare converted pixels with its known framebuffer. Keep this shim disabled
during the baseline real-bus test: its synchronous GPU reads add work absent
from the ordinary viewer and can perturb FIFO servicing. It is test-only and
is not installed into production.

## READY signal isolation

The Pi 5 / two Pi 3B+ bench reproduces corruption and request NACKs when only
an idle peer's READY output changes level. Its BSC RX/TX transaction counters
stay unchanged. Level-only toggling also fails with that peer's BSC service
stopped, the device removed, and SDA/SCL configured as inputs. New FIFO
publication is therefore unnecessary for this reproduction.

CPU/readback controls, unchanged function-select rewrites, and direction changes
that keep READY logically high pass the finite echo controls. Level-only
transitions with a fixed output direction fail. These observations distinguish
the READY signal transition from FIFO loading or pin-direction changes; they
do not establish the physical coupling route or prove that every historical
PQC failure has this cause. The controller READY input is unclaimed in this
experiment, and a separate 100-cycle idle-bus control produces no GPIO IRQ
increments. The direction-toggle test emulates open-drain release/high and
output/low. The fixed-direction level test actively drives both levels; it is
a push-pull diagnostic, so its BSC-disabled result alone does not prove the
same electrical mechanism as the original open-drain edge.

The connected/disconnected/reconnected comparison implicates the connected
READY path. The fixed-output high/low control fails after eight successful
echoes with the lead connected. Disconnecting only that lead at controller
GPIO22 passes all 500 echoes (1,449,901 payload bytes over 7,153 GPIO cycles);
reconnecting it restores a request-write `EREMOTEIO` after 67 successful echoes
and 674 GPIO cycles. Electrical checks verify the disconnection and reconnection.
The reconnected failing request waits only 11 microseconds for the bus lock.
The peer's BSC transaction and byte counters remain zero throughout these two
controls, with its original service running but idle.

Follow-up controls retain the same GPIO17 high/low pulse workload:

| READY path / controller input | Successful echoes before failure | Failure |
|---|---:|---|
| Breadboard, GPIO22, C3 tip disconnected | 85 | Request-write `EAGAIN`, lost arbitration |
| Direct jumper, GPIO22, C3 tip disconnected | 13 | Response-payload `EAGAIN`, lost arbitration |
| Direct jumper, GPIO24, C3 tip disconnected | 43 | Request-write `EREMOTEIO` |
| Direct jumper, GPIO24, repeated for scope | 11 | Full-length 3,136-byte reply differs at byte 915 |

A 100-echo baseline without pulses passes after bus recovery and again with
GPIO24 connected. The idle peer's transaction/byte counters remain zero.
GPIO22 specifically, READY breadboard contacts and C3's tip are therefore not
required for this finite reproduction. Other scope/ground paths, wire loading,
shared controller effects and target output/ground coupling remain unresolved.
The I2C diagnostic sources and loaded target module match the earlier controls;
other dependency/binary changes limit direct comparisons across the lab reboot.

A byte-correlated capture uses hardware single acquisition, triggered on an
I2C write to address `0x24` with first data byte `0x01` (echo). Each arm sends
one 3,133-byte echo, preventing later device-info traffic from replacing the
record. The clean capture matches the complete saved request and response body.
With READY pulsing, five echoes pass and the sixth fails with request-write
`EAGAIN` and kernel lost arbitration. Its scope record matches the first 2,604
saved request bytes. The next byte (zero-based request offset 2,604) should be
`0x9D`; the wire presents eight data bits `0x9C`, then clocks stop before ACK.
SDA remains low while SCL stays high. A native 20 ns excerpt preserves that bit.

This establishes the actual on-wire failure, but not which device holds SDA
low or why. C3 remains disconnected, so no READY-edge timing is available;
receiver thresholds and probe skew are uncalibrated. Recovery with low/release
clocks and STOP restores idle-high lines; four no-pulse echoes then pass.
The failed acquisition remains stopped and no uncertain HSM mutation is replayed.

A post-abort output-isolation control identifies the addressed target as the
held-low source. After a 4,668-byte echo write loses arbitration, disconnecting
only idle Pi2's GPIO18 SDA output (input mode) leaves SDA low. Restoring Pi2
ALT3 leaves it low. Disconnecting only addressed Pi1's SDA output raises SDA;
restoring Pi1 ALT3 pulls it low again. No clocks, READY configuration changes,
service restarts or physical rewiring occur during those checks. Pi1 then
reports `FR=0x32` (`RXBUSY=1`, `TXBUSY=0`); idle Pi2 reports `FR=0x12`.

The BSC hardware generates receiver ACKs, so receive-busy does not imply a
high-impedance SDA output. These observations fit a receive/ACK state waiting for another clock.
They identify the post-abort holder, not the cause of the original bad bit or
whether a driver transition or hardware misalignment initiated it. Recovery
clocks and STOP restore the bus; four subsequent echoes pass and both flag
registers return to `0x12`. Both SDA muxes are restored; original services
remain running.

The separate NACK capture's roughly 23 microsecond READY pulse is the intended
cleanup acknowledgement. Its first cleanup rise overlaps the following
`0x06` device-info write by about 40 microseconds after that write starts.
The target logs record the partial echo and complete device-info request;
this recovery overlap remains distinct from the original arbitration loss.

Diagnostic wiring currently connects Pi2 GPIO17 (pin 11) directly to controller
GPIO24 (pin 18), with C3 disconnected. The documented normal bench mapping is GPIO22;
no production configuration was changed. Any subsequent two-target diagnostic must use
GPIO24 explicitly or restore the physical GPIO22 connection first. Diagnostic
jobs are stopped. The next measurement should trace receive completion and
BSC reconfiguration against wire timing to distinguish driver transitions from
hardware loss of alignment.

See the [SDA output isolation](sda-output-isolation.json),
[correlated failure capture](ready-failure-capture.json) and
[isolation record](ready-signal-isolation.json) for counts, source revisions
and measurement limits. Concurrent authenticated PQC remains unqualified.

## Shared-bus concurrency

Run two target addresses on the same physical bus. The controller must hold one
advisory lock on the bus device through request cleanup acknowledgment and separately through the complete
response read, releasing it during computation. Run simultaneous clients
with randomized payloads and delayed header/body reads while loading all target
CPU cores. Any byte mismatch, extra receive record, I/O error, or driver error
counter increment is a failure.

A negative test with the controller lock deliberately disabled may reproduce
cross-address interference, but it must be isolated from real HSM state and is
not a supported deployment mode.

## 400 kHz signal integrity

Record the physical SCL frequency, SDA/SCL high and low levels, rise and fall
times, ringing, and visible setup/hold margins. Measure SCL rising edge to rising
edge inside an uninterrupted clock burst: the expected periods are 10 µs at
100 kHz and 2.5 µs at 400 kHz.

Treat the configured adapter rate and measured SCL rate separately. On a Pi 4
controller, dynamic reduction of the core clock can make a divider configured
for 400 kHz produce approximately 160 kHz physically. `core_freq_min=500` under
`[all]` keeps the relevant parent clock at 500 MHz on that controller, at the
cost of modestly higher idle power; this is a controller workaround, not a
target-driver requirement.

Do not add target-side pull-ups unless the measured rise time requires them.
Calculate the combined resistance before adding any additional pull-up.

## Sustained load

Monitor counters during randomized transfers with CPU, storage, and network
activity on the target:

```sh
cat /sys/bus/platform/drivers/bcm27xx-bsc-target/*/stats
```

Use sequence numbers and checksums, cover millions of bytes, and capture a
triggered scope trace for any mismatch or error-counter change.

## Evidence to retain

- Board models, revisions, and kernel versions
- Overlay address, idle pull, READY GPIO, and timer interval
- Configured and physically measured controller bus rate
- Cable length, pull-up arrangement, and probe configuration
- Driver counters before and after each run
- Representative waveforms and decoded transactions
- Payload count, byte count, mismatches, retries, and elapsed time
