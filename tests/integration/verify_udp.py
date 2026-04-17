import os
import sys
import time
import threading
from pymavlink import mavutil

# CI sets MAVROUTER_TCP_PORT / MAVROUTER_UDP_PORT to ephemeral ports it
# claimed before starting the router; fall back to 5760 / 14550 when a
# developer runs this script against a locally-started router.
TCP_PORT = int(os.environ.get('MAVROUTER_TCP_PORT', 5760))
UDP_PORT = int(os.environ.get('MAVROUTER_UDP_PORT', 14550))


def autopilot_simulator():
    """Simulates a drone sending heartbeats via TCP"""
    try:
        # Allow some time for main thread to start
        time.sleep(0.5)
        # Connect as SysID 1
        master = mavutil.mavlink_connection(f'tcp:127.0.0.1:{TCP_PORT}', source_system=1)
        while True:
            master.mav.heartbeat_send(
                mavutil.mavlink.MAV_TYPE_QUADROTOR,
                mavutil.mavlink.MAV_AUTOPILOT_PX4,
                0, 0, 0
            )
            time.sleep(0.5)
    except Exception as e:
        print(f"Simulator error: {e}")

def main():
    # Start simulator
    sim_thread = threading.Thread(target=autopilot_simulator, daemon=True)
    sim_thread.start()

    print("Connecting via UDP to router...")
    master = mavutil.mavlink_connection(f'udpout:127.0.0.1:{UDP_PORT}', source_system=254)

    print("Sending Heartbeats...")
    
    start = time.time()
    while time.time() - start < 10:
        # Send HB every 0.5s
        master.mav.heartbeat_send(
            mavutil.mavlink.MAV_TYPE_GCS,
            mavutil.mavlink.MAV_AUTOPILOT_INVALID,
            0, 0, 0
        )
        
        # Check for reply
        while True:
            msg = master.recv_match(blocking=False)
            if not msg:
                break
            
            if msg.get_type() == 'HEARTBEAT':
                src_sys = msg.get_srcSystem()
                if src_sys == 1:
                    print(f"SUCCESS: Received Heartbeat from SysID {src_sys}")
                    sys.exit(0)
        
        time.sleep(0.5)

    print("TIMEOUT: No heartbeat from SysID 1")
    sys.exit(1)

if __name__ == "__main__":
    main()
