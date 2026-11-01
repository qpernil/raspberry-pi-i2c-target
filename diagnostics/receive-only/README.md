# Configure-once receive-only BSC diagnostic

Hostnames and device/account identifiers below are illustrative; replace them with your own configured values.

This optional fixture isolates controller writes from the response/READY paths of
the production target driver. It is a separate module and never replaces the
installed production module. Both bind the same device-tree compatible and must
not be loaded together. Stop controller traffic and the Pi1 target profile before
using it. Keep HSM state directories and installed profiles unchanged.

The module programs address, FIFO threshold, interrupt mask and control once on
probe. `CR=0x205` enables I2C and receive only; transmit is disabled. IRQ and
300 microsecond timer service only drain RX bytes, acknowledge interrupts, and
record errors. They do not finish requests, publish replies, reset the peripheral,
change its address or operate READY. All bytes are retained in an append-only
32 MiB capture, exposed read-only as `/dev/bsc-rx-minimal`; `stats` reports byte
counts, overflow/drop counters and non-destructive register snapshots. Close all
capture descriptors before unloading. Configuration is disabled on module removal.

## Build and deployment

Use the canonical Mac repository for edits and Git publication. Pull the published
branch through HTTPS on lab hosts. Build the C module on Pi1 against its running
kernel and the C controller on build-host; neither requires a Rust build on the Pis.

```sh
make -C diagnostics/receive-only modules     # target Pi1
make -C diagnostics/receive-only controller  # controller build-host
```

Use the existing Pi3 overlay with address `0x24` and **without** `ready_gpio`.
Load `bsc_rx_minimal.ko` instead of the production module. Baseline writes must
arrive byte-for-byte with zero overruns, underruns and capture drops before a
pulse result can be interpreted. Repeat the same vector sequence and cadence
with only the Pi2 GPIO17 low/release pulse loop added for a production-style
comparison. A fixed-direction high/low loop is a separate push-pull stress control.
Pi2 receives no I2C commands.
A passing finite run does not establish that every response path is safe.

The [bench closeout](../../docs/bench-validation-2026-11-01.md) separates these
controls from production low/release signaling, records the interrupted matrix,
and specifies the restored, untuned production state. Do not resume pad tuning
as part of ordinary deployment or service startup.

## Controller and error capture

`generate-vectors.py` reproduces the seed-91 echo-stress request sequence. Its
13th request is 4096 bytes and matches the saved failing request SHA-256
`e1996f7d4b243030f9a0e9c080940e7a45e74296f4e8cc1a71251d0fad3337d9`.
Each vector is a four-byte little-endian length followed by unchanged application
bytes. The controller holds an exclusive adapter lock, disables adapter retries,
issues writes only, and never retries a failed or short transfer. Inter-write
spacing defaults to 1000 us plus 23 us per request byte, approximating the omitted
reply's wire time; it is not an identical HSM request/response workload.

The error marker is an open-drain output on controller GPIO23. For this diagnostic
only, release Pi1 GPIO17 as an input and reuse the existing READY1 lead and C4
probe. Verify high/released and low states before traffic. Thus no physical wire
move is required when the existing mapping is confirmed. GPIO23 stays released
until `write()` reports failure, then latches low before logging or another bus
operation. A gate file permits arming the scope before the first write. The
controller holds its descriptors after writing `result.json` until terminated.

Verify a marker-only run triggers a hardware single C4 falling-edge acquisition.
Use pre-trigger history with C1=SCL, C2=SDA, C3=READY2 and C4=error marker. Save and
correlate bytes and the final clock/data edges before calling a capture a failure
waveform. A delayed timeout can outlast pre-trigger memory and must be reported
as such. A steady successful run produces no error edge; stop it explicitly.

After evidence capture, stop controller and pulse units before bus recovery.
Clear a stuck bus using low-drive/release SCL clocks and STOP, restore controller
GPIO2/3 to I2C, unload the diagnostic/overlay, return GPIO23 to pulled-up input,
then restart the original Pi1 profile. Verify the production module/worker hashes,
active services, clean checkouts and idle-high SDA/SCL. Preserve stored keys.

## Verified result and interpretation

The [validation record](validation.json) contains the finite bench result. The
100-write baseline delivers all 142,975 bytes exactly. READY2 pulses reproduce
controller arbitration loss on write 119, after 118 successful writes. The failing
request is 2,444 bytes; the first corrupted byte is offset 952 (zero-based), not a
4,096-byte boundary. There are no RX overruns, underruns or capture drops.

That recorded stimulus actively drives READY2 high and low (push-pull); it
is an electrical stress control, not the production open-drain waveform.
The first 952 bytes match. Byte 952 should be `0xf3` (`11110011`); the nominal
wire data bits are `11110010` (`0xf2`), and Pi1's RX FIFO delivers `0xf9`
(`11111001`). Inserting an extra high sample early in the intended byte produces
that received value. An early receiver ACK explains the last data bit being low,
followed by arbitration loss, no ACK clock/STOP, and `FR=0x32`. This is a working
bit-alignment model, rather than proof of a calibrated receiver threshold.

A marker-triggered hardware single record captures the request start, wrong bit
and stuck bus. Native samples are 2 ns apart. READY2 falls during the second SCL
rising edge of the affected byte, where SCL becomes non-monotonic and crosses
illustrative thresholds more than once. Simultaneous excursions on SDA and the
released marker lead require local-ground/probe and coupling controls before the
physical cause can be assigned. C4 is the error marker in this record; it is not
a target READY transition. A post-abort SDA low-level shift is also consistent
with a different device pulling low.

The response publication path, TX enable, HSM execution, Pi1 READY operation and
per-request BSC control changes are unnecessary for this reproduction. The
production driver and HSM workers are restored after the capture; the next
physical control should measure the disturbance at the receiver with a short
local probe ground, then vary a single READY2 edge/return-path characteristic.

## Controller invocation

With the exclusive target and marker roles prepared as above:

```sh
python3 diagnostics/receive-only/generate-vectors.py /var/tmp/rx-vectors.bin --count 2000
mkdir /var/tmp/rx-run
sudo diagnostics/receive-only/rx-write-controller \
  --vectors /var/tmp/rx-vectors.bin --output /var/tmp/rx-run \
  --gate /var/tmp/rx-run/GATE
```

In another terminal, verify `ready.json`, arm the single C4 falling-edge trigger,
then create `/var/tmp/rx-run/GATE`. Stop the held controller after its result and
scope capture are saved. `--marker-only` exercises the marker without I2C traffic.
Read `/dev/bsc-rx-minimal` only after writes stop and compare it with the vector
stream. Chunk SCPI waveform exports and verify every returned block length: a
large native span may be truncated despite the requested point-count readback.

## Reversible slew comparison

On the verified Pi 3B+ map, `pad-control.py` snapshots GPIO0-27 pad fields with
host/model/boot identity. `--limit-slew SNAPSHOT` clears only the slew-unlimited
bit; drive strength and input hysteresis are preserved. `--restore SNAPSHOT`
restores the saved fields with readback verification. The setting is bank-wide,
so run an original/limited/original comparison with the same other GPIO activity
quiesced. Schedule an independent restore timer before changing it, and cancel
the timer only after the original value has been restored and read back.

`--self-test` verifies bit preservation without accessing hardware. `--save PATH`
creates a new snapshot and refuses to overwrite one. Restriction to the verified
Pi 3B+ model and same-host/same-boot restoration is intentional.

The raw controller's optional `--snapshot-after N` also marks and stops after
write N succeeds, before the reply-emulation delay. Its result records the option
and stores `snapshot-request.bin`; an actual failure still stops and saves
`failed-request.bin`. This permits like-for-like waveform snapshots when a tested
setting has no failures. The marker cannot perturb the selected write because
it is asserted after `write()` completes, and no subsequent writes are sent.

## Measured slew-control limits

The [slew comparison record](slew-validation.json) covers an original/limited/
original control with the same RX-only fixture, vectors, cadence and READY2 pulse
loop. Both production services are stopped throughout; Pi2 BSC is quiesced. The
100-write unpulsed baseline is byte-exact, with zero overflow or capture drops.

| Pi2 pad setting | Successful writes before failure | Failure | Selected READY2 rise | Selected SCL backstep |
| --- | ---: | --- | ---: | ---: |
| Original `0x1b` | 179 | Remote I/O/NACK | 6 ns | 0.37 V |
| Limited `0x0b` | 275 | Lost arbitration, SDA low | 8 ns | 0.22 V |
| Restored `0x1b` | 52 | Remote I/O/NACK | 6 ns | 0.29 V |

The three retained failure tails contain a READY2 **rising** edge during an SCL
rise, with a non-monotonic SCL excursion. Both READY2 polarities matter. The
selected dip is smaller with limiting, but those edges occur at different clock
phases; these samples do not establish a general amplitude or failure-rate
improvement. All settings fail, typical sampled READY2 falls remain 6-8 ns, and
coupling remains visible. Edge-time comparisons are limited by 2 ns sampling.

Only the slew bit changes; drive strength remains 8 mA and input hysteresis stays
enabled. This bank-wide software control does not replace receiver-local probing
or controlled signal/return-path changes. Probe skew, local reference and receiver
thresholds remain uncalibrated; concurrent HSM/PQC operation remains unqualified.
The original pad value, production services and unchanged module/worker/profile
hashes are verified after the test, and four ordinary echoes pass on each target.

## Independent drive-strength control

`pad-control.py --set-drive SNAPSHOT --drive-ma 4` sets the Pi3 GPIO0-27 bank
to 4 mA nominal drive while preserving the saved slew and input-hysteresis fields.
It requires the current fields to match the snapshot, accepts only documented
2 mA steps from 2 through 16 mA, and uses the same saved-value restore/readback
contract. Use an independent restore timer and quiesce unrelated bank outputs.
This changes READY and BSC SDA/SCL drive together; it cannot isolate READY alone.
Drive strength describes output capability, not a current limit. Preserve input
hysteresis, and verify low levels and timing under the actual combined pull-ups.

Production READY is active-low open-drain, implemented on these Pi3s by output-low
and input/release states. The fixed-direction pulse fixture instead drives both
levels. Its rising-edge drive/slew results therefore do not directly characterize
the production released rise, which depends on pull-up resistance and capacitance.

To test both output controls in one snapshot-checked change, use
`--set-drive SNAPSHOT --drive-ma 4 --slew-limited`. This sets the saved drive
field to 4 mA and enables slew limiting while preserving input hysteresis;
`--restore SNAPSHOT` returns both fields to their original values. Compare
original, drive-only, combined and restored conditions with identical traffic.

## Controller pad control and per-side matrix

`rp1-pad-control.py` is restricted to the verified Pi5 controller map and GPIO2/3.
It saves both RP1 pad words with host/model/boot identity and supports
`--set SNAPSHOT --drive-ma 4 --slew slow` or `--restore SNAPSHOT`. Supported
strengths are 2, 4, 8 and 12 mA. Only drive and slew fields change; input enable,
Schmitt hysteresis, pull configuration and output-disable fields are preserved.
The bus must be quiescent while changing the pair. Schedule an independent
restore timer before changes and verify both original pad words before cancelling it.
`--self-test` checks preservation over all 256 pad values, four drive strengths
and both slew modes without hardware access.

A per-side sweep holds the other boards at their saved values: 16 Pi2 source-bank
settings (2 through 16 mA with slew limited/unlimited), 16 Pi1 receiver-bank
settings, and eight Pi5 controller-pair settings. Both SDA/SCL pads receive the
same controller setting; this is not their independent-pin cross-product or a
joint cross-product of all three boards. Keep hysteresis enabled, I2C/READY
functions fixed, and controller READY inputs pulled up. Each setting needs an
unpulsed byte-integrity baseline and the same bounded READY2 pulse/vector run,
with zero overrun/drop counters and original values restored on every exit.

Production-style matrix stimulus must never drive READY2 high. Stop Pi2's target
owner, set GPIO17 output-low, and alternate `pinctrl set 17 ip pn` (release) with
`pinctrl set 17 op dl` (pull low), retaining the controller GPIO22 input pull-up.
Verify both wire endpoints read low when driven and high when released before
traffic. Direction changes implement the Pi3's software open-drain behavior;
they are intentional in this fixture. Keep SDA/SCL muxing fixed throughout each
transfer. Restore production ownership only after the pulse generator stops.

The push-pull capture and its 100 kHz, slew-only and drive-only comparisons remain
separate evidence. A push-pull fault does not by itself establish that the same
disturbance occurs under production low/release signaling.

## Export and analyze retained scope records

`siglent-capture.py` exports an already stopped SDS824X HD acquisition over TCP
5025. It verifies `*IDN?` and stopped state, uses native WORD samples by default,
and validates every definite-length reply and chunk size. It changes waveform
export selection, not acquisition or trigger configuration. It refuses a running
acquisition; stop or capture the desired event separately. An export does not
prove the initiating fault is present, especially after a delayed software stop.

```sh
python3 diagnostics/receive-only/siglent-capture.py \
  --host 192.0.2.91 --output /var/tmp/ready-capture --points 50000
```

Use `--points 0 --start 0` for the full acquisition; the default selects a centered
window. `--interval 1` retains the native sample interval. Raw descriptors and
samples plus `capture.json` are saved to a new directory. Socket-pair tests cover
fragmented binary payloads, embedded newlines, trailing terminators and truncation.

For C3 edge-triggered READY2 captures saved under a comparison root's immediate
`rising-*`/`falling-*` subdirectories, run:

```sh
python3 diagnostics/receive-only/analyze-ready-levels.py /var/tmp/ready-comparison
```

The analyzer uses two settled 10 microsecond windows, excluding the transition,
and reports median, RMS about the mean, peak-to-peak, percentile spread and clock
activity. The approximate 10–90% edge time is sample-quantized. C1=SCL, C2=SDA,
C3=READY2 and C4=READY1 must be present; trigger slope metadata must describe C3.
These are voltage/probe-reference observations, not calibrated receiver margins
or statistical failure-rate measurements.
