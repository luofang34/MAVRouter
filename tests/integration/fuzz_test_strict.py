import sys
import time
import socket
import random
import os
from pymavlink import mavutil

def main():
    print("Connecting to router (TCP Raw)...")
    try:
        s = socket.create_connection(('127.0.0.1', 5760))
    except Exception as e:
        print(f"Connection failed: {e}")
        sys.exit(1)

    print("Sending 1MB of random fuzz data...")
    for _ in range(100):
        data = os.urandom(10240) # 10KB
        try:
            s.sendall(data)
        except Exception as e:
            print(f"Send failed: {e}")
            sys.exit(1)
        time.sleep(0.01)
    
    s.close()
    print("Fuzzing complete. Reconnecting via MAVLink to verify liveness...")
    
    try:
        master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')
        # Wait for heartbeat from PX4 (routed through Serial)
        hb = master.recv_match(type='HEARTBEAT', blocking=True, timeout=5)
        if hb:
            print(f"Router Alive. Received Heartbeat from SysID {hb.get_srcSystem()}")
            sys.exit(0)
        else:
            print("Router Connected but No Heartbeat received (Stalled?)")
            sys.exit(1)
    except Exception as e:
        print(f"Liveness check failed: {e}")
        sys.exit(1)

if __name__ == "__main__":
    main()
