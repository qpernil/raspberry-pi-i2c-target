# Raw response exchange and explicit recovery

The kernel driver preserves application bytes. Normal controller writes are
received unchanged through character-device ABI 3; responses are queued unchanged.
Requests and responses support up to 8192 bytes. There is no additional request
envelope or checksum. Receive-only OLED/display streams and ordinary raw echo
clients retain their byte protocols. The direct `/dev/mem` demonstration is
independent of this driver and is unchanged.

## Normal exchange

The controller must fully consume the preceding response before sending another
normal request. The driver invalidates older software work on new input, detects
receive completion from idle/empty flags, and acknowledges acceptance with a
20 microsecond READY assertion followed by release. BSC stays enabled in I2C mode:
normal receives do not reset CR, purge the serializer, rewrite the address or
rewrite FIFO thresholds. READY remains asserted after a response read.

The shared bus lock covers the write and request acknowledgment, and separately
the complete response read. The HSM connector requests rising-edge detection once
and checks response readiness by level polling at 100 microsecond intervals.
Linux scheduling can extend that interval. It does not reconfigure READY edges
between requests. All clients sharing an adapter must cooperate in bus admission.

## Target-side explicit recovery ioctl

`BSC_TARGET_IOC_RECOVER`, `_IO('B', 3)` (`0x4203`), is issued by the application
owning the target descriptor after reading an explicit recovery request. GET_INFO
retains ABI 3 and advertises `BSC_TARGET_CAP_RECOVER` in `reserved[1]` bit 0.

The ioctl services pending input, verifies that the worker still owns the latest
request and that reception is idle, then discards old transmit state using the
hardware-qualified FIFO/serializer purge. It preserves the recovery request's
ownership so the application can publish its confirmation normally. READY remains
released for at least 20 microseconds before that reply. A superseded request or
active receive returns `EAGAIN`; absence of a pending request or READY returns
`EPROTO`. The controller must retain the physical bus lock through the entire
recovery exchange. The `recoveries` sysfs counter counts successful ioctls.

Initial device activation also initializes hardware. Ordinary FIFO flags cannot
prove that an abandoned serializer byte was consumed, so incomplete exchanges
require explicit recovery rather than inferring completion from TXFE/TXBUSY.

## HSM frontend recovery operation

The I2C HSM frontend, rather than the generic kernel driver, reserves one control
operation. It uses the existing three-byte HSM frame format:

- Request: `00 00 0d`, ASCII `BSCRECOV`, version byte `01`, four-byte big-endian token.
- Confirmation: identical 16 bytes with command byte `80`.

The frontend recognizes only this exact command, length, marker and version,
invokes the recovery ioctl, and returns the token confirmation. It does not send
the control operation to the HSM core. Ordinary requests go to the existing core
without another framing layer. NACK/arbitration/read/write errors are reported by
the adapter; application framing and authenticated-session MACs provide HSM error
checks. Unauthenticated raw protocols without integrity checks cannot detect every
silent bit error.

The connector performs this operation before DeviceInfo on every open/reconnect.
An incomplete or uncertain exchange closes the transport; a later connection
recovers before another command. Failed commands are never replayed. A failed
recovery fails the connection closed and does not clear a physically stuck bus.

Upgrade the HSM frontend and connector together with a recovery-capable driver.
Generic clients need no HSM control operation for normal complete exchanges.
Clients abandoning a response must implement recovery in their own application
protocol using the ioctl, or restart their target application while the bus is
quiescent. The driver reserves no byte sequence from those generic clients.

## Client compatibility

| Client | Contract |
| --- | --- |
| Raw READY request/response | Application bytes and ABI 3 layout are unchanged. Fully consume each reply. An abandoned reply requires an application recovery request followed by the owner ioctl, or a quiescent target restart. |
| HSM frontend and connector | Upgrade together with the recovery-capable driver. Normal HSM frames, authentication and secure-session checks are unchanged; connection startup uses the frontend control operation. |
| Receive-only OLED/display | No READY or recovery control is required. The receive ring and application bytes are unchanged. |
| Direct `/dev/mem` demonstration | Independent of this kernel driver and unchanged. |

The finite hardware record covers raw echo and receive-only byte paths. It does
not qualify every application, sustained concurrent authenticated PQC, or an
uncalibrated electrical margin.
