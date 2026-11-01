#!/usr/bin/env python3
"""Export an already stopped SDS824X HD acquisition; never arm or force a trigger."""
import argparse
import json
import math
from pathlib import Path
import socket
import struct
import time


class Scpi:
    def __init__(self, host, port=5025):
        self.connection = socket.create_connection((host, port), timeout=3)
        self.connection.settimeout(10)

    def close(self):
        self.connection.close()

    def exact(self, count):
        data = bytearray()
        while len(data) < count:
            part = self.connection.recv(count - len(data))
            if not part:
                raise EOFError("scope closed during a reply")
            data.extend(part)
        return bytes(data)

    def send(self, command):
        self.connection.sendall((command + "\n").encode("ascii"))

    def set(self, command):
        self.send(command)
        time.sleep(.02)

    def query(self, command):
        self.send(command)
        while True:
            data = bytearray()
            while not data.endswith(b"\n"):
                data.extend(self.exact(1))
                if len(data) > 4096:
                    raise ValueError("oversized text reply")
            if data.strip():
                return data.decode("ascii").strip()

    def binary(self, command):
        self.send(command)
        # A previous definite-length block can leave a trailing LF.
        while True:
            prefix = self.exact(1)
            if prefix == b"#":
                break
            if prefix not in b"\r\n":
                raise ValueError("expected a definite-length SCPI block")
        digits = int(self.exact(1))
        if not 1 <= digits <= 9:
            raise ValueError("indefinite-length blocks are unsupported")
        length = int(self.exact(digits))
        if not 0 <= length <= 1_000_000:
            raise ValueError("oversized waveform block")
        return self.exact(length)


def export(args):
    destination = Path(args.output)
    destination.mkdir(parents=True, exist_ok=False)
    scope = Scpi(args.host)
    metadata = {"captured_at_unix": time.time(), "channels": {},
                "limit": "Saved voltage samples include probe/reference effects; a software-stopped record need not contain the initiating fault."}
    try:
        metadata["instrument"] = scope.query("*IDN?")
        if "Siglent Technologies,SDS824X HD," not in metadata["instrument"]:
            raise ValueError("descriptor layout is verified only for SDS824X HD")
        metadata["acquisition_state"] = scope.query(":TRIG:STAT?")
        if metadata["acquisition_state"].lower() != "stop":
            raise ValueError("stop the acquisition before exporting; this tool does not stop it")
        metadata["trigger_type"] = scope.query(":TRIG:TYPE?")
        if metadata["trigger_type"].upper() == "EDGE":
            metadata["edge_source"] = scope.query(":TRIG:EDGE:SOUR?")
            if metadata["edge_source"] == "C3":
                metadata["slope"] = scope.query(":TRIG:EDGE:SLOP?")
        for channel in args.channels:
            for command in [f":WAV:SOUR C{channel}", ":WAV:WIDT WORD",
                            ":WAV:STAR 0", ":WAV:INT 1", ":WAV:POIN 0"]:
                scope.set(command)
            preamble = scope.binary(":WAV:PRE?")
            if len(preamble) < 332:
                raise ValueError("short waveform descriptor")
            total = struct.unpack_from("<i", preamble, 116)[0]
            dt = struct.unpack_from("<f", preamble, 176)[0]
            if not 0 < total <= 30_000_000 or not math.isfinite(dt) or dt <= 0:
                raise ValueError("invalid waveform descriptor")
            available = (total + args.interval - 1) // args.interval
            count = min(args.points or available, available)
            start = args.start if args.start is not None else max(0, (total - count * args.interval) // 2)
            if start >= total:
                raise ValueError("start lies outside the acquisition")
            count = min(count, (total - start + args.interval - 1) // args.interval)
            data = bytearray()
            scope.set(":WAV:INT " + str(args.interval))
            for offset in range(0, count, 100000):
                size = min(100000, count - offset)
                scope.set(":WAV:STAR " + str(start + offset * args.interval))
                scope.set(":WAV:POIN " + str(size))
                block = scope.binary(":WAV:DATA?")
                if len(block) != size * 2:
                    raise ValueError("short waveform block")
                data.extend(block)
            (destination / f"C{channel}-data.bin").write_bytes(data)
            (destination / f"C{channel}-preamble.bin").write_bytes(preamble)
            metadata["channels"][str(channel)] = {
                "native_points": total, "points": count, "start": start,
                "interval": args.interval, "native_dt": dt, "dt": dt * args.interval}
            print(f"C{channel}: {count} points, {dt * args.interval:g} s/sample", flush=True)
        if scope.query(":TRIG:STAT?").lower() != "stop":
            raise RuntimeError("acquisition state changed during export")
    finally:
        scope.close()
    (destination / "capture.json").write_text(json.dumps(metadata, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--output", required=True, help="new local capture directory")
    parser.add_argument("--channels", nargs="+", type=int, choices=range(1, 5), default=[1, 2, 3, 4])
    parser.add_argument("--points", type=int, default=50000, help="output points per channel; 0 exports all")
    parser.add_argument("--start", type=int, help="native start index; default centers the selected window")
    parser.add_argument("--interval", type=int, default=1, help="native sample stride; 1 preserves resolution")
    args = parser.parse_args()
    if (not 0 <= args.points <= 30_000_000 or args.interval < 1
            or (args.start is not None and args.start < 0)
            or len(set(args.channels)) != len(args.channels)):
        parser.error("invalid points, interval, start or duplicate channels")
    export(args)


if __name__ == "__main__":
    main()
