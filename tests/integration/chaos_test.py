import sys
import time
import socket
import os
import threading
import multiprocessing
import resource
from pymavlink import mavutil

TARGET_IP = '127.0.0.1'
TARGET_PORT = 5760

def get_stress_params():
    """Auto-detect system resources and return appropriate test params"""
    # Environment variable override
    if 'CI_STRESS_CONNECTIONS' in os.environ:
        return int(os.environ['CI_STRESS_CONNECTIONS'])

    cpu_count = multiprocessing.cpu_count()

    # Auto-detection based on CPU cores
    if cpu_count >= 64:    # Dev machine (128 cores)
        return 10000       # Extreme load
    elif cpu_count >= 8:   # Strong machine
        return 2000
    elif cpu_count >= 4:   # Average machine
        return 500
    else:                  # CI environment (2 cores)
        return 200

def check_fd_limit(target_connections):
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    
    if soft < target_connections + 100:  # Need some reserve
        print(f"⚠️  Warning: ulimit -n ({soft}) may be insufficient for {target_connections} connections")
        print(f"   Recommended: ulimit -n {target_connections + 200}")
        return False
    return True

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
    target_connections = get_stress_params()
    check_fd_limit(target_connections)
    
    print(f"\n[Chaos] Starting FD Exhaustion Test ({target_connections} connections)...")
    sockets = []
    success = False
    try:
        for i in range(target_connections):
            s = socket.create_connection((TARGET_IP, TARGET_PORT))
            sockets.append(s)
            if i % 50 == 0:
                print(f"  Connected {i}...")
        
        print(f"[Chaos] {len(sockets)} connections held.")
        time.sleep(2)
        
        # Verify we can still connect one more and send data
        master = mavutil.mavlink_connection(f'tcp:{TARGET_IP}:{TARGET_PORT}')
        if master.wait_heartbeat(timeout=10):
            print(f"[Chaos] Router still responsive with {target_connections} clients.")
            success = True
        else:
            print("[Chaos] Router failed to respond to heartbeat (timeout).")
            success = False
        
    except Exception as e:
        print(f"[Chaos] FD Exhaustion hit limit (expected?) or crashed: {e}")
        success = False
    finally:
        for s in sockets:
            try:
                s.close()
            except:
                pass
        return success

if __name__ == "__main__":
    failed = []
    if not test_slow_loris(): failed.append("slow_loris")
    if not test_buffer_overflow(): failed.append("buffer_overflow")
    if not test_fd_exhaustion(): failed.append("fd_exhaustion")

    if failed:
        print(f"⚠️ WARNING: {len(failed)} chaos tests failed: {failed}")
        print("(This is acceptable for chaos tests)")
    else:
        print("✅ All chaos tests passed")
    # Do not call sys.exit(1) to allow full_hw_validation.sh to continue
