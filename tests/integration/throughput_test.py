import sys
import time
import threading
import random
from pymavlink import mavutil

# Configuration
TARGET_IP = '127.0.0.1'
TARGET_PORT = 5760
NUM_CLIENTS = 50
MSG_RATE_PER_CLIENT = 500  # messages per second
DURATION = 30  # seconds

# Shared metrics
total_sent = 0
lock = threading.Lock()

def client_task(client_id):
    global total_sent
    try:
        # Create a unique connection
        master = mavutil.mavlink_connection(f'tcp:{TARGET_IP}:{TARGET_PORT}')
        
        # Wait for heartbeat
        master.wait_heartbeat(timeout=5)
        
        start_time = time.time()
        local_sent = 0
        next_log = start_time + 1.0
        
        while time.time() - start_time < DURATION:
            now = time.time()
            
            # Send Heartbeat (1Hz)
            if local_sent % MSG_RATE_PER_CLIENT == 0:
                master.mav.heartbeat_send(
                    mavutil.mavlink.MAV_TYPE_GCS,
                    mavutil.mavlink.MAV_AUTOPILOT_INVALID,
                    0, 0, 0
                )
            
            # Send High Frequency Ping
            master.mav.ping_send(
                int(now * 1000000),
                local_sent,
                0, 0 # Broadcast
            )
            local_sent += 1
            
            # Rate limiting
            time.sleep(1.0 / MSG_RATE_PER_CLIENT)
            
        with lock:
            total_sent += local_sent
            
        master.close()
        
    except Exception as e:
        print(f"[Client {client_id}] Error: {e}")

def main():
    print(f"Starting Throughput Test: {NUM_CLIENTS} clients, {MSG_RATE_PER_CLIENT} msg/s each")
    print(f"Target Total Load: {NUM_CLIENTS * MSG_RATE_PER_CLIENT} msg/s")
    print(f"Duration: {DURATION}s")
    
    threads = []
    start_global = time.time()
    
    for i in range(NUM_CLIENTS):
        t = threading.Thread(target=client_task, args=(i,))
        t.start()
        threads.append(t)
        
    for t in threads:
        t.join()
        
    total_duration = time.time() - start_global
    total_rate = total_sent / total_duration
    
    print("-" * 40)
    print(f"Total Messages Sent: {total_sent}")
    print(f"Actual Duration: {total_duration:.2f}s")
    print(f"Aggregate Throughput: {total_rate:.2f} msg/s")
    
    # Pass criteria: At least 80% of target throughput
    target_rate = NUM_CLIENTS * MSG_RATE_PER_CLIENT
    if total_rate > target_rate * 0.8:
        print("✅ PASSED: High Throughput Test")
        sys.exit(0)
    else:
        print(f"❌ FAILED: Throughput too low (<{target_rate * 0.8:.0f} msg/s)")
        sys.exit(1)

if __name__ == "__main__":
    main()
