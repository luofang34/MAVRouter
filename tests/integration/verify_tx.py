import sys
import time
from pymavlink import mavutil

def main():
    print("Connecting to router...")
    master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')

    print("Waiting for heartbeat...")
    hb = master.recv_match(type='HEARTBEAT', blocking=True, timeout=10)
    if not hb:
        print("No heartbeat received!")
        sys.exit(1)

    # Extract source from the message, not the master object (which might be default)
    src_sys = hb.get_srcSystem()
    src_comp = hb.get_srcComponent()
    print(f"Received Heartbeat from SysID {src_sys} CompID {src_comp}")

    if src_sys == 0:
        print("Invalid SysID 0")
        sys.exit(1)

    # 1. Clear buffer
    while master.recv_match(blocking=False):
        pass

    # 2. Send Command
    target_comp = src_comp
    if target_comp == 0:
        target_comp = 1 # Force Autopilot if unknown

    print(f"Sending MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES to {src_sys}/{target_comp}...")
    master.mav.command_long_send(
        src_sys,
        target_comp,
        mavutil.mavlink.MAV_CMD_REQUEST_AUTOPILOT_CAPABILITIES,
        0, # Confirmation
        1, # Param1: Request version
        0, 0, 0, 0, 0, 0 # Unused
    )

    # 3. Wait for Response
    print("Waiting for response...")
    start = time.time()
    while time.time() - start < 5:
        msg = master.recv_match(blocking=True, timeout=1.0)
        if not msg:
            continue
            
        # print(f"Debug: Received {msg.get_type()} from {msg.get_srcSystem()}/{msg.get_srcComponent()}")

        if msg.get_type() == 'AUTOPILOT_VERSION':
            print("SUCCESS: Received AUTOPILOT_VERSION!")
            sys.exit(0)
            
        if msg.get_type() == 'COMMAND_ACK':
            if msg.command == 520:
                print(f"Received ACK for command: Result={msg.result}")
                if msg.result != mavutil.mavlink.MAV_RESULT_ACCEPTED:
                     print("Command was rejected/failed, but TX worked.")
                     sys.exit(0)

    print("TIMEOUT: Did not receive response.")
    sys.exit(1)

if __name__ == "__main__":
    main()
