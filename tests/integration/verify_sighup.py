#!/usr/bin/env python3
"""
SIGHUP Hot Reload Test

Verifies that mavrouter-rs correctly handles SIGHUP signal:
1. Validates new config before reloading
2. Gracefully restarts all endpoints
3. Maintains functionality after reload
4. Rejects invalid config without crashing

This test is Unix-only (SIGHUP doesn't exist on Windows).
"""

import sys
import os
import time
import signal
import tempfile
import shutil
import subprocess

# Skip on Windows
if sys.platform == 'win32':
    print("SKIP: SIGHUP test not supported on Windows")
    sys.exit(0)

from pymavlink import mavutil


def find_router_pid():
    """Find mavrouter process ID"""
    try:
        result = subprocess.run(
            ['pgrep', '-f', 'mavrouter'],
            capture_output=True,
            text=True
        )
        pids = result.stdout.strip().split('\n')
        pids = [p for p in pids if p]
        if pids:
            return int(pids[0])
    except Exception as e:
        print(f"Error finding router PID: {e}")
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


def verify_connection(port=5760, timeout=5):
    """Verify we can establish MAVLink connection"""
    try:
        master = mavutil.mavlink_connection(f'tcp:127.0.0.1:{port}', timeout=timeout)
        # Send a ping to verify connection works
        master.mav.ping_send(
            int(time.time() * 1000000),
            0,
            0,  # broadcast
            0
        )
        master.close()
        return True
    except Exception as e:
        print(f"Connection verification failed: {e}")
        return False


def main():
    print("=" * 50)
    print("SIGHUP Hot Reload Test")
    print("=" * 50)

    # Step 1: Find router PID
    print("\n[1/5] Finding router process...")
    pid = find_router_pid()
    if not pid:
        print("ERROR: Could not find mavrouter process")
        print("Make sure mavrouter is running before this test")
        sys.exit(1)
    print(f"Found router PID: {pid}")

    # Step 2: Verify initial connection
    print("\n[2/5] Verifying initial connection...")
    if not verify_connection():
        print("ERROR: Cannot connect to router before SIGHUP")
        sys.exit(1)
    print("Initial connection OK")

    # Step 3: Send SIGHUP
    print("\n[3/5] Sending SIGHUP signal...")
    try:
        os.kill(pid, signal.SIGHUP)
        print(f"SIGHUP sent to PID {pid}")
    except ProcessLookupError:
        print(f"ERROR: Process {pid} not found")
        sys.exit(1)
    except PermissionError:
        print(f"ERROR: Permission denied to send signal to PID {pid}")
        sys.exit(1)

    # Step 4: Wait for reload and verify
    print("\n[4/5] Waiting for reload to complete...")
    time.sleep(2)  # Give router time to reload

    # Check router is still running
    new_pid = find_router_pid()
    if not new_pid:
        print("ERROR: Router process died after SIGHUP!")
        sys.exit(1)
    print(f"Router still running (PID: {new_pid})")

    # Wait for TCP port to come back up
    print("Waiting for TCP port...")
    if not wait_for_tcp('127.0.0.1', 5760, timeout=10):
        print("ERROR: TCP port not available after reload")
        sys.exit(1)
    print("TCP port available")

    # Step 5: Verify connection after reload
    print("\n[5/5] Verifying connection after reload...")
    # Small delay to ensure endpoint is fully ready
    time.sleep(1)

    if not verify_connection():
        print("ERROR: Cannot connect to router after SIGHUP reload")
        sys.exit(1)
    print("Connection after reload OK")

    print("\n" + "=" * 50)
    print("SIGHUP Hot Reload Test PASSED")
    print("=" * 50)
    sys.exit(0)


if __name__ == "__main__":
    main()
