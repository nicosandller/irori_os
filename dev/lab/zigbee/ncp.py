"""A lab Ember coordinator on a PTY.

Speaks ASH the way zigbee-herdsman 10.9.2 does, answers the EZSP `version`
command with protocol 0x13, and replies to the coordinator's own ZDO queries
so Zigbee2MQTT 2.14.1 can finish starting. Opening the network
(`PERMIT_JOINING`) announces the devices in `catalog.toml`; their interviews
are answered here too. Nothing here is linked into Irori or the Zigbee
extension: Zigbee2MQTT opens `/dev/zigbee0` and this process is what's on the
other end.
"""

from __future__ import annotations

import json
import os
import pty
import select
import struct
import sys
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path

from ash import CANCEL, DATA, RST, crc_bytes, encode_frame, randomize, rstack

EZSP_PROTOCOL_VERSION = 0x13
EZSP_STACK_TYPE_MESH = 0x02
# EmberZNet 8.2.0. herdsman 10.9 reads this as a little-endian uint16.
STACK_VERSION = 0x0802
VERSION_FRAME_ID = 0x00
EXTENDED_FORMAT = 0x01
RESPONSE = 0x80
ASYNC_CALLBACK = 0x90  # response direction plus the async-callback flag

# Frame ids herdsman 10.9.2 sends while Zigbee2MQTT is starting.
GET_VALUE = 0x00AA
GET_EXTENDED_VALUE = 0x0003
GET_CONFIGURATION_VALUE = 0x0052
GET_NETWORK_PARAMETERS = 0x0028
GET_EUI64 = 0x0026
GET_NODE_ID = 0x0027
NETWORK_INIT = 0x0017
NETWORK_STATE = 0x0018
FORM_NETWORK = 0x001E
SET_SOURCE_ROUTE_DISCOVERY_MODE = 0x005A
GET_NETWORK_KEY_INFO = 0x0116
STACK_STATUS_HANDLER = 0x0019
SEND_UNICAST = 0x0034
SEND_BROADCAST = 0x0036
INCOMING_MESSAGE_HANDLER = 0x0045
PERMIT_JOINING = 0x0022
TRUST_CENTER_JOIN_HANDLER = 0x0024
# Status-only commands herdsman 10.9.2 sends while starting and while joining is
# open. Each one's response is an SLStatus, and a failure aborts that step.
SET_CONCENTRATOR = 0x0010
SET_MANUFACTURER_CODE = 0x0015
SET_CONFIGURATION_VALUE = 0x0053
SET_POLICY = 0x0055
SET_MULTICAST_TABLE_ENTRY = 0x0064
SET_INITIAL_SECURITY_STATE = 0x0068
SET_VALUE = 0x00AB
CLEAR_KEY_TABLE = 0x00B1
IMPORT_TRANSIENT_KEY = 0x0111
EXPORT_KEY = 0x0114
STATUS_ONLY = {
    SET_CONCENTRATOR,
    SET_MANUFACTURER_CODE,
    PERMIT_JOINING,
    SET_CONFIGURATION_VALUE,
    SET_POLICY,
    SET_MULTICAST_TABLE_ENTRY,
    SET_INITIAL_SECURITY_STATE,
    SET_VALUE,
    CLEAR_KEY_TABLE,
    IMPORT_TRANSIENT_KEY,
    EXPORT_KEY,
}

# ZDO clusters Zigbee2MQTT asks the coordinator while herdsman is starting.
# The response id is the request id with the high bit set.
ZDO_PROFILE = 0x0000
ZDO_NWK_ADDR = 0x0000
ZDO_IEEE_ADDR = 0x0001
ZDO_NODE_DESC = 0x0002
ZDO_POWER_DESC = 0x0003
ZDO_SIMPLE_DESC = 0x0004
ZDO_ACTIVE_EP = 0x0005
ZDO_MATCH_DESC = 0x0006
ZDO_BIND = 0x0021
ZDO_UNBIND = 0x0022
ZDO_LEAVE = 0x0034
ZDO_PERMIT_JOIN = 0x0036
ZDO_SUCCESS = 0x00
ZDO_NOT_FOUND = 0x81
ZDO_NOT_ACTIVE = 0x83
ZDO_NO_MATCH = 0x86
HA_PROFILE = 0x0104
COORDINATOR_ENDPOINT = 1

SL_OK = 0x0000
# sl_status_t. An unimplemented command must not look like it succeeded.
SL_NOT_SUPPORTED = 0x000F
SL_NOT_JOINED = 0x0017
SL_NETWORK_UP = 0x0015
# EmberDeviceUpdate.STANDARD_SECURITY_UNSECURED_JOIN, EmberJoinDecision.USE_PRECONFIGURED_KEY.
UNSECURED_JOIN = 1
USE_PRECONFIGURED_KEY = 0
# A router manufacturer that is not 0 or 4098 (the Tuya fast path) and not an end device,
# so interview does not sleep for 10 seconds before reading Basic.
DEVICE_MANUFACTURER = 0x1286
COORDINATOR_MANUFACTURER = 0x0211
# ZCL global commands. A response uses direction server-to-client and disables the default response.
ZCL_READ = 0x00
ZCL_READ_RSP = 0x01
ZCL_DEFAULT_RSP = 0x0B
ZCL_FRAME_RESPONSE = 0x18
ZCL_SUCCESS = 0x00
ZCL_UNSUPPORTED_ATTRIBUTE = 0x86
ZCL_UINT8 = 0x20
ZCL_ENUM8 = 0x30
ZCL_STRING = 0x42
GEN_BASIC = 0x0000
# STACK_STATUS has to wait until the host has registered for it. ZDO and ZCL waiters are
# registered as soon as the send returns, so those callbacks can follow immediately.
STATUS_DELAY = 0.4
MESSAGE_DELAY = 0.05
# EZSP permit-joining and the ZDO broadcast that follows it must not announce twice.
ANNOUNCE_GAP = 2.0
# EmberVersionType.GA. Anything else is logged as a pre-release.
VERSION_TYPE_GA = 0xAA
NODE_COORDINATOR = 1
VALUE_VERSION_INFO = 0x11

# A fixed coordinator identity. The lab network is formed fresh each install.
COORDINATOR_EUI = bytes.fromhex("02110000000000ff")
NETWORK_PARAMS = (
    bytes(8)  # extended PAN id, all zeroes until a real network is formed
    + struct.pack("<H", 0x1A2B)  # pan id
    + bytes((5, 11, 0))  # tx power, channel, join method
    + struct.pack("<H", 0)  # network manager
    + bytes((0,))  # nwk update id
    + struct.pack("<I", 0x07FFF800)  # all 2.4 GHz channels
)


def u32(value: int) -> bytes:
    return struct.pack("<I", value)


def u16(value: int) -> bytes:
    return struct.pack("<H", value)


def version_struct() -> bytes:
    """EmberVersion: build, major, minor, patch, special, type. 8.2.0 GA."""
    return struct.pack("<HBBBBB", 0, 8, 2, 0, 0, VERSION_TYPE_GA)


@dataclass(frozen=True)
class LabDevice:
    nwk: int
    wire: bytes
    model: str
    manufacturer: str


def wire_eui(ieee: str) -> bytes | None:
    """Little-endian EUI64. Display `0x0211000000000001` is wire `01 00 00 00 00 00 11 02`."""
    text = ieee.strip().lower().removeprefix("0x")
    if len(text) != 16:
        return None
    try:
        return bytes.fromhex(text)[::-1]
    except ValueError:
        return None


def load_devices(path: Path) -> list[LabDevice]:
    """Catalog rows become network addresses 1, 2, ... The coordinator keeps address 0."""
    if not path.is_file():
        return []
    rows = tomllib.loads(path.read_text()).get("zigbee", [])
    devices: list[LabDevice] = []
    for row in rows:
        wire = wire_eui(str(row.get("ieee", "")))
        if wire is None:
            continue
        model = str(row.get("zigbee_model") or "ZBMINIL2")
        manufacturer = str(row.get("manufacturer_name") or "SONOFF")
        devices.append(LabDevice(len(devices) + 1, wire, model, manufacturer))
    return devices


def trust_center_join(device: LabDevice) -> bytes:
    """TRUST_CENTER_JOIN_HANDLER: nwk, EUI64, unsecured join, preconfigured key, parent 0."""
    return (
        struct.pack("<H", device.nwk)
        + device.wire
        + bytes((UNSECURED_JOIN, USE_PRECONFIGURED_KEY))
        + struct.pack("<H", 0)
    )


def scheduled_joins(devices: list[LabDevice], now: float) -> list[tuple[float, int, bytes]]:
    """Stagger joins so they are not one frame. Interviews keep going after the permit window."""
    return [
        (now + 0.3 + index * 0.25, TRUST_CENTER_JOIN_HANDLER, trust_center_join(device))
        for index, device in enumerate(devices)
    ]


def _by_nwk(devices: list[LabDevice] | None, nwk: int) -> LabDevice | None:
    if not devices:
        return None
    for device in devices:
        if device.nwk == nwk:
            return device
    return None


def _by_wire(devices: list[LabDevice] | None, wire: bytes) -> LabDevice | None:
    if not devices:
        return None
    for device in devices:
        if device.wire == wire:
            return device
    return None


def _aps(params: bytes, offset: int) -> tuple[int, int, int, int, int]:
    """Return (profile, cluster, source endpoint, destination endpoint, offset after the frame)."""
    profile, cluster, src_ep, dst_ep = struct.unpack_from("<HHBB", params, offset)
    return profile, cluster, src_ep, dst_ep, offset + 11


def _node_descriptor(nwk: int, logical_type: int, manufacturer: int, seq: int) -> bytes:
    # logical type in the low 3 bits, 2.4 GHz, mains-powered, no TLVs.
    body = (
        bytes((ZDO_SUCCESS,))
        + struct.pack("<H", nwk)
        + bytes((logical_type & 0x07, 0x40, 0x8E))
        + struct.pack("<H", manufacturer)
        + bytes((82,))
        + struct.pack("<HHH", 82, 0, 82)
        + bytes((0,))
    )
    return bytes((seq,)) + body


def _simple_descriptor(nwk: int, endpoint: int, in_clusters: tuple[int, ...], seq: int) -> bytes:
    descriptor = struct.pack("<BHHBB", endpoint, HA_PROFILE, 0, 1, len(in_clusters))
    for cluster_id in in_clusters:
        descriptor += struct.pack("<H", cluster_id)
    descriptor += bytes((0,))  # no output clusters
    return bytes((seq, ZDO_SUCCESS)) + struct.pack("<HB", nwk, len(descriptor)) + descriptor


def _zdo_body(cluster: int, payload: bytes, dest: int, devices: list[LabDevice] | None = None) -> bytes | None:
    """Response parameters for a ZDO request, or None when this cluster is not answered yet.

    herdsman skips the first byte (the transaction sequence) and then reads the
    fields below. A status other than success stops the reader, which is how an
    unknown device is reported without inventing a descriptor.
    """
    seq = payload[0] if payload else 0
    nwk = struct.unpack_from("<H", payload, 1)[0] if len(payload) >= 3 else dest

    def framed(body: bytes) -> bytes:
        return bytes((seq,)) + body

    if cluster == ZDO_ACTIVE_EP:
        if nwk != 0 and _by_nwk(devices, nwk) is None:
            return framed(bytes((ZDO_NOT_FOUND,)))
        return framed(bytes((ZDO_SUCCESS,)) + struct.pack("<H", nwk) + bytes((1, COORDINATOR_ENDPOINT)))
    if cluster == ZDO_NODE_DESC:
        if nwk == 0:
            return _node_descriptor(0, 0, COORDINATOR_MANUFACTURER, seq)
        if _by_nwk(devices, nwk) is None:
            return framed(bytes((ZDO_NOT_FOUND,)))
        # Router, so interview does not take the end-device paths.
        return _node_descriptor(nwk, 1, DEVICE_MANUFACTURER, seq)
    if cluster == ZDO_POWER_DESC:
        if nwk != 0 and _by_nwk(devices, nwk) is None:
            return framed(bytes((ZDO_NOT_FOUND,)))
        return framed(bytes((ZDO_SUCCESS,)) + struct.pack("<H", nwk) + bytes((0x10, 0x10)))
    if cluster == ZDO_SIMPLE_DESC:
        endpoint = payload[3] if len(payload) > 3 else 0
        if nwk == 0:
            if endpoint != COORDINATOR_ENDPOINT:
                return framed(bytes((ZDO_NOT_ACTIVE,)))
            # endpoint, HA profile, device id, version, no clusters in or out.
            descriptor = struct.pack("<BHHBB", COORDINATOR_ENDPOINT, HA_PROFILE, 0x0065, 1, 0) + bytes((0,))
            return framed(bytes((ZDO_SUCCESS,)) + struct.pack("<HB", nwk, len(descriptor)) + descriptor)
        if _by_nwk(devices, nwk) is None:
            return framed(bytes((ZDO_NOT_FOUND,)))
        if endpoint != COORDINATOR_ENDPOINT:
            return framed(bytes((ZDO_NOT_ACTIVE,)))
        # genBasic only. IAS Zone and Poll Control make interview try to enroll or bind.
        return _simple_descriptor(nwk, COORDINATOR_ENDPOINT, (GEN_BASIC,), seq)
    if cluster == ZDO_IEEE_ADDR:
        if nwk == 0:
            return framed(bytes((ZDO_SUCCESS,)) + COORDINATOR_EUI + struct.pack("<H", 0))
        device = _by_nwk(devices, nwk)
        if device is None:
            return framed(bytes((ZDO_NOT_FOUND,)))
        return framed(bytes((ZDO_SUCCESS,)) + device.wire + struct.pack("<H", device.nwk))
    if cluster == ZDO_NWK_ADDR:
        eui = payload[1:9]
        if eui == COORDINATOR_EUI:
            return framed(bytes((ZDO_SUCCESS,)) + COORDINATOR_EUI + struct.pack("<H", 0))
        device = _by_wire(devices, eui)
        if device is None:
            return framed(bytes((ZDO_NOT_FOUND,)))
        return framed(bytes((ZDO_SUCCESS,)) + device.wire + struct.pack("<H", device.nwk))
    if cluster == ZDO_MATCH_DESC:
        return framed(bytes((ZDO_NO_MATCH,)))
    if cluster in (ZDO_BIND, ZDO_UNBIND, ZDO_LEAVE, ZDO_PERMIT_JOIN):
        return framed(bytes((ZDO_SUCCESS,)))
    return None


def _zcl_string(text: str) -> bytes:
    raw = text.encode("utf-8")[:32]
    return bytes((ZCL_STRING, len(raw))) + raw


def _zcl_reply(cluster: int, payload: bytes, device: LabDevice) -> bytes | None:
    """Answer a Basic read. Anything else gets a default response so the waiter does not time out."""
    if len(payload) < 3:
        return None
    frame_control = payload[0]
    if frame_control & 0x04:
        if len(payload) < 5:
            return None
        seq = payload[3]
        command = payload[4]
        body = payload[5:]
    else:
        seq = payload[1]
        command = payload[2]
        body = payload[3:]
    header = bytes((ZCL_FRAME_RESPONSE, seq))
    if cluster == GEN_BASIC and command == ZCL_READ:
        parts = bytearray()
        values = {0x0000: 8, 0x0001: 1, 0x0002: 1, 0x0003: 1}
        for index in range(0, len(body) - 1, 2):
            attr = struct.unpack_from("<H", body, index)[0]
            parts += struct.pack("<H", attr)
            if attr == 0x0004:
                parts += bytes((ZCL_SUCCESS,)) + _zcl_string(device.manufacturer)
            elif attr == 0x0005:
                parts += bytes((ZCL_SUCCESS,)) + _zcl_string(device.model)
            elif attr == 0x0006:
                parts += bytes((ZCL_SUCCESS,)) + _zcl_string("20260101")
            elif attr == 0x4000:
                parts += bytes((ZCL_SUCCESS,)) + _zcl_string("1.0.0")
            elif attr in values:
                parts += bytes((ZCL_SUCCESS, ZCL_UINT8, values[attr]))
            elif attr == 0x0007:
                parts += bytes((ZCL_SUCCESS, ZCL_ENUM8, 1))
            else:
                parts += bytes((ZCL_UNSUPPORTED_ATTRIBUTE,))
        return header + bytes((ZCL_READ_RSP,)) + bytes(parts)
    return header + bytes((ZCL_DEFAULT_RSP, command, ZCL_SUCCESS))


def _incoming(
    cluster: int,
    sender: int,
    aps_sequence: int,
    payload: bytes,
    *,
    profile: int = ZDO_PROFILE,
    src_ep: int = 0,
    dst_ep: int = 0,
    sender_eui: bytes | None = None,
) -> bytes:
    """INCOMING_MESSAGE_HANDLER parameters for EZSP protocol 0x13."""
    eui = COORDINATOR_EUI if sender_eui is None else sender_eui
    aps = struct.pack("<HHBBHHB", profile, cluster, src_ep, dst_ep, 0, 0, aps_sequence & 0xFF)
    packet = struct.pack("<H", sender) + eui + bytes((0xFF, 0xFF, 0xFF, 0)) + struct.pack("<I", 0)
    return bytes((0,)) + aps + packet + bytes((len(payload),)) + payload


def outgoing(
    frame_id: int,
    params: bytes,
    aps_sequence: int,
    devices: list[LabDevice] | None = None,
) -> tuple[bytes, list[tuple[int, bytes]]] | None:
    """SEND_UNICAST / SEND_BROADCAST response, plus a ZDO callback when we can build one.

    Returns None for every other frame id. The response is a status and the APS
    sequence herdsman stores on the request before it starts waiting.
    """
    if frame_id == SEND_UNICAST:
        if len(params) < 17:
            return u32(SL_OK) + bytes((aps_sequence & 0xFF,)), []
        dest = struct.unpack_from("<H", params, 1)[0]
        profile, cluster, src_ep, dst_ep, offset = _aps(params, 3)
        # message tag is a uint16 at `offset`, then a length byte and the ZDO payload.
        length_at = offset + 2
    elif frame_id == SEND_BROADCAST:
        if len(params) < 20:
            return u32(SL_OK) + bytes((aps_sequence & 0xFF,)), []
        dest = struct.unpack_from("<H", params, 2)[0]
        profile, cluster, src_ep, dst_ep, offset = _aps(params, 5)
        # radius byte, then the uint16 message tag.
        length_at = offset + 1 + 2
    else:
        return None
    length = params[length_at] if len(params) > length_at else 0
    payload = params[length_at + 1 : length_at + 1 + length]
    callbacks: list[tuple[int, bytes]] = []
    if profile == ZDO_PROFILE and cluster < 0x8000:
        body = _zdo_body(cluster, payload, dest, devices)
        if body is not None:
            callbacks.append(
                (INCOMING_MESSAGE_HANDLER, _incoming(cluster | 0x8000, dest, aps_sequence, body))
            )
    elif profile == HA_PROFILE:
        device = _by_nwk(devices, dest)
        if device is not None:
            reply = _zcl_reply(cluster, payload, device)
            if reply is not None:
                # The waiter matches the request's destination endpoint against this source endpoint.
                callbacks.append(
                    (
                        INCOMING_MESSAGE_HANDLER,
                        _incoming(
                            cluster,
                            dest,
                            aps_sequence,
                            reply,
                            profile=HA_PROFILE,
                            src_ep=dst_ep,
                            dst_ep=src_ep,
                            sender_eui=device.wire,
                        ),
                    )
                )
    return u32(SL_OK) + bytes((aps_sequence & 0xFF,)), callbacks


def answer(frame_id: int, params: bytes) -> tuple[bytes, int | None]:
    """EZSP response body, and a stack-status callback to send once the host is waiting.

    The callback value is an SLStatus, or None. Most startup commands only want a
    success status. The ones that read further have an explicit body.
    """
    if frame_id == GET_VALUE:
        value_id = params[0] if params else 0
        if value_id == VALUE_VERSION_INFO:
            body = version_struct()
            return u32(SL_OK) + bytes((len(body),)) + body, None
        return u32(SL_OK) + bytes((0,)), None
    if frame_id == GET_EXTENDED_VALUE:
        return u32(SL_OK) + bytes((2, 0, 0)), None
    if frame_id == GET_CONFIGURATION_VALUE:
        return u32(SL_OK) + u16(0), None
    if frame_id == GET_EUI64:
        return COORDINATOR_EUI, None
    if frame_id == GET_NODE_ID:
        return u16(0), None
    if frame_id == GET_NETWORK_PARAMETERS:
        return u32(SL_OK) + bytes((NODE_COORDINATOR,)) + NETWORK_PARAMS, None
    if frame_id == NETWORK_STATE:
        return bytes((2,)), None  # JOINED_NETWORK
    if frame_id == NETWORK_INIT:
        # No network yet, so Zigbee2MQTT forms one from its settings.
        return u32(SL_NOT_JOINED), None
    if frame_id == SET_SOURCE_ROUTE_DISCOVERY_MODE:
        return u32(0), None
    if frame_id == FORM_NETWORK:
        return u32(SL_OK), SL_NETWORK_UP
    if frame_id == GET_NETWORK_KEY_INFO:
        # network key is set, sequence 0, frame counter 1. The key bytes themselves
        # stay inside the NCP; this command only reports that metadata.
        return u32(SL_OK) + bytes((1, 0, 0, 0)) + u32(1), None
    if frame_id in STATUS_ONLY:
        return u32(SL_OK), None
    print(f"lab zigbee: EZSP {frame_id:#06x} not handled yet", flush=True)
    return u32(SL_NOT_SUPPORTED), None


class Ncp:
    def __init__(self, master: int, devices: list[LabDevice] | tuple[LabDevice, ...] = ()) -> None:
        self.master = master
        self.devices = list(devices)
        self.frm_rx = 0
        self.frm_tx = 0
        self.connected = False
        self.pending = bytearray()
        self.aps_sequence = 0
        self.last_announce = 0.0
        # (when, frame id, body) callbacks, sent after the host has registered its waiter.
        self.later: list[tuple[float, int, bytes]] = []

    def _open_network(self, duration: int) -> None:
        if duration == 0 or not self.devices:
            return
        now = time.monotonic()
        if now - self.last_announce < ANNOUNCE_GAP:
            print("lab zigbee: permit joining already announced", flush=True)
            return
        self.last_announce = now
        self.later.extend(scheduled_joins(self.devices, now))
        print(
            f"lab zigbee: permit joining for {duration}s, announcing {len(self.devices)} devices",
            flush=True,
        )

    def serve(self) -> None:
        while True:
            self._send_due()
            ready, _, _ = select.select([self.master], [], [], 0.05)
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 4096)
            except OSError:
                return
            if not chunk:
                return
            self._chunk(chunk)

    def _send_due(self) -> None:
        now = time.monotonic()
        due = [item for item in self.later if item[0] <= now]
        self.later = [item for item in self.later if item[0] > now]
        for _, frame_id, body in due:
            self._send(self._callback(frame_id, body))
            print(f"lab zigbee: callback {frame_id:#06x}", flush=True)

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
        # The first version command is the legacy 3-byte header. Everything after
        # the host learns the protocol version uses the extended header.
        if len(frame) >= 4 and frame[2] == VERSION_FRAME_ID and (frame[1] & RESPONSE) == 0:
            desired = frame[3]
            print(
                f"lab zigbee: EZSP version, host asked for {desired}, answering {EZSP_PROTOCOL_VERSION}",
                flush=True,
            )
            payload = bytes(
                (
                    sequence,
                    RESPONSE,
                    VERSION_FRAME_ID,
                    EZSP_PROTOCOL_VERSION,
                    EZSP_STACK_TYPE_MESH,
                    STACK_VERSION & 0xFF,
                    (STACK_VERSION >> 8) & 0xFF,
                )
            )
            self._send(payload)
            return
        if len(frame) >= 5 and frame[2] == EXTENDED_FORMAT:
            frame_id = frame[3] | (frame[4] << 8)
            params = frame[5:]
        else:
            frame_id = frame[2]
            params = frame[3:]
        self.aps_sequence = (self.aps_sequence + 1) & 0xFF
        if frame_id == PERMIT_JOINING:
            self._open_network(params[0] if params else 0)
        sent = outgoing(frame_id, params, self.aps_sequence, self.devices)
        if sent is None:
            body, status = answer(frame_id, params)
            callbacks = [(STACK_STATUS_HANDLER, u32(status))] if status is not None else []
        else:
            body, callbacks = sent
            for callback_id, callback_body in callbacks:
                if callback_id == INCOMING_MESSAGE_HANDLER and len(callback_body) >= 5:
                    replied = struct.unpack_from("<H", callback_body, 3)[0]
                    print(f"lab zigbee: ZDO response {replied:#06x}", flush=True)
        print(f"lab zigbee: EZSP {frame_id:#06x} -> {len(body)} bytes", flush=True)
        self._send(bytes((sequence, RESPONSE, EXTENDED_FORMAT, frame_id & 0xFF, (frame_id >> 8) & 0xFF)) + body)
        for callback_id, callback_body in callbacks:
            delay = STATUS_DELAY if callback_id == STACK_STATUS_HANDLER else MESSAGE_DELAY
            self.later.append((time.monotonic() + delay, callback_id, callback_body))

    def _callback(self, frame_id: int, body: bytes) -> bytes:
        return bytes((0, ASYNC_CALLBACK, EXTENDED_FORMAT, frame_id & 0xFF, (frame_id >> 8) & 0xFF)) + body

    def _send(self, payload: bytes) -> None:
        control = DATA | ((self.frm_tx & 0x07) << 4) | (self.frm_rx & 0x07)
        self._write(encode_frame(control, payload, randomized=True))
        self.frm_tx = (self.frm_tx + 1) & 7

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
    catalog = Path(__file__).resolve().parents[1] / "home" / "catalog.toml"
    devices = load_devices(catalog)
    print("lab zigbee: set the extension serial port to", link, flush=True)
    print("lab zigbee: zigbee2mqtt_version = 2.14.1", flush=True)
    print(f"lab zigbee: {len(devices)} catalog devices from {catalog}", flush=True)
    master = open_link(link)
    # Give udev-less /dev a moment so the symlink is visible before we block.
    time.sleep(0.05)
    try:
        Ncp(master, devices).serve()
    except KeyboardInterrupt:
        return


if __name__ == "__main__":
    main()
