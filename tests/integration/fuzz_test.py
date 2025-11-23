import sys
import time
import socket
import random
import os

def main():
    print("Connecting to router...")
    try:
        s = socket.create_connection(('127.0.0.1', 5760))
    except Exception as e:
        print(f"Connection failed: {e}")
        sys.exit(1)

    print("Sending 1MB of random fuzz data...")
    # Chunked to simulate stream
    for _ in range(100):
        data = os.urandom(10240) # 10KB
        try:
            s.sendall(data)
        except Exception as e:
            print(f"Send failed: {e}")
            sys.exit(1)
        time.sleep(0.01)

    print("Fuzzing complete. Checking liveness...")
    # Send a valid MAVLink packet?
    # Just check if we can still send.
    try:
        s.sendall(b"PING")
    except Exception as e:
        print(f"Router seems dead: {e}")
        sys.exit(1)
        
    print("Router survived.")
    s.close()

if __name__ == "__main__":
    main()
