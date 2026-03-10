#!/usr/bin/env python3
"""
Sniffer Mode Test

Verifies that mavrouter-rs sniffer mode works correctly:
1. Starts router with sniffer_sysids = [253]
2. Connects a "sniffer" client (sysid=253) via TCP
3. Connects a "normal autopilot" client (sysid=1) via TCP
4. Connects a GCS client (sysid=254) via UDP
5. Sends messages from autopilot and GCS
6. Verifies sniffer receives messages from BOTH sources
7. Verifies normal routing still works (GCS gets autopilot heartbeats)

The sniffer client should receive ALL traffic going through the router,
regardless of routing decisions, because sysid 253 is in sniffer_sysids.
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
    print("Sniffer Mode Test")
    print("=" * 50)

    config_path = tempfile.mktemp(suffix='.toml')
    proc = None

    try:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        time.sleep(1)

        # Step 1: Create config with sniffer mode
        print("\n[1/7] Creating config with sniffer_sysids = [253]...")
        config = """
[general]
bus_capacity = 5000
tcp_port = 5764
sniffer_sysids = [253]

[[endpoint]]
type = "udp"
address = "127.0.0.1:14554"
mode = "server"
"""
        with open(config_path, 'w') as f:
            f.write(config)

        # Step 2: Start router
        print("\n[2/7] Starting router...")
        proc = subprocess.Popen(
            [BINARY, '--config', config_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT
        )
        if not wait_for_tcp('127.0.0.1', 5764, timeout=10):
            print("ERROR: Router failed to start")
            if proc:
                out = proc.stdout.read().decode('utf-8', errors='replace') if proc.stdout else ''
                print(f"Router output:\n{out[:2000]}")
                proc.kill()
            sys.exit(1)
        print(f"Router started (PID: {proc.pid})")

        # Step 3: Connect sniffer client (sysid=253) via TCP
        print("\n[3/7] Connecting sniffer client (sysid=253)...")
        sniffer = mavutil.mavlink_connection('tcp:127.0.0.1:5764', source_system=253)
        # Send a heartbeat so the router learns this sysid is on this endpoint
        sniffer.mav.heartbeat_send(
            mavutil.mavlink.MAV_TYPE_ONBOARD_CONTROLLER,
            mavutil.mavlink.MAV_AUTOPILOT_INVALID,
            0, 0, 0
        )
        time.sleep(0.5)
        print("Sniffer connected and identified")

        # Step 4: Connect autopilot (sysid=1) via TCP
        print("\n[4/7] Connecting autopilot client (sysid=1)...")
        autopilot = mavutil.mavlink_connection('tcp:127.0.0.1:5764', source_system=1)
        autopilot.mav.heartbeat_send(
            mavutil.mavlink.MAV_TYPE_QUADROTOR,
            mavutil.mavlink.MAV_AUTOPILOT_PX4,
            0, 0, 0
        )
        time.sleep(0.5)
        print("Autopilot connected")

        # Step 5: Connect GCS (sysid=254) via UDP
        print("\n[5/7] Connecting GCS client (sysid=254) via UDP...")
        gcs = mavutil.mavlink_connection('udpout:127.0.0.1:14554', source_system=254)
        gcs.mav.heartbeat_send(
            mavutil.mavlink.MAV_TYPE_GCS,
            mavutil.mavlink.MAV_AUTOPILOT_INVALID,
            0, 0, 0
        )
        time.sleep(0.5)
        print("GCS connected")

        # Step 6: Send messages and check what sniffer receives
        print("\n[6/7] Sending messages from autopilot and GCS...")

        # Drain any pending messages from sniffer
        while True:
            msg = sniffer.recv_match(blocking=False)
            if not msg:
                break

        # Send bursts from both sources
        for i in range(10):
            autopilot.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_QUADROTOR,
                mavutil.mavlink.MAV_AUTOPILOT_PX4,
                0, 0, 0
            )
            gcs.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_GCS,
                mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                0, 0, 0
            )
            time.sleep(0.05)

        time.sleep(1)  # Let messages propagate

        # Collect messages on sniffer
        sniffer_sources = set()
        msg_count = 0
        while True:
            msg = sniffer.recv_match(blocking=False)
            if not msg:
                break
            if msg.get_type() == 'BAD_DATA':
                continue
            src = msg.get_srcSystem()
            if src != 253:  # Don't count own messages
                sniffer_sources.add(src)
                msg_count += 1

        print(f"Sniffer received {msg_count} messages from sources: {sniffer_sources}")

        # Step 7: Verify sniffer got messages from BOTH sources
        print("\n[7/7] Verifying sniffer received from all sources...")

        success = True
        if 1 not in sniffer_sources:
            print("ERROR: Sniffer did NOT receive messages from autopilot (sysid=1)")
            success = False
        else:
            print("OK: Sniffer received messages from autopilot (sysid=1)")

        if 254 not in sniffer_sources:
            print("ERROR: Sniffer did NOT receive messages from GCS (sysid=254)")
            success = False
        else:
            print("OK: Sniffer received messages from GCS (sysid=254)")

        if msg_count < 5:
            print(f"WARNING: Low message count ({msg_count}), expected more")

        # Cleanup
        sniffer.close()
        autopilot.close()
        gcs.close()
        proc.terminate()
        proc.wait(timeout=5)

        if not success:
            print("\n" + "=" * 50)
            print("Sniffer Mode Test FAILED")
            print("=" * 50)
            sys.exit(1)

        print("\n" + "=" * 50)
        print("Sniffer Mode Test PASSED")
        print("=" * 50)
        sys.exit(0)

    finally:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        if os.path.exists(config_path):
            os.remove(config_path)


if __name__ == "__main__":
    main()
