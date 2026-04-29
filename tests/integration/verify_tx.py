import os
import sys
import time
from pymavlink import mavutil

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPT_DIR not in sys.path:
    sys.path.insert(0, SCRIPT_DIR)
from _test_helpers import drain_recv_match  # noqa: E402

# CI sets MAVROUTER_TCP_PORT to an ephemeral port it claimed before
# starting the router; fall back to 5760 for local runs.
TCP_PORT = int(os.environ.get('MAVROUTER_TCP_PORT', 5760))


def main():
    print("Connecting to router...")
    master = mavutil.mavlink_connection(f'tcp:127.0.0.1:{TCP_PORT}')

    print("Waiting for heartbeat...")
    hb = master.recv_match(type='HEARTBEAT', blocking=True, timeout=10)
    if not hb:
        print("No heartbeat received!")
        sys.exit(1)

    src_sys = hb.get_srcSystem()
    src_comp = hb.get_srcComponent()
    print(f"Received Heartbeat from SysID {src_sys} CompID {src_comp}")

    if src_sys == 0:
        print("Invalid SysID 0")
        sys.exit(1)

    # 1. Clear buffer — bounded so a stuck broadcast fails the test fast.
    drain_recv_match(master, deadline_s=2.0)

    # 2. Send Command
    target_comp = src_comp
    if target_comp == 0:
        target_comp = 1

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
