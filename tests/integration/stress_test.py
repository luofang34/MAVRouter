import sys
import time
from pymavlink import mavutil

def main():
    print("Connecting...")
    master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')
    master.wait_heartbeat()
    
    print("Starting Ping Storm (1000 packets)...")
    start = time.time()
    for i in range(1000):
        master.mav.ping_send(
            int(time.time() * 1000000),
            i,
            master.target_system,
            master.target_component
        )
        # Optional: Don't sleep, maximize throughput
    
    duration = time.time() - start
    print(f"Sent 1000 Pings in {duration:.4f}s ({1000/duration:.2f} Hz)")
    print("Checking if Router is still alive (Wait for Heartbeat)...")
    
    # If router crashed, TCP would close or no HB.
    hb = master.recv_match(type='HEARTBEAT', blocking=True, timeout=5)
    if hb:
        print("Router Alive.")
        sys.exit(0)
    else:
        print("Router Died/Unresponsive.")
        sys.exit(1)

if __name__ == "__main__":
    main()
