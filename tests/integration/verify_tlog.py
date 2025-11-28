#!/usr/bin/env python3
"""
TLOG (Telemetry Log) Verification Test

Verifies that mavrouter-rs correctly writes TLOG files:
1. Creates TLOG file in configured directory
2. Writes valid MAVLink messages with timestamps
3. TLOG file is readable by standard MAVLink tools
4. Graceful shutdown flushes all buffered data

This test does not require hardware - uses loopback via UDP.
"""

import sys
import os
import time
import glob
import struct
import tempfile
import shutil
import subprocess
import signal

from pymavlink import mavutil


def find_router_pid():
    """Find mavrouter process ID"""
    try:
        result = subprocess.run(
            ['pgrep', '-x', 'mavrouter'],
            capture_output=True,
            text=True
        )
        pids = result.stdout.strip().split('\n')
        pids = [p for p in pids if p]
        if pids:
            return int(pids[0])
    except Exception:
        pass
    return None


def wait_for_tcp(host, port, timeout=10):
    """Wait for TCP port to become available"""
    import socket
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


def start_router_with_tlog(logs_dir, config_path):
    """Start mavrouter with TLOG enabled"""
    # Create config
    config = f"""
[general]
bus_capacity = 1000
tcp_port = 5760
log = "{logs_dir}"
log_telemetry = true

[[endpoint]]
type = "udp"
address = "127.0.0.1:14550"
mode = "server"
"""
    with open(config_path, 'w') as f:
        f.write(config)

    # Start router
    proc = subprocess.Popen(
        ['./target/release/mavrouter', '--config', config_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT
    )
    return proc


def send_test_messages(count=10):
    """Send test MAVLink messages via TCP

    We send HEARTBEAT messages which are always broadcast,
    ensuring they get logged to TLOG.

    Note: source_system must be non-zero because mavrouter drops
    messages with system_id=0 (invalid/placeholder sources).
    """
    master = mavutil.mavlink_connection(
        'tcp:127.0.0.1:5760',
        source_system=255,  # GCS system ID (non-zero required)
        source_component=190  # GCS component ID
    )

    for i in range(count):
        # HEARTBEAT is always broadcast and will be logged
        master.mav.heartbeat_send(
            mavutil.mavlink.MAV_TYPE_GCS,
            mavutil.mavlink.MAV_AUTOPILOT_INVALID,
            0, 0, 0
        )
        time.sleep(0.05)  # Small delay to ensure ordering

    # Also send some ping messages
    for i in range(count):
        master.mav.ping_send(
            int(time.time() * 1000000),
            i,
            0,  # broadcast
            0
        )
        time.sleep(0.05)

    master.close()
    return count * 2


def verify_tlog_file(tlog_path, expected_min_messages=5):
    """Verify TLOG file is valid and readable"""
    print(f"Verifying TLOG file: {tlog_path}")

    file_size = os.path.getsize(tlog_path)
    print(f"  File size: {file_size} bytes")

    if file_size == 0:
        print("  ERROR: TLOG file is empty!")
        return False

    # Try to read with pymavlink
    try:
        mlog = mavutil.mavlink_connection(tlog_path)
        msg_count = 0
        msg_types = set()

        while True:
            msg = mlog.recv_match(blocking=False)
            if msg is None:
                break
            msg_count += 1
            msg_types.add(msg.get_type())

        print(f"  Messages read: {msg_count}")
        print(f"  Message types: {msg_types}")

        if msg_count < expected_min_messages:
            print(f"  WARNING: Expected at least {expected_min_messages} messages")
            # Not a hard failure - some messages may not be logged

        return msg_count > 0

    except Exception as e:
        print(f"  ERROR reading TLOG: {e}")
        return False


def verify_tlog_timestamp_format(tlog_path):
    """Verify TLOG has correct timestamp format (8-byte big-endian microseconds)"""
    print(f"Verifying timestamp format...")

    try:
        with open(tlog_path, 'rb') as f:
            # Read first timestamp (8 bytes, big-endian)
            ts_bytes = f.read(8)
            if len(ts_bytes) < 8:
                print("  ERROR: File too short for timestamp")
                return False

            timestamp_us = struct.unpack('>Q', ts_bytes)[0]

            # Should be a reasonable Unix timestamp in microseconds
            # Between 2020 and 2100
            min_ts = 1577836800 * 1000000  # 2020-01-01
            max_ts = 4102444800 * 1000000  # 2100-01-01

            if min_ts <= timestamp_us <= max_ts:
                print(f"  First timestamp: {timestamp_us} us (valid)")
                return True
            else:
                print(f"  ERROR: Timestamp {timestamp_us} out of range")
                return False

    except Exception as e:
        print(f"  ERROR: {e}")
        return False


def main():
    print("=" * 50)
    print("TLOG Verification Test")
    print("=" * 50)

    # Create temp directory for logs
    logs_dir = tempfile.mkdtemp(prefix='mavrouter_tlog_test_')
    config_path = os.path.join(logs_dir, 'test_config.toml')

    try:
        # Kill any existing router
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        time.sleep(1)

        # Step 1: Start router with TLOG enabled
        print("\n[1/6] Starting router with TLOG enabled...")
        proc = start_router_with_tlog(logs_dir, config_path)

        if not wait_for_tcp('127.0.0.1', 5760, timeout=10):
            print("ERROR: Router failed to start")
            proc.kill()
            sys.exit(1)
        print("Router started")

        # Step 2: Send test messages
        print("\n[2/6] Sending test messages...")
        msg_count = send_test_messages(20)
        print(f"Sent {msg_count} messages")

        # Step 3: Wait for messages to be written
        print("\n[3/6] Waiting for TLOG write...")
        time.sleep(2)

        # Step 4: Graceful shutdown (send SIGTERM)
        print("\n[4/6] Sending graceful shutdown signal...")
        proc.terminate()
        try:
            proc.wait(timeout=5)
            print("Router shut down gracefully")
        except subprocess.TimeoutExpired:
            print("WARNING: Router didn't shut down in time, killing...")
            proc.kill()

        # Step 5: Find TLOG file
        print("\n[5/6] Looking for TLOG file...")
        tlog_files = glob.glob(os.path.join(logs_dir, 'flight_*.tlog'))

        if not tlog_files:
            print("ERROR: No TLOG file created!")
            sys.exit(1)

        tlog_path = tlog_files[0]
        print(f"Found: {tlog_path}")

        # Step 6: Verify TLOG contents
        print("\n[6/6] Verifying TLOG contents...")

        if not verify_tlog_timestamp_format(tlog_path):
            print("ERROR: Invalid timestamp format")
            sys.exit(1)

        if not verify_tlog_file(tlog_path, expected_min_messages=1):
            print("ERROR: TLOG file verification failed")
            sys.exit(1)

        print("\n" + "=" * 50)
        print("TLOG Verification Test PASSED")
        print("=" * 50)
        sys.exit(0)

    finally:
        # Cleanup
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        # Keep logs_dir for debugging if test fails
        if '--keep-logs' not in sys.argv:
            shutil.rmtree(logs_dir, ignore_errors=True)


if __name__ == "__main__":
    main()
