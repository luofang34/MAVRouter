import sys
import time
import os
import multiprocessing
from pymavlink import mavutil

def get_stress_packets():
    """Auto-detect system resources and return appropriate packet count"""
    # Environment variable override
    if 'CI_STRESS_PACKETS' in os.environ:
        return int(os.environ['CI_STRESS_PACKETS'])

    cpu_count = multiprocessing.cpu_count()

    # Auto-detection based on CPU cores
    if cpu_count >= 64:    # Dev machine (128 cores)
        return 100000      # Extreme load
    elif cpu_count >= 8:   # Strong machine
        return 20000
    elif cpu_count >= 4:   # Average machine
        return 5000
    else:                  # CI environment (2 cores)
        return 1000

def main():
    target_packets = get_stress_packets()
    print("Connecting...")
    master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')

    # Wait for heartbeat with timeout (may not be available in CI without hardware)
    print("Waiting for heartbeat (timeout: 5s)...")
    try:
        master.wait_heartbeat(timeout=5)
        print("Heartbeat received, system ready")
        have_heartbeat = True
    except Exception as e:
        print(f"TIMEOUT: No heartbeat from SysID 1 (expected in CI without hardware)")
        print("Proceeding with stress test anyway - router should still route messages")
        have_heartbeat = False

    print(f"Starting Ping Storm ({target_packets} packets)...")
    start = time.time()
    for i in range(target_packets):
        master.mav.ping_send(
            int(time.time() * 1000000),
            i,
            1,  # target_system - use 1 as default since we may not have heartbeat
            0   # target_component
        )
        # Optional: Don't sleep, maximize throughput

    duration = time.time() - start
    print(f"Sent {target_packets} Pings in {duration:.4f}s ({target_packets/duration:.2f} Hz)")

    # Verify TCP connection is still alive (router didn't crash)
    print("Verifying router TCP connection is still alive...")
    try:
        # Try to send one more message - if router crashed, this would fail
        master.mav.ping_send(int(time.time() * 1000000), 9999, 1, 0)
        print("Router connection still alive - stress test PASSED")
        sys.exit(0)
    except Exception as e:
        print(f"Router connection failed: {e}")
        print("Router may have crashed - stress test FAILED")
        sys.exit(1)

if __name__ == "__main__":
    main()
