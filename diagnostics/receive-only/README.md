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
with only the Pi2 GPIO17 high/low pulse loop added. Pi2 receives no I2C commands.
A passing finite run does not establish that every response path is safe.

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
