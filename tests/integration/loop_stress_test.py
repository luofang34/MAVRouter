import sys
import time
import threading
import os
import multiprocessing
import select
import socket
import argparse

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

def wait_for_port(ip, port, timeout=10):
    start = time.time()
    while time.time() - start < timeout:
        try:
            with socket.create_connection((ip, port), timeout=1):
                print(f"Port {port} is open.")
                return True
        except (ConnectionRefusedError, OSError):
            time.sleep(0.1)
    print(f"Timeout waiting for port {port}.")
    return False

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
    target_load = num_clients * msg_rate_per_client
    print(f"Target Load: {target_load} msg/s")

    start_mem = get_process_memory()
    
    threads = []
    total_sent = [0] # List to allow modification in threads
    lock = threading.Lock()
    stop_event = threading.Event()

    def client_task(client_id):
        try:
            master = mavutil.mavlink_connection(f'tcp:{TARGET_IP}:{TARGET_PORT}')
            # Wait for heartbeat efficiently (don't block forever)
            master.wait_heartbeat(timeout=2)
            
            local_sent = 0
            
            while not stop_event.is_set():
                # Drain incoming messages to prevent router buffer fill
                # Use raw recv for performance (we don't need to parse incoming messages)
                while True:
                    try:
                        # Check if data is available to read
                        r, _, _ = select.select([master.fd], [], [], 0)
                        if not r:
                            break
                        # Read raw data (up to 4096 bytes) and discard
                        data = master.fd.recv(4096)
                        if not data: # EOF
                            break
                    except (BlockingIOError, InterruptedError):
                        break
                    except Exception:
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
            # Print exception for debugging purposes if a thread fails
            print(f"Client {client_id} error: {e}", file=sys.stderr)
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
    parser = argparse.ArgumentParser(description='MAVRouter Load Stress Test')
    parser.add_argument('--profile', type=str, default='router', choices=['router', 'serial'],
                        help='Test profile: router (high load) or serial (baud rate limited)')
    args = parser.parse_args()

    print(f"Starting Loop Stress Test (Profile: {args.profile})...")

    if not wait_for_port(TARGET_IP, TARGET_PORT):
        print("❌ Router port not reachable. Exiting.")
        sys.exit(1)

    if args.profile == 'router':
        # Router Stress (CPU/Memory bound)
        # Round 1: Warm-up (5s, 10 clients, 1000 msg/s total)
        t1, m1 = run_load_round("Round 1: Warm-up", 5, 10, 100)
        
        # Round 2: High Load (10s, 50 clients, 10000 msg/s total)
        t2, m2 = run_load_round("Round 2: High Load", 10, 50, 200)
        
        # Round 3: Extreme Load (10s, 100 clients, 20000 msg/s total)
        t3, m3 = run_load_round("Round 3: Extreme Load", 10, 100, 200)

        threshold_r2 = 5000
        # Baseline: ~9,000 msg/s with per-client EndpointId (required for correct multiclient routing)
        # Per-client ID has ~20% overhead vs shared ID but is necessary for TCP server clients
        # to correctly forward messages between each other
        threshold_r3 = 9000
        max_mem_growth = 50.0

    elif args.profile == 'serial':
        # Serial Stress (Bandwidth/Latency bound)
        # Targeting 115200 baud (~11.5 KB/s). Approx 200 msg/s @ 50 bytes.
        
        # Round 1: Warm-up (5s, 1 client, 50 msg/s)
        t1, m1 = run_load_round("Round 1: Warm-up", 5, 1, 50)
        
        # Round 2: Saturation (10s, 1 client, 150 msg/s)
        # Should be handled comfortably by 115200
        t2, m2 = run_load_round("Round 2: Saturation", 10, 1, 150)
        
        # Round 3: Overload (10s, 1 client, 300 msg/s)
        # Exceeds bandwidth. Expect buffering/drops, but process should stay stable.
        t3, m3 = run_load_round("Round 3: Overload", 10, 1, 300)

        threshold_r2 = 100 # Expect >= 100 msg/s (target 150)
        threshold_r3 = 150 # Expect >= 150 msg/s even if target is 300 (saturation)
        max_mem_growth = 20.0 # Stricter memory check for lower load

    print("\n=== Test Summary ===")
    success = True
    
    if t2 < threshold_r2: 
        print(f"❌ Round 2 Throughput too low (<{threshold_r2})")
        success = False
    if t3 < threshold_r3:
        print(f"❌ Round 3 Throughput too low (<{threshold_r3})")
        success = False
        
    # Memory Check
    if m3 > max_mem_growth:
        print(f"❌ Memory leak detected in Round 3 (>{max_mem_growth}MB growth): {m3:.1f}MB")
        success = False
    elif m3 > 0:
        print(f"✅ Memory stable (Growth: {m3:.1f}MB)")
    
    if success:
        print(f"✅ PASSED: Loop Stress Test ({args.profile})")
        sys.exit(0)
    else:
        print(f"❌ FAILED: Loop Stress Test ({args.profile})")
        sys.exit(1)

if __name__ == "__main__":
    main()