#!/usr/bin/env python3
"""
SIGHUP Invalid Config Test

Verifies that mavrouter-rs correctly handles SIGHUP with invalid config:
1. Router should NOT crash
2. Router should continue running with old config
3. Error should be logged (not verified in this test)

This test is Unix-only (SIGHUP doesn't exist on Windows).
"""

import sys
import os
import time
import signal
import tempfile
import subprocess

# Skip on Windows
if sys.platform == 'win32':
    print("SKIP: SIGHUP test not supported on Windows")
    sys.exit(0)

from pymavlink import mavutil


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


def verify_connection(port=5760):
    """Verify we can establish MAVLink connection"""
    try:
        master = mavutil.mavlink_connection(f'tcp:127.0.0.1:{port}', timeout=5)
        master.mav.ping_send(int(time.time() * 1000000), 0, 0, 0)
        master.close()
        return True
    except Exception as e:
        print(f"Connection failed: {e}")
        return False


def main():
    print("=" * 50)
    print("SIGHUP Invalid Config Test")
    print("=" * 50)

    # Create temp config file
    config_path = tempfile.mktemp(suffix='.toml')

    # Valid config initially
    valid_config = """
[general]
bus_capacity = 1000
tcp_port = 5760

[[endpoint]]
type = "udp"
address = "127.0.0.1:14550"
mode = "server"
"""

    # Invalid config (syntax error)
    invalid_config = """
[general
bus_capacity = invalid_value
tcp_port = not_a_number
"""

    try:
        # Kill any existing router
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        time.sleep(1)

        # Write valid config
        with open(config_path, 'w') as f:
            f.write(valid_config)

        # Step 1: Start router
        print("\n[1/5] Starting router with valid config...")
        proc = subprocess.Popen(
            ['./target/release/mavrouter', '--config', config_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT
        )

        if not wait_for_tcp('127.0.0.1', 5760, timeout=10):
            print("ERROR: Router failed to start")
            proc.kill()
            sys.exit(1)
        print(f"Router started (PID: {proc.pid})")

        # Step 2: Verify initial connection
        print("\n[2/5] Verifying initial connection...")
        if not verify_connection():
            print("ERROR: Cannot connect to router")
            proc.kill()
            sys.exit(1)
        print("Initial connection OK")

        # Step 3: Replace config with invalid one
        print("\n[3/5] Replacing config with INVALID config...")
        with open(config_path, 'w') as f:
            f.write(invalid_config)
        print("Invalid config written")

        # Step 4: Send SIGHUP
        print("\n[4/5] Sending SIGHUP with invalid config...")
        os.kill(proc.pid, signal.SIGHUP)
        print("SIGHUP sent")

        # Wait a bit for router to process
        time.sleep(2)

        # Check router is still running
        if proc.poll() is not None:
            print("ERROR: Router crashed after SIGHUP with invalid config!")
            sys.exit(1)
        print("Router still running (good - rejected invalid config)")

        # Step 5: Verify connection still works (old config still active)
        print("\n[5/5] Verifying connection still works...")
        if not verify_connection():
            print("ERROR: Router stopped accepting connections after invalid SIGHUP")
            proc.kill()
            sys.exit(1)
        print("Connection still OK (router using old config)")

        # Cleanup
        proc.terminate()
        proc.wait(timeout=5)

        print("\n" + "=" * 50)
        print("SIGHUP Invalid Config Test PASSED")
        print("=" * 50)
        sys.exit(0)

    finally:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        if os.path.exists(config_path):
            os.remove(config_path)


if __name__ == "__main__":
    main()
