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
