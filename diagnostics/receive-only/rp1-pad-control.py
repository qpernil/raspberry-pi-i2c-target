#!/usr/bin/env python3
"""Reversible Pi5 controller GPIO2/3 output drive/slew diagnostic."""
import argparse, datetime, json, mmap, pathlib, socket, struct
BASE = 0x1f000f0000
PINS = (2, 3)
OUTPUT_FIELDS = 0x31
DRIVES = (2, 4, 8, 12)

def desired(value, drive_ma, fast):
    if drive_ma not in DRIVES:
        raise ValueError('Unsupported RP1 drive strength')
    return (value & ~OUTPUT_FIELDS) | (DRIVES.index(drive_ma) << 4) | int(fast)

def identity():
    model = pathlib.Path('/proc/device-tree/model').read_bytes().rstrip(b'\0').decode()
    if 'Raspberry Pi 5 Model B' not in model:
        raise RuntimeError('Restricted to the verified Pi5 controller map')
    if '1f000f0000-1f000fbfff' not in pathlib.Path('/proc/iomem').read_text():
        raise RuntimeError('Verified RP1 pad resource absent')
    return {'model': model, 'host': socket.gethostname(),
            'boot_id': pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()}

def decode(value):
    return {'raw': hex(value), 'drive_mA': DRIVES[(value >> 4) & 3],
            'slew_fast': bool(value & 1), 'hysteresis': bool(value & 2),
            'pull_up': bool(value & 8), 'pull_down': bool(value & 4),
            'input_enable': bool(value & 64)}

def main():
    p = argparse.ArgumentParser(description=__doc__)
    a = p.add_mutually_exclusive_group(required=True)
    a.add_argument('--read', action='store_true')
    a.add_argument('--save', type=pathlib.Path)
    a.add_argument('--set', type=pathlib.Path)
    a.add_argument('--restore', type=pathlib.Path)
    a.add_argument('--self-test', action='store_true')
    p.add_argument('--drive-ma', type=int, choices=DRIVES)
    p.add_argument('--slew', choices=('slow', 'fast'))
    args = p.parse_args()
    if bool(args.set) != (args.drive_ma is not None and args.slew is not None):
        p.error('--set SNAPSHOT requires --drive-ma and --slew')
    if not args.set and (args.drive_ma is not None or args.slew is not None):
        p.error('Drive/slew options require --set')
    if args.self_test:
        for value in range(256):
            for drive in DRIVES:
                for fast in (False, True):
                    result = desired(value, drive, fast)
                    assert result & ~OUTPUT_FIELDS == value & ~OUTPUT_FIELDS
                    assert decode(result)['drive_mA'] == drive
                    assert decode(result)['slew_fast'] == fast
        print('PASS: RP1 drive/slew preserve hysteresis, bias, input/output enables')
        return
    ident = identity()
    modifying = args.set or args.restore
    snapshot = json.loads(modifying.read_text()) if modifying else None
    if snapshot and (any(snapshot[k] != ident[k] for k in ident)
                     or snapshot['base'] != BASE or snapshot['pins'] != list(PINS)):
        raise RuntimeError('Snapshot identity/register/pins differ')
    with open('/dev/mem', 'r+b' if modifying else 'rb', buffering=0) as f:
        prot = mmap.PROT_READ | (mmap.PROT_WRITE if modifying else 0)
        with mmap.mmap(f.fileno(), 4096, flags=mmap.MAP_SHARED, prot=prot, offset=BASE) as m:
            before = {str(pin): struct.unpack_from('<I', m, 4 + 4 * pin)[0] for pin in PINS}
            after = before.copy()
            if args.save:
                record = {**ident, 'base': BASE, 'pins': list(PINS), 'values': before,
                          'saved_utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
                with args.save.open('x') as out:
                    json.dump(record, out, indent=2); out.write('\n'); out.flush()
                    import os
                    os.fsync(out.fileno())
            if modifying:
                original = snapshot['values']
                if set(original) != {str(pin) for pin in PINS} or any(
                        not isinstance(v, int) or not 0 <= v <= 255 for v in original.values()):
                    raise RuntimeError('Invalid saved pad values')
                if args.set and before != original:
                    raise RuntimeError('Pads changed since snapshot')
                if args.restore and any((before[k] ^ original[k]) & ~OUTPUT_FIELDS for k in before):
                    raise RuntimeError('Unrelated pad fields changed; refusing to overwrite them')
                after = {k: desired(v, args.drive_ma, args.slew == 'fast')
                         if args.set else original[k] for k, v in before.items()}
                # Independent writes; quiesce the bus before changing either pad.
                # A partial failure is restored from the same saved pair.
                for pin in PINS:
                    struct.pack_into('<I', m, 4 + 4 * pin, after[str(pin)])
                readback = {str(pin): struct.unpack_from('<I', m, 4 + 4 * pin)[0] for pin in PINS}
                if readback != after:
                    raise RuntimeError('RP1 pad readback mismatch')
            print(json.dumps({**ident, 'before': {k: decode(v) for k, v in before.items()},
                              'after': {k: decode(v) for k, v in after.items()}}))
if __name__ == '__main__':
    main()
