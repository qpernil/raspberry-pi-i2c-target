#!/usr/bin/env python3
"""Reproduce the echo stress tool's seed/size sequence as raw write vectors."""
import argparse, hashlib, json, pathlib, random, struct

def frames(count, seed=91):
    rng = random.Random(seed)
    sizes = [0,1,2,15,16,17,31,32,255,256,1024,3133,4093,4668]
    bulk = [31,255,1024,2441,3133,4093,4668]
    for i in range(count):
        length = sizes[i % len(sizes)] if i < 60 else rng.choice(bulk)
        yield b"\x01" + length.to_bytes(2,"big") + rng.randbytes(length)

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('output',type=pathlib.Path);p.add_argument('--count',type=int,default=2000);p.add_argument('--seed',type=int,default=91)
    a=p.parse_args()
    if not 1 <= a.count <= 5000:p.error('count must be 1..5000')
    data=list(frames(a.count,a.seed))
    a.output.write_bytes(b''.join(struct.pack('<I',len(v))+v for v in data))
    a.output.with_suffix('.json').write_text(json.dumps({'count':a.count,'seed':a.seed,'total_bytes':sum(map(len,data)),'stream_sha256':hashlib.sha256(b''.join(data)).hexdigest(),'sha256':[hashlib.sha256(v).hexdigest() for v in data]},indent=2)+'\n')
if __name__=='__main__':main()
