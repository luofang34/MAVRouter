import sys
import time
import asyncio
import socket
import argparse

# --- Dependency Check ---
try:
    from pymavlink.dialects.v20 import common as mavlink2
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


class ByteCapture:
    def __init__(self):
        self.data = bytearray()

    def write(self, buf):
        if isinstance(buf, (bytes, bytearray)):
            self.data.extend(buf)
        return len(buf) if isinstance(buf, (bytes, bytearray)) else 0

    def read(self, _n):
        return b''


def prebuild_ping_frames(system_id, component_id=1):
    """Pre-build 256 PING frames with incrementing sequences."""
    frames = []
    cap = ByteCapture()
    mav = mavlink2.MAVLink(cap, srcSystem=system_id, srcComponent=component_id)
    for seq in range(256):
        cap.data = bytearray()
        mav.seq = seq
        mav.ping_send(0, seq, 0, 0)
        frames.append(bytes(cap.data))
    return frames


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


async def client_coro(client_id, target_ip, target_port, msg_rate_per_client, stop_event):
    try:
        reader, writer = await asyncio.open_connection(target_ip, target_port)

        # Drain incoming data in background to prevent buffer fill
        async def drain_reader():
            try:
                while True:
                    data = await reader.read(4096)
                    if not data:
                        break
            except Exception:
                pass

        drain_task = asyncio.create_task(drain_reader())

        system_id = (client_id % 254) + 1
        frames = prebuild_ping_frames(system_id)

        local_sent = 0
        interval = 1.0 / msg_rate_per_client

        while not stop_event.is_set():
            writer.write(frames[local_sent % 256])
            local_sent += 1
            # Periodic drain to flush writes to socket
            if local_sent % 50 == 0:
                try:
                    await asyncio.wait_for(writer.drain(), timeout=0.5)
                except asyncio.TimeoutError:
                    pass
            await asyncio.sleep(interval)

        # Final flush
        try:
            await asyncio.wait_for(writer.drain(), timeout=2.0)
        except (asyncio.TimeoutError, Exception):
            pass

        drain_task.cancel()
        writer.close()
        return local_sent
    except Exception as e:
        print(f"Client {client_id} error: {e}", file=sys.stderr)
        return 0


async def run_load_round(round_name, duration, num_clients, msg_rate_per_client):
    print(f"\n--- {round_name} ({duration}s) ---")
    print(f"Clients: {num_clients}, Rate/Client: {msg_rate_per_client} msg/s")
    target_load = num_clients * msg_rate_per_client
    print(f"Target Load: {target_load} msg/s")

    start_mem = get_process_memory()
    stop_event = asyncio.Event()

    start_time = time.time()

    tasks = [
        asyncio.create_task(
            client_coro(i, TARGET_IP, TARGET_PORT, msg_rate_per_client, stop_event)
        )
        for i in range(num_clients)
    ]

    await asyncio.sleep(duration)
    stop_event.set()

    results = await asyncio.gather(*tasks, return_exceptions=True)

    actual_duration = time.time() - start_time
    total_sent = sum(r for r in results if isinstance(r, int))
    throughput = total_sent / actual_duration
    end_mem = get_process_memory()
    mem_growth = end_mem - start_mem

    print(f"Sent: {total_sent} msgs in {actual_duration:.2f}s")
    print(f"Throughput: {throughput:.0f} msg/s")
    if psutil:
        print(f"Memory: {start_mem:.1f}MB -> {end_mem:.1f}MB (Growth: {mem_growth:+.1f}MB)")

    return throughput, mem_growth


async def main():
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
        t1, m1 = await run_load_round("Round 1: Warm-up", 5, 10, 100)

        # Round 2: High Load (10s, 50 clients, 10000 msg/s total)
        t2, m2 = await run_load_round("Round 2: High Load", 10, 50, 200)

        # Round 3: Extreme Load (10s, 100 clients, 20000 msg/s total)
        t3, m3 = await run_load_round("Round 3: Extreme Load", 10, 100, 200)

        threshold_r2 = 8000
        threshold_r3 = 12000
        max_mem_growth = 50.0

    elif args.profile == 'serial':
        # Serial Stress (Bandwidth/Latency bound)
        # Targeting 115200 baud (~11.5 KB/s). Approx 200 msg/s @ 50 bytes.

        # Round 1: Warm-up (5s, 1 client, 50 msg/s)
        t1, m1 = await run_load_round("Round 1: Warm-up", 5, 1, 50)

        # Round 2: Saturation (10s, 1 client, 150 msg/s)
        # Should be handled comfortably by 115200
        t2, m2 = await run_load_round("Round 2: Saturation", 10, 1, 150)

        # Round 3: Overload (10s, 1 client, 300 msg/s)
        # Exceeds bandwidth. Expect buffering/drops, but process should stay stable.
        t3, m3 = await run_load_round("Round 3: Overload", 10, 1, 300)

        threshold_r2 = 140 # Expect >= 100 msg/s (target 150)
        threshold_r3 = 250 # Expect >= 150 msg/s even if target is 300 (saturation)
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
    asyncio.run(main())
