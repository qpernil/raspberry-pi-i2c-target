#!/usr/bin/env python3
"""Save/restore only the documented BCM2837 GPIO0-27 pad-control fields."""
import argparse, datetime, json, mmap, os, pathlib, socket, struct
ADDRESS = 0x3f100000
OFFSET = 0x2c
FIELDS = 0x1f
SLEW_UNLIMITED = 0x10

def desired(original, limited, drive_ma=None):
    value = (original & FIELDS) & (~SLEW_UNLIMITED if limited else FIELDS)
    if drive_ma is not None:
        if drive_ma not in range(2, 17, 2):
            raise ValueError('Drive strength must be 2..16 mA in steps of 2')
        value = (value & ~7) | (drive_ma // 2 - 1)
    return value

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
    a.add_argument('--set-drive', type=pathlib.Path)
    p.add_argument('--drive-ma', type=int, choices=range(2, 17, 2))
    p.add_argument('--slew-limited', action='store_true',
                   help='Also limit slew with --set-drive; preserve hysteresis')
    a.add_argument('--restore', type=pathlib.Path)
    a.add_argument('--self-test', action='store_true')
    args = p.parse_args()
    if (args.drive_ma is not None) != (args.set_drive is not None):
        p.error('--set-drive SNAPSHOT and --drive-ma MA must be used together')
    if args.slew_limited and args.set_drive is None:
        p.error('--slew-limited requires --set-drive SNAPSHOT and --drive-ma MA')
    if args.self_test:
        assert desired(0x1b, True) == 0x0b
        for v in range(32):
            assert desired(v, True) & 0xf == v & 0xf
            assert desired(v, False) == v
            assert desired(v, True) ^ v in (0, 16)
        for v in range(32):
            for drive in range(2, 17, 2):
                result = desired(v, False, drive)
                assert result & 0x18 == v & 0x18
                assert 2 * ((result & 7) + 1) == drive
                combined = desired(v, True, drive)
                assert combined & 0x18 == v & 0x08
                assert 2 * ((combined & 7) + 1) == drive
        for invalid in [0, 1, 3, 18]:
            try:
                desired(0x1b, False, invalid)
            except ValueError:
                pass
            else:
                raise AssertionError('Invalid drive accepted')
        print('PASS: independent slew/drive fields; hysteresis and other fields preserved')
        return
    ident = identity()
    modifying = args.limit_slew or args.set_drive or args.restore
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
                if args.set_drive and before != original:
                    raise RuntimeError('Pad changed since snapshot')
                after = desired(original, bool(args.limit_slew or args.slew_limited), args.drive_ma)
                struct.pack_into('<I', m, OFFSET, 0x5a000000 | after)
                readback = struct.unpack_from('<I', m, OFFSET)[0] & FIELDS
                if readback != after:
                    raise RuntimeError('Pad write did not read back correctly')
            print(json.dumps({**ident, 'before': hex(before), 'after': hex(after),
                              'slew_unlimited': bool(after & 16), 'hysteresis': bool(after & 8),
                              'drive_mA': 2 * ((after & 7) + 1)}))
if __name__ == '__main__':
    main()
