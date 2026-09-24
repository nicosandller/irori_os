"""ASH, the serial framing Zigbee2MQTT's ember driver speaks (Silicon Labs UG101).

The CRC and the pseudo-random sequence match zigbee-herdsman 10.9.2, which is the
herdsman inside Zigbee2MQTT 2.14.1. A known frame from that driver's comments is
the test vector.
"""

from __future__ import annotations

FLAG = 0x7E
ESCAPE = 0x7D
XON = 0x11
XOFF = 0x13
SUBSTITUTE = 0x18
CANCEL = 0x1A
FLIP = 0x20

RST = 0xC0
RSTACK = 0xC1
ERROR = 0xC2
DATA = 0x00
ACK = 0x80
NAK = 0xA0

ASH_VERSION = 2
RESET_SOFTWARE = 0x0B
LFSR_POLY = 0xB8
LFSR_SEED = 0x42

RESERVED = {FLAG, ESCAPE, XON, XOFF, SUBSTITUTE, CANCEL}


def crc16(byte: int, prev: int) -> int:
    """halCommonCrc16 from zigbee-herdsman, one byte at a time. prev starts at 0xFFFF."""
    prev &= 0xFFFF
    prev = ((prev >> 8) | ((prev << 8) & 0xFFFF)) & 0xFFFF
    prev = (prev ^ byte) & 0xFFFF
    prev = (prev ^ ((prev & 0xFF) >> 4)) & 0xFFFF
    prev = (prev ^ ((((prev << 8) & 0xFFFF) << 4) & 0xFFFF)) & 0xFFFF
    prev = (
        prev
        ^ (
            (((prev & 0xFF) << 5) & 0xFF)
            | (((((prev & 0xFF) >> 3) & 0xFFFF) << 8) & 0xFFFF)
        )
    ) & 0xFFFF
    return prev


def crc_bytes(data: bytes) -> int:
    value = 0xFFFF
    for byte in data:
        value = crc16(byte, value)
    return value


def randomize(data: bytes) -> bytes:
    seed = LFSR_SEED
    out = bytearray()
    for byte in data:
        out.append(byte ^ seed)
        seed = (seed >> 1) ^ LFSR_POLY if seed & 1 else seed >> 1
    return bytes(out)


def stuff(byte: int) -> bytes:
    if byte in RESERVED:
        return bytes((ESCAPE, byte ^ FLIP))
    return bytes((byte,))


def encode_frame(control: int, data: bytes = b"", *, randomized: bool = False) -> bytes:
    """One ASH frame, ending in FLAG. `data` is the data field before randomization."""
    payload = randomize(data) if randomized else data
    raw = bytes((control,)) + payload
    crc = crc_bytes(raw)
    body = raw + bytes((crc >> 8, crc & 0xFF))
    frame = bytearray()
    for byte in body:
        frame.extend(stuff(byte))
    frame.append(FLAG)
    return bytes(frame)


def rstack() -> bytes:
    """The NCP has reset in software and speaks ASH version 2. Not randomized."""
    return encode_frame(RSTACK, bytes((ASH_VERSION, RESET_SOFTWARE)))


def unescape(frame_with_flag: bytes) -> bytes | None:
    """Control + data of one frame, or None if the CRC does not match."""
    body = bytearray()
    esc = False
    for byte in frame_with_flag:
        if byte == FLAG:
            break
        if byte == ESCAPE:
            esc = True
            continue
        if esc:
            byte ^= FLIP
            esc = False
        body.append(byte)
    if len(body) < 3:
        return None
    data, crc_bytes_ = bytes(body[:-2]), body[-2:]
    crc_got = (crc_bytes_[0] << 8) | crc_bytes_[1]
    if crc_bytes(data) != crc_got:
        return None
    return data
