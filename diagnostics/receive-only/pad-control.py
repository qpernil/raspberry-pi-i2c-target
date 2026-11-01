#!/usr/bin/env python3
"""Save/restore only the documented BCM2837 GPIO0-27 pad-control fields."""
import argparse, datetime, json, mmap, os, pathlib, socket, struct
ADDRESS = 0x3f100000
OFFSET = 0x2c
FIELDS = 0x1f
SLEW_UNLIMITED = 0x10

def desired(original, limited):
    return (original & FIELDS) & (~SLEW_UNLIMITED if limited else FIELDS)

def identity():
    model = pathlib.Path('/proc/device-tree/model').read_bytes().rstrip(b'\0').decode()
    if 'Raspberry Pi 3 Model B Plus' not in model:
        raise RuntimeError('This diagnostic is restricted to the verified Pi 3B+ register map')
    return {'model': model, 'host': socket.gethostname(),
            'boot_id': pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()}

def main():
    p = argparse.ArgumentParser(description=__doc__)
    a = p.add_mutually_exclusive_group(required=True)
    a.add_argument('--read', action='store_true')
    a.add_argument('--save', type=pathlib.Path)
    a.add_argument('--limit-slew', type=pathlib.Path)
    a.add_argument('--restore', type=pathlib.Path)
    a.add_argument('--self-test', action='store_true')
    args = p.parse_args()
    if args.self_test:
        assert desired(0x1b, True) == 0x0b
        for v in range(32):
            assert desired(v, True) & 0xf == v & 0xf
            assert desired(v, False) == v
            assert desired(v, True) ^ v in (0, 16)
        print('PASS: only slew bit changes; drive and hysteresis preserved')
        return
    ident = identity()
    modifying = args.limit_slew or args.restore
    snapshot = None
    if modifying:
        snapshot = json.loads(modifying.read_text())
        if any(snapshot[k] != ident[k] for k in ident):
            raise RuntimeError('Snapshot host/model/boot differs; refusing a stale restore')
        if snapshot['address'] != ADDRESS + OFFSET:
            raise RuntimeError('Unexpected snapshot register')
    with open('/dev/mem', 'r+b' if modifying else 'rb', buffering=0) as f:
        prot = mmap.PROT_READ | (mmap.PROT_WRITE if modifying else 0)
        with mmap.mmap(f.fileno(), 4096, flags=mmap.MAP_SHARED, prot=prot, offset=ADDRESS) as m:
            before = struct.unpack_from('<I', m, OFFSET)[0] & FIELDS
            after = before
            if args.save:
                record = {**ident, 'address': ADDRESS + OFFSET, 'value': before,
                          'saved_utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
                with args.save.open('x') as out:
                    json.dump(record, out, indent=2);out.write('\n');out.flush();os.fsync(out.fileno())
            if modifying:
                original = snapshot['value']
                if not 0 <= original <= FIELDS:
                    raise RuntimeError('Invalid saved pad value')
                if args.limit_slew and (before != original or not original & SLEW_UNLIMITED):
                    raise RuntimeError('Pad changed since snapshot or slew already limited')
                after = desired(original, bool(args.limit_slew))
                struct.pack_into('<I', m, OFFSET, 0x5a000000 | after)
                readback = struct.unpack_from('<I', m, OFFSET)[0] & FIELDS
                if readback != after:
                    raise RuntimeError('Pad write did not read back correctly')
            print(json.dumps({**ident, 'before': hex(before), 'after': hex(after),
                              'slew_unlimited': bool(after & 16), 'hysteresis': bool(after & 8),
                              'drive_mA': 2 * ((after & 7) + 1)}))
if __name__ == '__main__':
    main()
