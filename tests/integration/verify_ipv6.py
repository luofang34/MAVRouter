#!/usr/bin/env python3
"""
IPv6 UDP Support Test

Verifies that mavrouter-rs correctly handles IPv6 UDP endpoints:
1. Starts router with an IPv6 UDP server endpoint on [::1]:14555
2. Connects a TCP client (IPv4) to verify basic functionality
3. Sends MAVLink messages via UDP to the IPv6 endpoint
4. Verifies messages are routed between IPv6 UDP and IPv4 TCP endpoints

This test requires IPv6 loopback support on the system.
"""

import sys
import os
import time
import tempfile
import subprocess
import socket
import threading

from pymavlink import mavutil

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.join(SCRIPT_DIR, '..', '..')
BINARY = os.path.join(REPO_ROOT, 'target', 'release', 'mavrouter')


def ipv6_available():
    """Check if IPv6 loopback is available on this system."""
    try:
        sock = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
        sock.bind(('::1', 0))
        sock.close()
        return True
    except (socket.error, OSError):
        return False


def wait_for_tcp(host, port, timeout=10):
    start = time.time()
    while time.time() - start < timeout:
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.settimeout(1)
            sock.connect((host, port))
            sock.close()
            return True
        except (socket.error, socket.timeout):
            time.sleep(0.5)
    return False


def main():
    print("=" * 50)
    print("IPv6 UDP Support Test")
    print("=" * 50)

    # Check IPv6 availability
    if not ipv6_available():
        print("SKIP: IPv6 not available on this system")
        sys.exit(0)
    print("IPv6 loopback available")

    config_path = tempfile.mktemp(suffix='.toml')
    proc = None

    try:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        time.sleep(1)

        # Step 1: Create config with IPv6 UDP endpoint
        print("\n[1/6] Creating config with IPv6 UDP endpoint...")
        config = """
[general]
bus_capacity = 5000
tcp_port = 5765

[[endpoint]]
type = "udp"
address = "[::1]:14555"
mode = "server"
"""
        with open(config_path, 'w') as f:
            f.write(config)

        # Step 2: Start router
        print("\n[2/6] Starting router...")
        proc = subprocess.Popen(
            [BINARY, '--config', config_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT
        )
        if not wait_for_tcp('127.0.0.1', 5765, timeout=10):
            print("ERROR: Router failed to start")
            if proc:
                out = proc.stdout.read().decode('utf-8', errors='replace') if proc.stdout else ''
                print(f"Router output:\n{out[:2000]}")
                proc.kill()
            sys.exit(1)
        print(f"Router started (PID: {proc.pid})")

        # Step 3: Connect autopilot simulator via TCP (sysid=1)
        print("\n[3/6] Connecting autopilot via TCP (sysid=1)...")
        autopilot = mavutil.mavlink_connection('tcp:127.0.0.1:5765', source_system=1)
        # Send heartbeats to establish routing
        for _ in range(5):
            autopilot.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_QUADROTOR,
                mavutil.mavlink.MAV_AUTOPILOT_PX4,
                0, 0, 0
            )
            time.sleep(0.1)
        print("Autopilot connected and sending heartbeats")

        # Step 4: Send data via IPv6 UDP
        print("\n[4/6] Sending MAVLink data via IPv6 UDP...")
        # pymavlink's udpout with IPv6 might not work directly,
        # so we'll use raw socket + pymavlink message construction
        udp_sock = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
        udp_sock.settimeout(5)

        # Bind to a local IPv6 port for receiving
        udp_sock.bind(('::1', 0))
        local_port = udp_sock.getsockname()[1]
        print(f"UDP IPv6 client bound to [::1]:{local_port}")

        # Create a MAVLink instance for encoding
        from pymavlink.dialects.v20 import common as mavlink2

        class ByteCapture:
            def __init__(self):
                self.data = bytearray()
            def write(self, buf):
                self.data.extend(buf)

        cap = ByteCapture()
        mav = mavlink2.MAVLink(cap, srcSystem=254, srcComponent=1)

        # Send heartbeats via IPv6 UDP to the router
        for i in range(10):
            cap.data = bytearray()
            mav.heartbeat_send(
                mavlink2.MAV_TYPE_GCS,
                mavlink2.MAV_AUTOPILOT_INVALID,
                0, 0, 0
            )
            udp_sock.sendto(bytes(cap.data), ('::1', 14555))
            time.sleep(0.1)
        print("Sent 10 heartbeats via IPv6 UDP")

        # Step 5: Check that traffic was received by reading from autopilot TCP
        print("\n[5/6] Checking autopilot received GCS heartbeats...")
        time.sleep(1)

        received_from_gcs = False
        start = time.time()
        while time.time() - start < 5:
            msg = autopilot.recv_match(blocking=True, timeout=1)
            if msg and msg.get_type() == 'HEARTBEAT':
                src = msg.get_srcSystem()
                if src == 254:
                    print(f"SUCCESS: Autopilot received heartbeat from GCS (sysid={src}) via IPv6 UDP!")
                    received_from_gcs = True
                    break

        # Step 6: Check that IPv6 UDP endpoint receives traffic from TCP
        print("\n[6/6] Checking IPv6 UDP receives autopilot heartbeats...")
        # Send more heartbeats from autopilot
        for _ in range(5):
            autopilot.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_QUADROTOR,
                mavutil.mavlink.MAV_AUTOPILOT_PX4,
                0, 0, 0
            )
            time.sleep(0.1)
        time.sleep(0.5)

        received_on_ipv6 = False
        try:
            for _ in range(20):
                data, addr = udp_sock.recvfrom(65535)
                if len(data) > 0:
                    print(f"Received {len(data)} bytes on IPv6 UDP from {addr}")
                    received_on_ipv6 = True
                    break
        except socket.timeout:
            print("No data received on IPv6 UDP (timeout)")

        # Cleanup
        udp_sock.close()
        autopilot.close()
        proc.terminate()
        proc.wait(timeout=5)

        # Results
        success = True
        if not received_from_gcs:
            print("ERROR: TCP autopilot did NOT receive GCS messages from IPv6 UDP")
            success = False

        if not received_on_ipv6:
            print("ERROR: IPv6 UDP did NOT receive autopilot messages from TCP")
            success = False

        if success:
            print("\n" + "=" * 50)
            print("IPv6 UDP Support Test PASSED")
            print("=" * 50)
            sys.exit(0)
        else:
            print("\n" + "=" * 50)
            print("IPv6 UDP Support Test FAILED")
            print("=" * 50)
            sys.exit(1)

    finally:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        if os.path.exists(config_path):
            os.remove(config_path)


if __name__ == "__main__":
    main()
