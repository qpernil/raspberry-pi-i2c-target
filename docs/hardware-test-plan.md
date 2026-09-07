# Hardware and oscilloscope validation

This plan qualifies the current Raspberry Pi BSC target driver as a long-lived
character-device endpoint, a receive-only display target, and a queued-response
target with required active-low READY signaling.

## Verified behavior

On ubuntu4 with two Pi 3B+ targets, driver ABI 3 passes 1,000 concurrent
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

The receive-only display parser and launcher tests pass; display hardware has
not been rerun with this revision. Electrical margin and unbounded interrupt
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
