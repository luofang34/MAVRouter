import sys
import time
import threading
import os
import multiprocessing

# --- Dependency Check ---
try:
    from pymavlink import mavutil
except ImportError:
    print("Error: pymavlink not installed. Please install it using:")
    print("  pip install pymavlink")
    print("Or from the project's requirements.txt:")
    print("  pip install -r tests/integration/requirements.txt")
    sys.exit(1)

try:
    import psutil
except ImportError:
    psutil = None
    print("Warning: psutil not installed. Memory checks will be disabled.")
    print("  To enable memory checks, please install it using:")
    print("  pip install psutil")
    print("Or from the project's requirements.txt:")
    print("  pip install -r tests/integration/requirements.txt")

# --- End Dependency Check ---

# Configuration
TARGET_IP = '127.0.0.1'
TARGET_PORT = 5760

def get_process_memory():
    """Returns RSS memory in MB"""
    if not psutil:
        return 0.0
    try:
        # Find mavrouter-rs process
        for proc in psutil.process_iter(['pid', 'name', 'cmdline']):
            if 'mavrouter-rs' in proc.info['name'] or \
               (proc.info['cmdline'] and 'mavrouter-rs' in proc.info['cmdline'][0]):
                return proc.memory_info().rss / 1024 / 1024
    except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
        pass
    return 0.0

def run_load_round(round_name, duration, num_clients, msg_rate_per_client):
    print(f"\n--- {round_name} ({duration}s) ---")
    print(f"Clients: {num_clients}, Rate/Client: {msg_rate_per_client} msg/s")
    print(f"Target Load: {num_clients * msg_rate_per_client} msg/s")

    start_mem = get_process_memory()
    
    threads = []
    total_sent = [0] # List to allow modification in threads
    lock = threading.Lock()
    stop_event = threading.Event()

    import select

# ...

def client_task(client_id):
    try:
        master = mavutil.mavlink_connection(f'tcp:{TARGET_IP}:{TARGET_PORT}')
        # Wait for heartbeat efficiently (don't block forever)
        master.wait_heartbeat(timeout=2)
        
        local_sent = 0
        
        while not stop_event.is_set():
            # Drain incoming messages to prevent router buffer fill
            while True:
                # Check if data is available to read
                r, _, _ = select.select([master.fd], [], [], 0)
                if not r:
                    break
                # Parse message (non-blocking since data is available, mostly)
                if master.recv_msg() is None:
                    break

            # Send Ping
            master.mav.ping_send(
                int(time.time() * 1000000),
                local_sent,
                0, 0
            )
            local_sent += 1
            
            # Simple rate limiting
            time.sleep(1.0 / msg_rate_per_client)
            
        with lock:
            total_sent[0] += local_sent
        master.close()
    except Exception as e:
        pass

    # Start clients
    start_time = time.time()
    for i in range(num_clients):
        t = threading.Thread(target=client_task, args=(i,)) 
        t.start()
        threads.append(t)

    # Run for duration
    time.sleep(duration)
    stop_event.set()

    # Wait for completion
    for t in threads:
        t.join()

    actual_duration = time.time() - start_time
    throughput = total_sent[0] / actual_duration
    end_mem = get_process_memory()
    mem_growth = end_mem - start_mem

    print(f"Sent: {total_sent[0]} msgs in {actual_duration:.2f}s")
    print(f"Throughput: {throughput:.0f} msg/s")
    if psutil:
        print(f"Memory: {start_mem:.1f}MB -> {end_mem:.1f}MB (Growth: {mem_growth:+.1f}MB)")
    
    return throughput, mem_growth

def main():
    print("Starting High Load Loop Stress Test...")

    # Round 1: Warm-up (5s, 10 clients, 1000 msg/s total)
    # Reduced per-client rate to 100 to hit 1000 total
    t1, m1 = run_load_round("Round 1: Warm-up", 5, 10, 100)
    
    # Round 2: High Load (10s, 50 clients, 10000 msg/s total)
    # 200 msg/s per client
    t2, m2 = run_load_round("Round 2: High Load", 10, 50, 200)
    
    # Round 3: Extreme Load (10s, 100 clients, 20000 msg/s total)
    # 200 msg/s per client
    t3, m3 = run_load_round("Round 3: Extreme Load", 10, 100, 200)

    print("\n=== Test Summary ===")
    success = True
    
    # Throughput check (Round 2 & 3 should sustain reasonable load)
    # Targets are lower than theoretical max to account for CI environment variance
    if t2 < 5000: 
        print("❌ Round 2 Throughput too low (<5000)")
        success = False
    if t3 < 10000:
        print("❌ Round 3 Throughput too low (<10000)")
        success = False
        
    # Memory Check
    if m3 > 5.0:
        print(f"❌ Memory leak detected in Round 3 (>5MB growth): {m3:.1f}MB")
        success = False
    elif m3 > 0:
        print(f"✅ Memory stable (Growth: {m3:.1f}MB)")
    
    if success:
        print("✅ PASSED: Loop Stress Test")
        sys.exit(0)
    else:
        print("❌ FAILED: Loop Stress Test")
        sys.exit(1)

if __name__ == "__main__":
    main()
