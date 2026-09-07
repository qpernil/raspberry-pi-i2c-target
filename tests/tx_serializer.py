#!/usr/bin/env python3
"""Compile the driver's actual refill/service functions against a BSC FIFO model.

The model deliberately keeps an unread serializer byte while TXFE is true and
TXBUSY is false, as observed on BCM hardware. Exercise exact reads, abandoned reads and superseded worker results without
guard bytes or completion inference.
"""
from pathlib import Path
import subprocess
import tempfile

source = (Path(__file__).resolve().parents[1] / 'kernel/bcm27xx_bsc_target.c').read_text()


def function(name):
    start = source.index('static ', source.rindex('\n}', 0, source.index(name)) + 2)
    brace = source.index('{', source.index(name, start))
    depth = 1
    end = brace + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


prefix = r"""
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
#define BIT(n) (1U << (n))
"""
defines = source[source.index('#define DR '):source.index('struct bsc_rx_slot')]
model = r"""
struct bsc_rx_slot { u32 len; u8 data[BSC_MAX_TRANSFER]; };
struct bsc_target {
    bool tx_queued, worker_pending, rx_overflowed;
    size_t tx_len, tx_loaded, rx_work_len;
    u64 request_generation, worker_generation;
    void *ready_gpio;
    u8 tx_data[BSC_MAX_TRANSFER], rx_work[BSC_MAX_TRANSFER];
    struct bsc_rx_slot *rx_slots;
    u32 rx_head, rx_tail, rx_count;
    struct { unsigned rx_overruns, rx_dropped, rx_transactions, rx_bytes,
        tx_underruns, tx_transactions, tx_bytes, tx_discarded; } stats;
    int rx_wait;
};
static u8 fifo[16], serializer, input[8192];
static unsigned count, head, resets, input_len, input_pos, acks;
static bool occupied, ready_asserted, rx_busy, bus_locked;
static u32 bsc_read(struct bsc_target *bsc, u32 reg) {
    (void)bsc;
    if (reg == RSR) return 0;
    if (reg == DR) { assert(input_pos < input_len); return input[input_pos++]; }
    assert(reg == FR);
    return (rx_busy ? FR_RXBUSY : 0) | (input_pos == input_len ? FR_RXFE : 0) |
        (count == 16 ? FR_TXFF : 0) | (count == 0 ? FR_TXFE : 0) |
        (count << FR_TXFLEVEL_SHIFT);
}
static void bsc_write(struct bsc_target *bsc, u32 reg, u32 value) {
    (void)bsc;
    if (reg != DR) return;
    if (!occupied) { serializer = value; occupied = true; return; }
    assert(count < 16); fifo[(head + count++) % 16] = value;
}
static void bsc_configure_locked(struct bsc_target *bsc) {
    (void)bsc; assert(bus_locked); count = head = 0; occupied = false; resets++;
}
static void bsc_set_ready(struct bsc_target *bsc, bool ready) {
    (void)bsc;
    if (!ready) { assert(bus_locked && !occupied && !count); acks++; }
    ready_asserted = ready;
}
static void udelay(unsigned delay) { assert(delay == 20); }
static void wake_up_interruptible(int *wait) { (void)wait; }
"""
tests = r"""
static void receive(struct bsc_target *bsc, u8 command) {
    bus_locked = true; input[0] = command; input_len = 1; input_pos = 0;
    unsigned before = acks;
    bsc_service_locked(bsc);
    assert(acks == before + 1 && !ready_asserted && !occupied && !count);
    bus_locked = false;
}
static void take_request(struct bsc_target *bsc, u8 expected) {
    assert(bsc->rx_count == 1);
    assert(bsc->rx_slots[bsc->rx_head].data[0] == expected);
    bsc->rx_head = (bsc->rx_head + 1) % BSC_RX_SLOTS; bsc->rx_count--;
    bsc->worker_generation = bsc->request_generation; bsc->worker_pending = true;
}
static u8 clock_byte(struct bsc_target *bsc) {
    assert(occupied); u8 value = serializer;
    if (count) { serializer = fifo[head++ % 16]; count--; } else occupied = false;
    bsc_service_locked(bsc); return value;
}
int main(void) {
    static struct bsc_rx_slot slots[BSC_RX_SLOTS];
    struct bsc_target bsc = { .rx_slots = slots, .ready_gpio = &bsc };
    for (unsigned length = 1; length <= 8192; length = length < 40 ? length + 1 : (length < 4096 ? length * 2 : (length < 8192 ? 8192 : 8193))) {
        receive(&bsc, 1); take_request(&bsc, 1);
        u8 response[BSC_MAX_TRANSFER];
        for (unsigned i = 0; i < length; i++) response[i] = i * 37 + 11;
        assert(bsc_publish_response_locked(&bsc, response, length) == (int)length);
        assert(ready_asserted);
        unsigned before = resets;
        for (unsigned i = 0; i < length; i++) {
            for (unsigned pause = 0; pause < 100; pause++) bsc_service_locked(&bsc);
            assert(clock_byte(&bsc) == response[i]);
            assert(resets == before && ready_asserted);
        }
        assert(!count && !occupied); /* No trailer/guard was queued. */
    }
    receive(&bsc, 2); take_request(&bsc, 2);
    const u8 abc[] = {0xa1, 0xb2, 0xc3};
    assert(bsc_publish_response_locked(&bsc, abc, 3) == 3);
    assert(clock_byte(&bsc) == 0xa1 && clock_byte(&bsc) == 0xb2);
    assert(count == 0 && occupied && serializer == 0xc3);
    receive(&bsc, 3); /* Clears the abandoned serializer. */
    take_request(&bsc, 3);
    receive(&bsc, 4); receive(&bsc, 5); /* Latest request wins during computation. */
    assert(bsc.rx_count == 1);
    assert(bsc_publish_response_locked(&bsc, abc, 3) == 3);
    assert(!occupied && !ready_asserted && bsc.stats.tx_discarded == 1);
    take_request(&bsc, 5);
    assert(bsc_publish_response_locked(&bsc, abc, 3) == 3);
    assert(clock_byte(&bsc) == 0xa1);
    /* New bytes already in hardware invalidate the result before the timer runs. */
    receive(&bsc, 6); take_request(&bsc, 6);
    bus_locked = true; rx_busy = true; input[0] = 7; input_len = 1; input_pos = 0;
    assert(bsc_publish_response_locked(&bsc, abc, 3) == 3);
    assert(bsc.stats.tx_discarded == 2 && !occupied);
    rx_busy = false; bsc_service_locked(&bsc); bus_locked = false;
    take_request(&bsc, 7);
    assert(bsc_publish_response_locked(&bsc, abc, 3) == 3);
    assert(clock_byte(&bsc) == 0xa1 && clock_byte(&bsc) == 0xb2 && clock_byte(&bsc) == 0xc3);
    puts("PASS: staged exact reads have no guard, retirement, or reset");
    puts("PASS: next requests clear abandoned data and discard superseded worker results");
}
"""
with tempfile.TemporaryDirectory() as temp:
    path = Path(temp)
    c = path / 'test.c'
    c.write_text(prefix + defines + model + '\n'.join(function(name) for name in (
        'bsc_set_interrupts_locked', 'bsc_drain_rx_locked', 'bsc_refill_tx_locked',
        'bsc_finish_rx_locked', 'bsc_service_locked', 'bsc_publish_response_locked')) + tests)
    subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror', str(c), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
