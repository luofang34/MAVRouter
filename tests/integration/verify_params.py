import sys
import time
from pymavlink import mavutil

def main():
    print("Connecting...")
    master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')
    master.wait_heartbeat()
    print(f"Heartbeat from {master.target_system}/{master.target_component}")

    # 1. Read Param Index 0
    print("Reading Param Index 0...")
    master.mav.param_request_read_send(
        master.target_system,
        master.target_component,
        b"",
        0 # Index
    )
    
    param = master.recv_match(type='PARAM_VALUE', blocking=True, timeout=5)
    if not param:
        print("Timeout reading param")
        sys.exit(1)
        
    p_id = param.param_id
    p_val = param.param_value
    p_type = param.param_type
    print(f"Read Param: {p_id} = {p_val} (Type {p_type})")

    # 2. Write Param (Same Value - Safe Test)
    print(f"Writing Param {p_id} back with same value...")
    master.mav.param_set_send(
        master.target_system,
        master.target_component,
        p_id.encode('utf-8'), # ID must be bytes
        p_val,
        p_type
    )
    
    # 3. Wait for updated value (Ack)
    ack = master.recv_match(type='PARAM_VALUE', blocking=True, timeout=5)
    if not ack:
        print("Timeout waiting for write ack")
        sys.exit(1)
        
    if ack.param_id == p_id and abs(ack.param_value - p_val) < 0.0001:
        print("Write Verified (Value match).")
    else:
        print(f"Write Mismatch/Error: Got {ack.param_id}={ack.param_value}")
        sys.exit(1)

    print("Parameter Test Passed.")

if __name__ == "__main__":
    main()
