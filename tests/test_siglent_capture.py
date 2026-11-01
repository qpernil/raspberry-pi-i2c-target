"""SCPI framing checks using socket pairs, with no instrument access."""
import importlib.util
from pathlib import Path
import socket
import threading
import unittest

SPEC = importlib.util.spec_from_file_location(
    "siglent_capture", Path(__file__).resolve().parents[1] / "diagnostics/receive-only/siglent-capture.py")
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)


class FramingTests(unittest.TestCase):
    def test_fragmented_binary_block_and_trailing_newline_before_text(self):
        client, server = socket.socketpair()
        scope = capture.Scpi.__new__(capture.Scpi)
        scope.connection = client
        client.settimeout(2)

        def instrument():
            with server:
                with server.makefile("rb") as commands:
                    self.assertEqual(commands.readline(), b"WAVE?\n")
                    for part in [b"\n#", b"1", b"4", b"\x00\n", b"\xff#\n"]:
                        server.sendall(part)
                    self.assertEqual(commands.readline(), b"STATE?\n")
                    server.sendall(b"Stop\n")

        thread = threading.Thread(target=instrument)
        thread.start()
        try:
            self.assertEqual(scope.binary("WAVE?"), b"\x00\n\xff#")
            self.assertEqual(scope.query("STATE?"), "Stop")
        finally:
            scope.close()
            thread.join(timeout=2)
        self.assertFalse(thread.is_alive())

    def test_closed_socket_is_an_error_not_a_partial_waveform(self):
        client, server = socket.socketpair()
        scope = capture.Scpi.__new__(capture.Scpi)
        scope.connection = client
        server.sendall(b"#15ab")
        server.shutdown(socket.SHUT_WR)
        try:
            with self.assertRaises(EOFError):
                scope.binary("WAVE?")
        finally:
            scope.close()
            server.close()


if __name__ == "__main__":
    unittest.main()
