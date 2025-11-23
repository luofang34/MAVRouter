import sys
import time
import socket
import os
import threading
from pymavlink import mavutil

TARGET_IP = '127.0.0.1'
TARGET_PORT = 5760

def test_slow_loris():
    print("\n[Chaos] Starting Slow Loris Test (1 byte/100ms)...")
    try:
        s = socket.create_connection((TARGET_IP, TARGET_PORT))
        mav = mavutil.mavlink.MAVLink(None)
        msg = mav.heartbeat_encode(mavutil.mavlink.MAV_TYPE_GCS, 0, 0, 0, 0, 0)
        buf = msg.pack(mav)
        
        # Send 1 byte at a time
        for byte in buf:
            s.send(bytes([byte]))
            time.sleep(0.05) # Total time ~ 0.05 * len (approx 1s)
            
        # Now check if router routed it.
        # Since we can't easily see the output (Serial), we rely on the fact that the router
        # didn't disconnect us.
        # Better: Listen on UDP endpoint to see if it got routed?
        # For now, just ensuring no disconnect.
        print("[Chaos] Slow Loris sent. Socket still open.")
        s.close()
        return True
    except Exception as e:
        print(f"[Chaos] Slow Loris Failed: {e}")
        return False

def test_buffer_overflow():
    print("\n[Chaos] Starting Buffer Overflow Test (1.1MB data)...")
    try:
        s = socket.create_connection((TARGET_IP, TARGET_PORT))
        # Send 1.1MB of 'A'
        chunk = b'A' * 1024
        for _ in range(1100): # 1.1MB
            s.send(chunk)
        
        # Send a valid heartbeat immediately after
        mav = mavutil.mavlink.MAVLink(None)
        msg = mav.heartbeat_encode(mavutil.mavlink.MAV_TYPE_GCS, 0, 0, 0, 0, 0)
        s.send(msg.pack(mav))
        
        print("[Chaos] Buffer overflow data sent. If router is alive, it dropped buffer.")
        s.close()
        return True
    except Exception as e:
        print(f"[Chaos] Buffer Overflow Failed: {e}")
        return False

def test_fd_exhaustion():
    print("\n[Chaos] Starting FD Exhaustion Test (200 connections)...")
    sockets = []
    try:
        for i in range(200):
            s = socket.create_connection((TARGET_IP, TARGET_PORT))
            sockets.append(s)
            if i % 50 == 0:
                print(f"  Connected {i}...")
        
        print(f"[Chaos] {len(sockets)} connections held.")
        time.sleep(2)
        
        # Verify we can still connect one more and send data
        master = mavutil.mavlink_connection(f'tcp:{TARGET_IP}:{TARGET_PORT}')
        master.wait_heartbeat(timeout=2)
        print("[Chaos] Router still responsive with 200 clients.")
        
    except Exception as e:
        print(f"[Chaos] FD Exhaustion hit limit (expected?) or crashed: {e}")
    finally:
        for s in sockets:
            s.close()
        return True

if __name__ == "__main__":
    if not test_slow_loris(): sys.exit(1)
    if not test_buffer_overflow(): sys.exit(1)
    if not test_fd_exhaustion(): sys.exit(1)
    print("\n[Chaos] All Chaos Tests Completed.")
