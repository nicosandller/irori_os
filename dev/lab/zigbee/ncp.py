"""A lab Ember coordinator on a PTY.

Speaks ASH the way zigbee-herdsman 10.9.2 does, and answers the opening EZSP
`version` command with protocol 0x13 (the version that herdsman requires).
Later EZSP commands are logged with their frame id. Filling those in, one
failing herdsman call at a time, is how the 14-device network gets interviewed.
Nothing here is linked into Irori or the Zigbee extension: Zigbee2MQTT opens
`/dev/zigbee0` and this process is what's on the other end.
"""

from __future__ import annotations

import json
import os
import pty
import select
import sys
import time
from pathlib import Path

from ash import CANCEL, DATA, RST, crc_bytes, encode_frame, randomize, rstack

EZSP_PROTOCOL_VERSION = 0x13
EZSP_STACK_TYPE_MESH = 0x02
# EmberZNet 8.2.0, little-endian, a firmware generation herdsman 10.9 accepts.
STACK_VERSION = 0x0802
VERSION_FRAME_ID = 0x00
EXTENDED_FORMAT = 0x01


def version_response(sequence: int, frm: int, ack: int) -> bytes:
    """Legacy EZSP frame: seq, response control, frame id, protocol, stack type, stack version."""
    payload = bytes(
        (
            sequence,
            0x80,
            VERSION_FRAME_ID,
            EZSP_PROTOCOL_VERSION,
            EZSP_STACK_TYPE_MESH,
            STACK_VERSION & 0xFF,
            (STACK_VERSION >> 8) & 0xFF,
        )
    )
    # DATA(frm, ack). The data field is randomized; the control byte is not.
    control = DATA | ((frm & 0x07) << 4) | (ack & 0x07)
    return encode_frame(control, payload, randomized=True)


class Ncp:
    def __init__(self, master: int) -> None:
        self.master = master
        self.frm_rx = 0
        self.frm_tx = 0
        self.connected = False
        self.pending = bytearray()

    def serve(self) -> None:
        while True:
            ready, _, _ = select.select([self.master], [], [], 1.0)
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 4096)
            except OSError:
                return
            if not chunk:
                return
            self._chunk(chunk)

    def _chunk(self, chunk: bytes) -> None:
        # CANCEL sits in front of RST and is not itself a frame. RST is what we answer.
        for frame in self._frames(chunk):
            self._frame(frame)

    def _frames(self, chunk: bytes) -> list[bytes]:
        """Unescape FLAG-terminated frames, checking the CRC. Bytes after the last FLAG wait."""
        frames: list[bytes] = []
        self.pending.extend(chunk)
        body = bytearray()
        esc = False
        consumed = 0
        for index, byte in enumerate(self.pending):
            if byte == CANCEL:
                body.clear()
                esc = False
                continue
            if byte == 0x7E:
                if len(body) >= 3:
                    data, crc_hi, crc_lo = bytes(body[:-2]), body[-2], body[-1]
                    if crc_bytes(data) == ((crc_hi << 8) | crc_lo):
                        frames.append(data)
                body.clear()
                esc = False
                consumed = index + 1
                continue
            if byte == 0x7D:
                esc = True
                continue
            if esc:
                byte ^= 0x20
                esc = False
            body.append(byte)
        if consumed:
            del self.pending[:consumed]
        return frames

    def _frame(self, frame: bytes) -> None:
        control = frame[0]
        if control == RST:
            self.connected = True
            self.frm_rx = 0
            self.frm_tx = 0
            self._write(rstack())
            print("lab zigbee: RST, answered RSTACK", flush=True)
            return
        if control & 0x80:
            return
        frm = (control >> 4) & 0x07
        data = randomize(frame[1:])  # XOR again undoes the host's randomization
        self.frm_rx = (frm + 1) & 7
        self._ezsp(data)

    def _ezsp(self, frame: bytes) -> None:
        if len(frame) < 3:
            return
        sequence = frame[0]
        # Legacy version command: [seq, control, frame id 0, desired version]
        if len(frame) >= 4 and frame[2] == VERSION_FRAME_ID and frame[1] & 0x80 == 0:
            desired = frame[3]
            print(
                f"lab zigbee: EZSP version, host asked for {desired}, answering {EZSP_PROTOCOL_VERSION}",
                flush=True,
            )
            # The response is its own DATA frame. frm_tx on our side starts at 0.
            self._write(version_response(sequence, self.frm_tx, self.frm_rx))
            self.frm_tx = (self.frm_tx + 1) & 7
            return
        frame_id = frame[3] if len(frame) > 4 and frame[2] == EXTENDED_FORMAT else frame[2]
        print(
            f"lab zigbee: EZSP frame id {frame_id:#06x} ({len(frame)} bytes) not handled yet",
            flush=True,
        )

    def _write(self, frame: bytes) -> None:
        os.write(self.master, frame)


def open_link(path: str) -> int:
    master, slave = pty.openpty()
    slave_name = os.ttyname(slave)
    link = Path(path)
    if link.is_symlink() or link.exists():
        link.unlink()
    os.symlink(slave_name, path)
    uid = int(os.environ.get("IRORI_LAB_UID", "0"))
    gid = int(os.environ.get("IRORI_LAB_GID", "0"))
    if uid:
        os.chown(slave_name, uid, gid)
        os.chmod(slave_name, 0o660)
    print(f"lab zigbee: {path} -> {slave_name}", flush=True)
    return master


def main() -> None:
    link = sys.argv[1] if len(sys.argv) > 1 else "/dev/zigbee0"
    state = Path(sys.argv[2]) if len(sys.argv) > 2 else Path("/var/lib/irori/lab/zigbee.json")
    state.parent.mkdir(parents=True, exist_ok=True)
    if not state.exists():
        state.write_text(json.dumps({"joined": [], "formed": None}))
    print("lab zigbee: set the extension serial port to", link, flush=True)
    print("lab zigbee: zigbee2mqtt_version = 2.14.1", flush=True)
    master = open_link(link)
    # Give udev-less /dev a moment so the symlink is visible before we block.
    time.sleep(0.05)
    try:
        Ncp(master).serve()
    except KeyboardInterrupt:
        return


if __name__ == "__main__":
    main()
