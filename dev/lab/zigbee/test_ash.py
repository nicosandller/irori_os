"""Vectors copied from zigbee-herdsman 10.9.2's ASH comments."""

import os
import pty
import select
import threading
import tty
import unittest

from ash import CANCEL, RST, crc_bytes, encode_frame, randomize, rstack, unescape
from ncp import Ncp

# EZSP "version" command 00 00 00 02, DATA(2, 5, 0), with the pseudo-random sequence.
VERSION_DATA = bytes.fromhex("25 42 21 A8 56 A6 09 7E")
# The same frame with randomization off.
VERSION_PLAIN = bytes.fromhex("25 00 00 00 02 1A AD 7E")


class AshVectors(unittest.TestCase):
    def test_randomize_matches_the_comment(self) -> None:
        self.assertEqual(randomize(bytes.fromhex("00 00 00 02")).hex(), "4221a856")

    def test_randomized_version_frame(self) -> None:
        frame = encode_frame(0x25, bytes.fromhex("00 00 00 02"), randomized=True)
        self.assertEqual(frame, VERSION_DATA)
        self.assertEqual(unescape(frame), bytes.fromhex("25 42 21 A8 56"))

    def test_plain_version_frame(self) -> None:
        # The comment's `1A` is the raw CRC high byte. On the wire it is stuffed,
        # because 0x1A is ASH CANCEL.
        frame = encode_frame(0x25, bytes.fromhex("00 00 00 02"), randomized=False)
        self.assertEqual(unescape(frame), bytes.fromhex("25 00 00 00 02"))
        self.assertIn(bytes.fromhex("7d3a"), frame)

    def test_crc_of_the_plain_control_and_data(self) -> None:
        self.assertEqual(crc_bytes(bytes.fromhex("25 00 00 00 02")), 0x1AAD)

    def test_rstack_is_version_2_software_reset(self) -> None:
        body = unescape(rstack())
        self.assertIsNotNone(body)
        assert body is not None
        self.assertEqual(body[0], 0xC1)
        self.assertEqual(body[1:], bytes((2, 0x0B)))


class NcpReset(unittest.TestCase):
    def test_rst_is_answered_with_rstack(self) -> None:
        master, slave = pty.openpty()
        tty.setraw(slave)
        threading.Thread(target=Ncp(master).serve, daemon=True).start()
        os.write(slave, bytes((CANCEL,)) + encode_frame(RST))
        ready, _, _ = select.select([slave], [], [], 2)
        self.assertTrue(ready, "no RSTACK")
        got = os.read(slave, 64)
        self.assertEqual(unescape(got), bytes((0xC1, 2, 0x0B)))


if __name__ == "__main__":
    unittest.main()
