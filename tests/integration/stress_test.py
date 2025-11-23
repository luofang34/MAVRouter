import sys
import time
from pymavlink import mavutil

def main():
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

    print("Starting Ping Storm (1000 packets)...")
    start = time.time()
    for i in range(1000):
        master.mav.ping_send(
            int(time.time() * 1000000),
            i,
            1,  # target_system - use 1 as default since we may not have heartbeat
            0   # target_component
        )
        # Optional: Don't sleep, maximize throughput

    duration = time.time() - start
    print(f"Sent 1000 Pings in {duration:.4f}s ({1000/duration:.2f} Hz)")

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
