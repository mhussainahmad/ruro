"""Prints reference packets from the vendor Python SDKs.

The byte sequences in the Dynamixel and Feetech unit tests were produced with
this script, so the Rust encoders are checked against the vendors' own code:

    python -m venv venv && venv/bin/pip install dynamixel-sdk feetech-servo-sdk
    venv/bin/python nodes/ruro-nodes-hw/tools/servo_fixtures.py
"""

import dynamixel_sdk as dxl
import scservo_sdk as scs


class CapturePort:
    """A port handler stand-in that records what the SDK transmits."""

    def __init__(self):
        self.tx = []
        self.is_using = False
        self.baudrate = 1_000_000

    def clearPort(self):
        pass

    def writePort(self, packet):
        self.tx.append(bytes(packet))
        return len(packet)

    def readPort(self, length):
        return []

    def setPacketTimeout(self, *args):
        pass

    def setPacketTimeoutMillis(self, *args):
        pass

    def isPacketTimeout(self):
        return True

    def getBaudRate(self):
        return self.baudrate


def show(name, port):
    for packet in port.tx:
        print(f"{name}: {packet.hex(' ')}")
    port.tx.clear()


def dynamixel():
    port = CapturePort()
    ph = dxl.PacketHandler(2.0)
    ph.ping(port, 1)
    show("dxl ping id1", port)
    ph.read4ByteTxRx(port, 1, 132)
    show("dxl read present_position id1", port)
    ph.write4ByteTxOnly(port, 1, 116, 512)
    show("dxl write goal_position=512 id1", port)
    ph.write1ByteTxOnly(port, 1, 64, 1)
    show("dxl write torque_enable=1 id1", port)
    # Header-like payload, exercises byte stuffing.
    ph.writeTxOnly(port, 1, 116, 4, [0xFF, 0xFF, 0xFD, 0x00])
    show("dxl write stuffed id1", port)

    sw = dxl.GroupSyncWrite(port, ph, 116, 4)
    sw.addParam(1, [0x96, 0x00, 0x00, 0x00])
    sw.addParam(2, [0xAA, 0x00, 0x00, 0x00])
    sw.txPacket()
    show("dxl sync_write goal_position id1=150 id2=170", port)

    sr = dxl.GroupSyncRead(port, ph, 132, 4)
    sr.addParam(1)
    sr.addParam(2)
    sr.txPacket()
    show("dxl sync_read present_position id1,id2", port)

    # Status packet CRC, as a servo would send it (read reply, position 0x000001A6).
    status = [0xFF, 0xFF, 0xFD, 0x00, 0x01, 0x08, 0x00, 0x55, 0x00, 0xA6, 0x01, 0x00, 0x00]
    crc = ph.updateCRC(0, status, len(status))
    print(f"dxl status crc: {crc:04x}")


def feetech():
    port = CapturePort()
    ph = scs.PacketHandler(0)  # STS series: little-endian (SCS_END = 0)
    ph.ping(port, 1)
    show("sts ping id1", port)
    ph.read2ByteTxRx(port, 1, 56)
    show("sts read present_position id1", port)
    ph.write2ByteTxOnly(port, 1, 42, 2048)
    show("sts write goal_position=2048 id1", port)
    ph.write1ByteTxOnly(port, 1, 40, 1)
    show("sts write torque_enable=1 id1", port)

    sw = scs.GroupSyncWrite(port, ph, 42, 2)
    for i, pos in enumerate([2048, 1024, 3072, 0, 4095, 100], start=1):
        sw.addParam(i, [scs.SCS_LOBYTE(pos), scs.SCS_HIBYTE(pos)])
    sw.txPacket()
    show("sts sync_write goal_position ids1-6", port)

    sr = scs.GroupSyncRead(port, ph, 56, 2)
    for i in range(1, 7):
        sr.addParam(i)
    sr.txPacket()
    show("sts sync_read present_position ids1-6", port)

    print(f"sts signed -100 -> {scs.SCS_TOSCS(-100, 15):04x}")
    print(f"sts signed 0x8064 -> {scs.SCS_TOHOST(0x8064, 15)}")


if __name__ == "__main__":
    dynamixel()
    feetech()
