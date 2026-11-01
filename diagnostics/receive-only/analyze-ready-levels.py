#!/usr/bin/env python3
"""Compare settled READY2 levels in saved, native-sample edge captures.

Usage: analyze-ready-levels.py COMPARISON_ROOT [COMPARISON_ROOT ...]
Each root contains rising-*/falling-* directories with capture.json and C1-C4.
"""
import array
import json
import pathlib
import statistics
import struct
import sys

if len(sys.argv) < 2:
    raise SystemExit(__doc__)

rows = []
for arg in sys.argv[1:]:
    root = pathlib.Path(arg)
    for p in sorted(root.glob('*-*/capture.json')):
        meta = json.loads(p.read_text()); waves = {}
        for ch in [1, 2, 3, 4]:
            pre = (p.parent / f'C{ch}-preamble.bin').read_bytes()
            scale, offset, codes = struct.unpack_from('<fff', pre, 156)
            probe = struct.unpack_from('<f', pre, 328)[0]
            raw = array.array('h'); raw.frombytes((p.parent / f'C{ch}-data.bin').read_bytes())
            if sys.byteorder != 'little':
                raw.byteswap()
            waves[ch] = [v / codes * scale * probe - offset * probe for v in raw]
        x = waves[3]; n = len(x); dt = meta['channels']['3']['dt']
        slope = meta['slope'].lower()
        if not (slope.startswith('ris') or slope.startswith('fall')):
            raise ValueError(f'{p}: unsupported edge slope')
        rising = slope.startswith('ris')
        crossings = [i for i in range(1, n) if (x[i-1] < 1.6 <= x[i] if rising else x[i-1] >= 1.6 > x[i])]
        edge = min(crossings, key=lambda i: abs(i - n // 2))
        row = {'capture': str(p.parent), 'slope': meta['slope'], 'native_dt_ns': dt * 1e9, 'settled_windows': {}}
        plateaus = []
        for name, lo, hi in [('before', -20e-6, -10e-6), ('after', 10e-6, 20e-6)]:
            start, stop = edge + round(lo / dt), edge + round(hi / dt)
            assert 0 <= start < stop <= n, (p, start, stop, n)
            v = x[start:stop]; ordered = sorted(v); count = len(v); middle = statistics.median(v)
            plateaus.append(middle)
            row['settled_windows'][name] = {'level': 'high' if middle > 1.6 else 'low', 'median_V': middle, 'rms_about_mean_mV': statistics.pstdev(v) * 1000, 'peak_to_peak_mV': (max(v) - min(v)) * 1000, 'p99_minus_p01_mV': (ordered[int(.99 * (count - 1))] - ordered[int(.01 * (count - 1))]) * 1000, 'SCL_threshold_rises': sum(waves[1][i-1] < 1.6 <= waves[1][i] for i in range(start+1, stop)), 'samples': count}
        low, high = sorted(plateaus); v10, v90 = low + .1 * (high - low), low + .9 * (high - low)
        def crossing(v):
            return [i for i in range(1, n) if (x[i-1] < v <= x[i] if rising else x[i-1] >= v > x[i])]
        first = max((i for i in crossing(v10 if rising else v90) if i <= edge), default=None)
        last = min((i for i in crossing(v90 if rising else v10) if i >= edge), default=None)
        row['transition_10_90_or_90_10_ns'] = None if first is None or last is None else (last - first) * dt * 1e9
        rows.append(row)

result = {'method': 'C3 READY2, raw native samples, two 10 us windows per capture: 20 to 10 us before and 10 to 20 us after the READY threshold crossing. Transition region excluded.', 'limits': 'Typical edge captures at different clock phases; not first-fault captures. Observed voltages include probe/ground-reference effects. RMS is standard deviation about each window mean, not receiver noise margin.', 'captures': rows}
if not rows:
    raise SystemExit('No matching READY2 edge captures found')
print(json.dumps(result, indent=2))
