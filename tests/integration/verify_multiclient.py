import sys
import time
import threading
from pymavlink import mavutil

# Shared state
client_results = [False, False]

def client_task(index):
    try:
        # Create a unique connection
        # Note: socket.create_connection might need unique source ports but OS handles that.
        master = mavutil.mavlink_connection('tcp:127.0.0.1:5760')
        
        print(f"[Client {index}] Connected. Waiting for heartbeat...")
        
        # Wait for heartbeat (max 10s)
        hb = master.recv_match(type='HEARTBEAT', blocking=True, timeout=10)
        if hb:
            print(f"[Client {index}] Received Heartbeat from SysID {hb.get_srcSystem()} CompID {hb.get_srcComponent()}")
            client_results[index] = True
        else:
            print(f"[Client {index}] No heartbeat received (Timeout).")
            
        master.close()
    except Exception as e:
        print(f"[Client {index}] Error: {e}")

def main():
    print("Starting Multi-Client Test (2 Simultaneous TCP Clients)...")
    
    t1 = threading.Thread(target=client_task, args=(0,))
    t2 = threading.Thread(target=client_task, args=(1,))
    
    t1.start()
    t2.start()
    
    t1.join()
    t2.join()
    
    if all(client_results):
        print("SUCCESS: Both clients received data (Broadcast worked).")
        sys.exit(0)
    else:
        print("FAILURE: One or more clients failed to receive data.")
        sys.exit(1)

if __name__ == "__main__":
    main()
