# READY response transport, wire protocol 4

The character-device ABI remains version 3. READY response mode requires wire
protocol 4 on the I2C controller and target driver. Receive-only mode passes
unframed bytes unchanged. Existing HSM workers use the same character-device
interface and receive only validated request payloads.

Each controller write is one frame, encoded as:

| Offset | Size | Meaning |
| --- | --- | --- |
| 0 | 4 | `42 53 43 04` (`BSC`, version 4) |
| 4 | 1 | Kind: 1 normal request, 2 explicit recovery |
| 5 | 1 | Reserved, zero |
| 6 | 2 | Payload length, big endian |
| 8 | 4 | Controller token, big endian |
| 12 | length | Application request, empty for recovery |
| 12 + length | 4 | CRC-32/ISO-HDLC of all preceding bytes, big endian |

The 8192-byte Linux I2C write limit permits 8176 application-request bytes.
Application responses retain their existing framing and 8192-byte limit.
CRC detects accidental corruption; it is not authentication. Invalid lengths,
CRC failures, RX overruns and legacy frames are dropped before userspace and
require recovery before further normal requests. Partial records concatenated
with recovery data fail framing/CRC validation rather than becoming HSM commands.

## Normal exchange

The controller may send a normal request only after fully consuming the prior
response, or after successfully completing recovery. It holds the shared bus
lock through the complete write and the target's physical rising READY
acknowledgment. The target leaves BSC enabled in I2C mode, strips the envelope,
and queues the application payload. No control-register reset, serializer purge,
address rewrite or FIFO-threshold rewrite occurs on this path.

The target guarantees the acknowledgment with a 20 microsecond low pulse followed
by release. The controller requests rising-edge detection once and retains it
throughout the connection. It releases the bus lock after acknowledgment and
checks READY's active-low response level at bounded 100 microsecond intervals.
It reacquires the lock to read the exact complete response. READY remains low
afterward; the next acknowledged request clears the old READY state.

## Explicit recovery

Before DeviceInfo on every open/reconnect, and after an incomplete or uncertain
exchange before another command, the controller sends a kind-2 frame with empty
payload and a fresh token. It holds the bus lock for the whole recovery exchange.
The driver invalidates older work, purges the old FIFO/serializer, restores BSC
configuration, and returns `80 00 08 42 53 43 04` followed by the four-byte token.
READY remains released for at least 20 microseconds between the recovery
acknowledgment and response assertion so the controller can latch the rising
edge. The controller consumes and verifies all 11 bytes before any application request.
The recovery control frame never reaches an HSM worker. Generation checks discard
a late result from an older worker without replaying its operation.

The exceptional purge uses the hardware-qualified disable/enable sequence;
its intermediate modes are not used during normal exchanges. The `recoveries`
sysfs counter counts explicit wire recoveries separately from normal RX/TX
counters. Initial device activation also initializes hardware; it is not a
normal exchange. An unread serializer byte cannot be inferred from TXFE/TXBUSY,
so recovery cannot be replaced by trusting those flags.

A recovery handshake failure fails the connection closed. It does not replay an
uncertain HSM command, and it does not claim to clear a physically stuck bus.
Keep controller traffic quiescent during module/service lifecycle changes.
Old controllers and drivers are incompatible with this wire format and must be
updated together. A legacy driver cannot return the version/token confirmation;
a legacy controller's unframed request is rejected by the new driver.
