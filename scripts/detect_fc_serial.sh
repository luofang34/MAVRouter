#!/usr/bin/env bash
# Detects first available flight controller port
# Supports Linux (/dev/ttyACM*, /dev/ttyUSB*) and macOS (/dev/tty.usbmodem*, /dev/tty.usbserial*)
# Returns port path if found, exit 0
# Returns empty if not found, exit 1

OS="$(uname -s)"
PORT=""

case "$OS" in
    Linux)
        # Check common Linux paths
        for p in /dev/ttyACM* /dev/ttyUSB*; do
            if [ -e "$p" ]; then
                PORT="$p"
                break
            fi
        done
        ;;
    Darwin)
        # Check common macOS paths
        for p in /dev/tty.usbmodem* /dev/tty.usbserial*; do
            if [ -e "$p" ]; then
                PORT="$p"
                break
            fi
        done
        ;;
    *)
        # Unknown OS fallback
        # This case is less likely to occur on typical CI/Dev machines but provides a safe default.
        if [ -e /dev/ttyACM0 ]; then PORT="/dev/ttyACM0"; fi
        ;;
esac

if [ -n "$PORT" ]; then
    echo "$PORT"
    exit 0
else
    exit 1
fi
