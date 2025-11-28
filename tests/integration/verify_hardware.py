import sys
import time
from pymavlink import mavutil

def main():
    print("Connecting to router...")
    # Connect to TCP port exposed by router
    # mavrouter-rs is Server, so we are Client.
    master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')

    print("Waiting for heartbeat...")
    # Wait for a heartbeat
    hb = master.recv_match(type='HEARTBEAT', blocking=True, timeout=10)
    if not hb:
        print("No heartbeat received!")
        sys.exit(1)

    print(f"Received Heartbeat from SysID {master.target_system} CompID {master.target_component}")
    
    target_system = master.target_system
    target_component = master.target_component

    if target_system == 0:
        print("Received heartbeat from system 0 (Broadcast?), waiting for specific system...")
        sys.exit(1)

    # Use component 1 (MAV_COMP_ID_AUTOPILOT1) for param requests
    # Some flight controllers don't respond to component 0 requests
    request_component = 1 if target_component == 0 else target_component
    print(f"Requesting parameters from SysID {target_system} CompID {request_component}...")

    # Send PARAM_REQUEST_LIST
    master.mav.param_request_list_send(
        target_system,
        request_component
    )

    print("Waiting for PARAM_VALUE...")
    # We only need ONE parameter to prove it works
    param = master.recv_match(type='PARAM_VALUE', blocking=True, timeout=5)
    
    if param:
        print(f"Received PARAM_VALUE: {param.param_id}\t{param.param_value}")
        print("Test Passed: Bidirectional communication verified.")
        sys.exit(0)
    else:
        print("Timed out waiting for PARAM_VALUE")
        sys.exit(1)

if __name__ == "__main__":
    main()
