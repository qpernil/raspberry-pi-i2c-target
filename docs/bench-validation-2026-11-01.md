# Bench validation: 2026-11-01

Hostnames and device/account identifiers below are illustrative; replace them with your own configured values.

This investigation record describes finite measurements on the Pi5 controller
and two Pi3B+ targets. The companion [JSON record](bench-validation-2026-11-01.json)
contains source identities, exact recovery results and local evidence hashes.
Shared-bus reliability remains unqualified. Neither a GPIO setting nor a physical
coupling mechanism has been established as the solution.

## Implemented system and deployed state

Normal exchanges retain BSC I2C mode and unchanged application bytes. Purging is
reserved for explicit recovery after an incomplete exchange or reconnect; failed
commands are not automatically replayed. See the [transport contract](response-transport.md).
The receive-only fixture, pad helpers, scope exporter and analysis tools are
optional diagnostics. They are not production driver behavior.

The production target module and HSM workers remain installed on both Pis, with
normal supervisor services active. The build-host connector is built from Git with
`experimental-i2c,firmware-full`; no connector prebuilt is committed. Its base
unit and I2C override are recorded in the
[connector deployment directory](https://github.com/qpernil/pkcs11rs/tree/master/deployment/).
The connector starts automatically and exposes two USB and two I2C HSMs.
Embedded HSM `99000001` is disabled in both unit files; its persistent state and
root-only backup are retained. No stored key was removed for this closeout.

The machines were rebooted and no pad tuning reapplied. Observed defaults are
8 mA/unlimited slew/Schmitt on for each Pi3 GPIO0–27 bank, and
12 mA/slow slew/Schmitt on for the Pi5 SDA/SCL pads. These are bench readbacks,
not universal boot guarantees. The connector configures GPIO23/22 as pulled-up
READY inputs. Target GPIO17 drives low or releases. READY2 returns to GPIO22;
C1=SCL, C2=SDA, C3=READY2 through the breadboard, C4=READY1. Diagnostic marker
roles, inverted READY and push-pull stimulus are not left active.

## Receive-only and electrical controls

The configure-once RX fixture reproduces failure with `CR=0x205`: TX disabled,
no response publication, no Pi1 READY transitions and no per-request BSC changes.
A 100-write baseline is byte-exact; the push-pull READY2 stress control fails on
write 119. The first wrong byte is offset 952 of a 2,444-byte request, not byte
4,096. The [retained failure](../diagnostics/receive-only/validation.json) correlates
expected `f3`, wire `f2`, target `f9` and disturbed SCL. An extra receiver clock
sample and early ACK fit the capture; local-ground effects, probe skew, receiver
thresholds and the coupling path remain uncalibrated.

| Control | Finite outcome | Interpretation limit |
| --- | --- | --- |
| Push-pull, 100 kHz | Unpulsed 100-write pass; pulsed failure after 11 successes | Lower rate does not eliminate this stress-control fault |
| Push-pull, slew original/limited/restored | Failures after 179/275/52 successes | Different edge phases; no established failure-rate improvement |
| Low/release per-side pad matrix | 17 of 40 settings completed; source 2/4 mA with either slew passed 2,000 writes each | Source 6–16 mA failed; first receiving-side 2 mA setting failed; controller sweep not run |
| Production 2 mA trial | 200 concurrent echoes per target and 8,192-byte total frames pass | Finite trial, not a qualified setting |
| Production 4 mA/limited slew | 1,000 concurrent echoes per target; 2,000 authenticated reads per target; 100 RSA-4096 signs alongside 4,000 peer reads; 100 maximum frames per target pass | Existing-key signatures return 512 bytes; signatures were not independently verified |
| Repeated ordinary production traffic at 4 mA/limited slew | Fails after 169/168 successful echoes in 40.52 seconds | No artificial READY pulses; 4 mA is not a demonstrated fix |

The matrix was stopped at the user's request. A subsequent receiving-side
baseline with RX overflow is excluded. The unrun combinations are not qualified
and are not a pending deployment step. No drive/slew override is recommended or
installed, and these finite runs do not support comparative failure-rate claims.

READY2-only comparisons also fail with low/release, fixed-direction push-pull,
and inverted high/release signaling, after 332, 145 and 453 successful echoes
respectively. Direction changes are therefore unnecessary for the push-pull
failure. Ordinary production signaling is restored. Typical edge captures show
settled RMS of 69–74 mV at normal driven low versus 46–61 mV released high;
inverted released low is 49–68 mV versus 85–88 mV driven high. These captures use
different clock phases and include probe/reference effects; they are not the
initiating fault captures or calibrated noise margins. Changing polarity does
not establish a reliable transport.

## Recovery without restarting services

On reboot defaults, normal production traffic encounters a transport failure
after 285 Pi1 and 284 Pi2 echoes. The controller reports arbitration loss and
SDA stuck low. Immediate distinct commands fail on both targets. A different
Pi2 command sent 1.048 seconds after the original error succeeds in 8.34 ms;
three later fresh echoes per target also pass. The peer was not reprobed at
1 second, so its recovery latency is unknown.

Connector PID 5130, invocation, restart count and both target service identities
remain unchanged. Each target's recovery counter increases from 2 to 3. There
is no manual bus clear or service restart during this episode. This demonstrates
one in-place recovery through transport reopen and explicit recovery, while
showing that immediate availability is not guaranteed. It does not qualify
authenticated-session/PQC recovery or recovery from every physical stuck state.

A separate HTTP-200 HSM `WrongLength` reply (`7f000108`) is followed by six
successful fresh echoes, but no recovery-counter change; it is not evidence of
the explicit recovery path. Another run interrupted after 1,528/1,529 successes
by a diagnostic monitor error is excluded from recovery conclusions.

The reusable
[fresh-command probe](https://github.com/qpernil/pkcs11rs/blob/master/tools/i2c-recovery-probe.py)
uses unique nonces, distinguishes HSM replies from HTTP/transport errors, checks
optional service identity and writes atomic results. Its local HTTP fixtures
test delayed availability, no replay, failed recovery, no observed fault and
restart detection. Kernel logs and counter deltas remain necessary evidence.

## Preservation and remaining qualification

Read-only authenticated inventories retain every earlier object's metadata:
99 objects on Pi1 and 21 on Pi2. The final inventories contain 108 and 30,
including nine additional iPhone smoke-test key/certificate objects per target
at IDs `7e20`–`7e22`; those unrelated objects are preserved. This closeout neither
creates nor deletes HSM keys. The disabled embedded state's hash is unchanged.

The physical bus fault, calibrated receiver-local measurements and concurrent
authenticated/PQC recovery remain unresolved qualification work. Raw waveforms,
logs and operator scripts stay in the local evidence workspace; published hashes
identify the retained bundles without publishing private HSM state. Consumer CI
and successful finite traffic do not resolve the physical fault.
