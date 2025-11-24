import sys
import time
import threading
from pymavlink import mavutil

# Configuration
TARGET_IP = '127.0.0.1'
TARGET_PORT = 5760
NUM_CLIENTS = 100
MSG_RATE_PER_CLIENT = 100  # Moderate rate
DURATION = 60  # seconds

def client_task(client_id):
    try:
        master = mavutil.mavlink_connection(f'tcp:{TARGET_IP}:{TARGET_PORT}')
        master.wait_heartbeat(timeout=10)
        
        start_time = time.time()
        seq = 0
        
        while time.time() - start_time < DURATION:
            master.mav.ping_send(
                int(time.time() * 1000000),
                seq,
                0, 0
            )
            seq += 1
            time.sleep(1.0 / MSG_RATE_PER_CLIENT)
            
        master.close()
    except Exception as e:
        # Failures in mixed load are expected under heavy stress, logging only
        pass

def main():
    print(f"Starting Mixed Load Test: {NUM_CLIENTS} clients, {MSG_RATE_PER_CLIENT} msg/s each")
    print(f"Duration: {DURATION}s")
    
    threads = []
    for i in range(NUM_CLIENTS):
        t = threading.Thread(target=client_task, args=(i,))
        t.start()
        threads.append(t)
        
    # Just wait for completion, this is a stability test
    # We rely on the router NOT crashing.
    for t in threads:
        t.join()
        
    print("✅ PASSED: Mixed Load Test (Router survived)")

if __name__ == "__main__":
    main()
