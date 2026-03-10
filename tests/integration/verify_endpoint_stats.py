#!/usr/bin/env python3
"""
Per-Endpoint Stats Test

Verifies that mavrouter-rs tracks per-endpoint traffic statistics:
1. Starts router with stats_socket_path configured
2. Sends traffic via TCP and UDP
3. Queries stats socket for endpoint_stats
4. Verifies counters reflect traffic sent

This test is Unix-only (uses Unix domain sockets).
"""

import sys
import os
import time
import json
import tempfile
import subprocess
import socket

# Skip on Windows
if sys.platform == 'win32':
    print("SKIP: Stats socket test not supported on Windows")
    sys.exit(0)

from pymavlink import mavutil

# Resolve binary path relative to this script (repo_root/target/release/mavrouter)
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


def query_stats_socket(socket_path, command, timeout=5):
    """Send a command to the Unix stats socket and return the response."""
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect(socket_path)
        sock.sendall(command.encode('utf-8'))
        data = sock.recv(65536)
        return data.decode('utf-8')
    finally:
        sock.close()


def main():
    print("=" * 50)
    print("Per-Endpoint Stats Test")
    print("=" * 50)

    stats_socket = tempfile.mktemp(prefix='mavrouter_stats_', suffix='.sock')
    config_path = tempfile.mktemp(suffix='.toml')
    proc = None

    try:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        time.sleep(1)

        # Step 1: Create config with stats socket
        print("\n[1/6] Creating config with stats socket...")
        config = f"""
[general]
bus_capacity = 1000
tcp_port = 5763
stats_socket_path = "{stats_socket}"
stats_sample_interval_secs = 1
stats_log_interval_secs = 60

[[endpoint]]
type = "udp"
address = "127.0.0.1:14553"
mode = "server"
"""
        with open(config_path, 'w') as f:
            f.write(config)
        print(f"Stats socket: {stats_socket}")

        # Step 2: Start router
        print("\n[2/6] Starting router...")
        proc = subprocess.Popen(
            [BINARY, '--config', config_path],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT
        )
        if not wait_for_tcp('127.0.0.1', 5763, timeout=10):
            print("ERROR: Router failed to start")
            if proc:
                out = proc.stdout.read().decode('utf-8', errors='replace') if proc.stdout else ''
                print(f"Router output:\n{out[:2000]}")
                proc.kill()
            sys.exit(1)
        print(f"Router started (PID: {proc.pid})")

        # Wait for stats socket to appear
        for _ in range(20):
            if os.path.exists(stats_socket):
                break
            time.sleep(0.5)
        if not os.path.exists(stats_socket):
            print("ERROR: Stats socket not created")
            proc.kill()
            sys.exit(1)
        print("Stats socket created")

        # Step 3: Query initial stats (should be zeros)
        print("\n[3/6] Querying initial endpoint stats...")
        try:
            response = query_stats_socket(stats_socket, "endpoint_stats")
            initial_stats = json.loads(response)
            print(f"Initial stats: {len(initial_stats)} endpoints")
            for ep in initial_stats:
                print(f"  Endpoint {ep['id']} ({ep['name']}): in={ep['msgs_in']} out={ep['msgs_out']}")
        except Exception as e:
            print(f"ERROR: Failed to query stats: {e}")
            proc.kill()
            sys.exit(1)

        # Step 4: Send traffic
        print("\n[4/6] Sending traffic...")
        # Connect TCP client and send heartbeats
        tcp_master = mavutil.mavlink_connection('tcp:127.0.0.1:5763', source_system=1)
        for _ in range(20):
            tcp_master.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_QUADROTOR,
                mavutil.mavlink.MAV_AUTOPILOT_PX4,
                0, 0, 0
            )
            time.sleep(0.05)

        # Connect UDP client and send heartbeats
        udp_master = mavutil.mavlink_connection('udpout:127.0.0.1:14553', source_system=254)
        for _ in range(20):
            udp_master.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_GCS,
                mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                0, 0, 0
            )
            time.sleep(0.05)

        time.sleep(1)  # Let stats propagate
        tcp_master.close()
        udp_master.close()

        # Step 5: Query stats again — should have non-zero counters
        print("\n[5/6] Querying endpoint stats after traffic...")
        try:
            response = query_stats_socket(stats_socket, "endpoint_stats")
            final_stats = json.loads(response)

            total_msgs_in = sum(ep['msgs_in'] for ep in final_stats)
            total_msgs_out = sum(ep['msgs_out'] for ep in final_stats)
            total_bytes_in = sum(ep['bytes_in'] for ep in final_stats)

            print(f"Final stats: {len(final_stats)} endpoints")
            for ep in final_stats:
                print(f"  Endpoint {ep['id']} ({ep['name']}): in={ep['msgs_in']} out={ep['msgs_out']} bytes_in={ep['bytes_in']} bytes_out={ep['bytes_out']} err={ep['errors']}")

            if total_msgs_in == 0:
                print("ERROR: No messages recorded as received!")
                proc.kill()
                sys.exit(1)
            print(f"Total msgs_in={total_msgs_in}, msgs_out={total_msgs_out}")

        except Exception as e:
            print(f"ERROR: Failed to query final stats: {e}")
            proc.kill()
            sys.exit(1)

        # Step 6: Query routing stats too
        print("\n[6/6] Querying routing table stats...")
        try:
            response = query_stats_socket(stats_socket, "stats")
            rt_stats = json.loads(response)
            print(f"Routing: systems={rt_stats['total_systems']} routes={rt_stats['total_routes']} endpoints={rt_stats['total_endpoints']}")

            if rt_stats['total_systems'] == 0:
                print("WARNING: No systems in routing table (might be timing)")

            # Verify help command
            help_response = query_stats_socket(stats_socket, "help")
            if "Available commands" not in help_response:
                print(f"ERROR: Unexpected help response: {help_response}")
                proc.kill()
                sys.exit(1)
            print("Help command works")
        except Exception as e:
            print(f"ERROR: Failed to query routing stats: {e}")
            proc.kill()
            sys.exit(1)

        # Cleanup
        proc.terminate()
        proc.wait(timeout=5)

        print("\n" + "=" * 50)
        print("Per-Endpoint Stats Test PASSED")
        print("=" * 50)
        sys.exit(0)

    finally:
        subprocess.run(['pkill', '-9', 'mavrouter'], capture_output=True)
        for path in [config_path, stats_socket]:
            if os.path.exists(path):
                os.remove(path)


if __name__ == "__main__":
    main()
