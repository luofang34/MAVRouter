"""Shared test helpers for the integration test suite.

CLAUDE.md requires every blocking socket / mavlink / asyncio operation to
have a wall-clock deadline so a wedged router fails the test fast instead
of hanging until the GitHub Actions job-level timeout (120 s) bails it
out.

Three patterns recur across the suite and motivate this module:

* `wait_for_tcp(host, port, timeout)` replaces three near-identical
  copies (verify_config_dir.py, verify_tlog.py, loop_stress_test.py:
  `wait_for_port`) of the same connect-then-close probe.

* `drain_recv_match(connection, deadline_s)` bounds the bare
  `while connection.recv_match(blocking=False): pass` drain loops in
  verify_filtering.py and friends. The naive form spins forever if the
  router never stops sending; the helper raises `TestTimeout` so the
  test fails with a useful message.

* `collect_until(connection, predicate, timeout_s)` bounds the
  `while True: msg = recv_match(blocking=False); if predicate(msg): ...`
  collector pattern with a wall-clock deadline.

Import as `from _test_helpers import ...`. Python adds each script's
directory to `sys.path` automatically (whether the script is launched
from the repo root or from `tests/integration`), so no path massaging
is required at the call site.
"""

import socket
import time


class TestTimeout(Exception):
    """Raised when a wrapped operation exceeds its wall-clock deadline."""


def wait_for_tcp(host: str, port: int, timeout: float = 10.0) -> bool:
    """Probe `host:port` until a TCP connect succeeds or `timeout` elapses.

    Each individual connect attempt is capped at 1 s so a hung port that
    accepts SYN but never completes the handshake doesn't burn the full
    budget on a single attempt.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.settimeout(1.0)
            sock.connect((host, port))
            sock.close()
            return True
        except (socket.error, socket.timeout):
            time.sleep(0.1)
    return False


def drain_recv_match(connection, deadline_s: float = 2.0) -> int:
    """Drain pending non-blocking `recv_match` calls, capped by wall-clock.

    Returns the number of messages drained. Raises `TestTimeout` if the
    drain didn't converge — i.e. the router kept feeding messages for
    longer than `deadline_s`, which means the test's preconditions are
    not what the test thinks they are.
    """
    deadline = time.monotonic() + deadline_s
    drained = 0
    while time.monotonic() < deadline:
        if connection.recv_match(blocking=False) is None:
            return drained
        drained += 1
    raise TestTimeout(
        f"drain_recv_match did not converge within {deadline_s}s "
        f"(drained {drained} messages and the router is still sending) — "
        f"check whether a stream is supposed to be quiet at this point."
    )


def collect_until(connection, predicate, timeout_s: float):
    """Poll `recv_match(blocking=False)` until `predicate(msg)` is truthy.

    Raises `TestTimeout` on deadline. `BAD_DATA` messages and `None`
    returns are skipped automatically — every call site we have today
    discards them.
    """
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        msg = connection.recv_match(blocking=False)
        if msg is None:
            time.sleep(0.005)
            continue
        if msg.get_type() == 'BAD_DATA':
            continue
        if predicate(msg):
            return msg
    raise TestTimeout(
        f"collect_until did not see a matching message within {timeout_s}s"
    )
