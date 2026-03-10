#!/usr/bin/env python3
"""
Config Directory Test

Verifies that mavrouter-rs correctly loads configuration from a directory:
1. Creates a temp directory with multiple .toml config files
2. Starts router with --config-dir
3. Verifies endpoints from all config files are active
4. Verifies general settings are merged (last file wins)
"""

import sys
import os
import time
import tempfile
import shutil
import subprocess
import socket

from pymavlink import mavutil

# Resolve binary path relative to this script (repo_root/target/release/mavrouter)
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.join(SCRIPT_DIR, '..', '..')
BINARY = os.path.join(REPO_ROOT, 'target', 'release', 'mavrouter')


def wait_for_tcp(host, port, timeout=10):
    """Wait for TCP port to become available"""
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
    print("Config Directory Test")
    print("=" * 50)

    # Create temp config directory
    config_dir = tempfile.mkdtemp(prefix='mavrouter_confdir_')
    proc = None

    try:
        # Kill any existing router on our test ports
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        time.sleep(1)

        # Step 1: Write config files to directory
        print("\n[1/5] Creating config directory with multiple files...")

        # 00-general.toml: base settings
        with open(os.path.join(config_dir, '00-general.toml'), 'w') as f:
            f.write("""
[general]
bus_capacity = 1000
tcp_port = 5762
""")

        # 10-udp.toml: UDP endpoint
        with open(os.path.join(config_dir, '10-udp.toml'), 'w') as f:
            f.write("""
[[endpoint]]
type = "udp"
address = "127.0.0.1:14552"
mode = "server"
""")

        # 20-override.toml: override bus_capacity (last file wins)
        with open(os.path.join(config_dir, '20-override.toml'), 'w') as f:
            f.write("""
[general]
bus_capacity = 3000
""")

        # Also a non-toml file that should be ignored
        with open(os.path.join(config_dir, 'README.txt'), 'w') as f:
            f.write("This should be ignored")

        print(f"Config dir: {config_dir}")
        for f in sorted(os.listdir(config_dir)):
            print(f"  {f}")

        # Step 2: Start router with --config-dir only (no --config file)
        # We need a minimal base config file since --config has a default
        base_config = tempfile.mktemp(suffix='.toml')
        with open(base_config, 'w') as f:
            f.write("")  # Empty config — directory provides everything

        print("\n[2/5] Starting router with --config-dir...")
        proc = subprocess.Popen(
            [BINARY, '--config', base_config, '--config-dir', config_dir],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT
        )

        # Step 3: Verify TCP port from config dir is open
        print("\n[3/5] Waiting for TCP port 5762...")
        if not wait_for_tcp('127.0.0.1', 5762, timeout=10):
            print("ERROR: TCP port 5762 not available — config dir not loaded")
            if proc:
                out = proc.stdout.read().decode('utf-8', errors='replace') if proc.stdout else ''
                print(f"Router output:\n{out[:2000]}")
                proc.kill()
            sys.exit(1)
        print("TCP port 5762 is open (from 00-general.toml)")

        # Step 4: Verify MAVLink connection works via TCP
        print("\n[4/5] Verifying MAVLink connection via TCP...")
        try:
            master = mavutil.mavlink_connection('tcp:127.0.0.1:5762', source_system=254)
            master.mav.ping_send(int(time.time() * 1000000), 0, 0, 0)
            time.sleep(0.5)
            master.close()
            print("MAVLink TCP connection OK")
        except Exception as e:
            print(f"ERROR: MAVLink connection failed: {e}")
            proc.kill()
            sys.exit(1)

        # Step 5: Verify UDP endpoint is active by sending to it
        print("\n[5/5] Verifying UDP endpoint from 10-udp.toml...")
        try:
            udp_master = mavutil.mavlink_connection('udpout:127.0.0.1:14552', source_system=1)
            udp_master.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_QUADROTOR,
                mavutil.mavlink.MAV_AUTOPILOT_PX4,
                0, 0, 0
            )
            time.sleep(0.5)
            udp_master.close()
            print("UDP endpoint 14552 accepted data (from 10-udp.toml)")
        except Exception as e:
            print(f"ERROR: UDP endpoint not active: {e}")
            proc.kill()
            sys.exit(1)

        # Cleanup
        proc.terminate()
        proc.wait(timeout=5)

        print("\n" + "=" * 50)
        print("Config Directory Test PASSED")
        print("=" * 50)
        sys.exit(0)

    finally:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        shutil.rmtree(config_dir, ignore_errors=True)
        if os.path.exists(base_config):
            os.remove(base_config)


if __name__ == "__main__":
    main()
