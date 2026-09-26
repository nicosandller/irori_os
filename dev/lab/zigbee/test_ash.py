"""Vectors copied from zigbee-herdsman 10.9.2's ASH comments."""

import os
import pty
import select
import threading
import tty
import unittest

from ash import CANCEL, RST, crc_bytes, encode_frame, randomize, rstack, unescape
import struct

from ncp import (
    GET_VALUE,
    INCOMING_MESSAGE_HANDLER,
    Ncp,
    SEND_UNICAST,
    VALUE_VERSION_INFO,
    answer,
    outgoing,
    u32,
)

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


class EzspAnswers(unittest.TestCase):
    def test_version_info_is_seven_bytes(self) -> None:
        body, callback = answer(GET_VALUE, bytes((VALUE_VERSION_INFO,)))
        self.assertIsNone(callback)
        self.assertEqual(body[4], 7)
        self.assertEqual(len(body), 5 + 7)

    def test_active_endpoints_reply_names_the_coordinator_endpoint(self) -> None:
        # DIRECT to the coordinator, cluster Active_EP_req, transaction sequence 1.
        zdo = bytes((1, 0, 0))
        params = bytes((0,)) + struct.pack("<H", 0)
        params += struct.pack("<HHBBHHB", 0, 5, 0, 0, 4416, 0, 0)
        params += struct.pack("<H", 1) + bytes((len(zdo),)) + zdo
        body, callbacks = outgoing(SEND_UNICAST, params, 1)
        assert body is not None
        self.assertEqual(body, u32(0) + bytes((1,)))
        self.assertEqual(len(callbacks), 1)
        frame_id, incoming = callbacks[0]
        self.assertEqual(frame_id, INCOMING_MESSAGE_HANDLER)
        profile, cluster = struct.unpack_from("<HH", incoming, 1)
        self.assertEqual((profile, cluster), (0, 0x8005))
        sender = struct.unpack_from("<H", incoming, 12)[0]
        self.assertEqual(sender, 0)
        length_at = 1 + 11 + 18
        self.assertEqual(incoming[length_at], 6)
        # sequence, success, nwk 0, one endpoint, endpoint 1
        self.assertEqual(incoming[length_at + 1 :], bytes((1, 0, 0, 0, 1, 1)))


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
