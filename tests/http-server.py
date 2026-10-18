#!/usr/bin/env python3
"""Python's HTTP test server with a local, deterministic server_name."""

import runpy
import socket

# HTTPServer uses getfqdn only to populate server_name. Some hosted macOS
# runners stall in reverse DNS before listen(), which is unrelated to the
# process and socket behavior this fixture exercises.
socket.getfqdn = lambda name="": name
runpy.run_module("http.server", run_name="__main__")
