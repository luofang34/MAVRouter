#!/bin/bash
# Detects first available flight controller port
# Returns port path if found, exit 0
# Returns empty if not found, exit 1

PORT=""
if [ -e /dev/ttyACM0 ]; then
    PORT="/dev/ttyACM0"
elif [ -e /dev/ttyUSB0 ]; then
    PORT="/dev/ttyUSB0"
fi

if [ -n "$PORT" ]; then
    echo "$PORT"
    exit 0
else
    exit 1
fi
