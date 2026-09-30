"""Offline packet regression for sing-box selector/SOCKS5 UDP forwarding."""
import io
import json
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


def exact(sock, size):
    data = b""
    while len(data) < size:
        chunk = sock.recv(size - len(data))
        if not chunk:
            raise EOFError("SOCKS5 control connection closed")
        data += chunk
    return data


def address(read):
    kind = read(1)[0]
    if kind == 1:
        host = socket.inet_ntop(socket.AF_INET, read(4))
    elif kind == 4:
        host = socket.inet_ntop(socket.AF_INET6, read(16))
    elif kind == 3:
        host = read(read(1)[0]).decode()
    else:
        raise AssertionError(f"unknown address type {kind}")
    return host, struct.unpack("!H", read(2))[0]


def bound(kind):
    sock = socket.socket(socket.AF_INET, kind)
    sock.bind(("127.0.0.1", 0))
    sock.settimeout(5)
    return sock


def main():
    cfg = json.load(sys.stdin)
    with bound(socket.SOCK_STREAM) as gateway, bound(socket.SOCK_DGRAM) as upstream:
        gateway.listen()
        done = threading.Event()
        errors = []

        def control(conn):
            with conn:
                try:
                    assert exact(conn, 1) == b"\x05"
                    methods = exact(conn, exact(conn, 1)[0])
                    assert 0 in methods
                    conn.sendall(b"\x05\x00")
                    assert exact(conn, 3) == b"\x05\x03\x00"
                    address(lambda n: exact(conn, n))
                    conn.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01" + struct.pack("!H", upstream.getsockname()[1]))
                    done.wait(20)
                except Exception as e:
                    errors.append(repr(e))

        def accept():
            while not done.is_set():
                try:
                    conn, _ = gateway.accept()
                except (TimeoutError, OSError):
                    continue
                threading.Thread(target=control, args=(conn,), daemon=True).start()

        threading.Thread(target=accept, daemon=True).start()
        # Only relocate listeners and replace DNS with a deterministic, offline
        # answer. Keep the generated routing, SOCKS outbound and selectors intact.
        # Relocate both slot and policy listeners. Replacing resi-1 directly
        # would bypass the policy hop and silently stop testing the real path.
        reservations = [bound(socket.SOCK_STREAM) for _ in cfg["inbounds"]]
        ports = {}
        for inbound, reserved in zip(cfg["inbounds"], reservations):
            ports[inbound["listen_port"]] = reserved.getsockname()[1]
            inbound["listen_port"] = reserved.getsockname()[1]
        listen_port = cfg["inbounds"][0]["listen_port"]
        raw_count = 0
        for outbound in cfg["outbounds"]:
            if outbound["tag"].startswith("resi-egress-"):
                outbound["server_port"] = gateway.getsockname()[1]
                raw_count += 1
            elif outbound.get("type") == "socks" and outbound.get("server") == "127.0.0.1":
                outbound["server_port"] = ports[outbound["server_port"]]
        assert raw_count == 1, "fixture must exercise one real policy endpoint"
        for reserved in reservations:
            reserved.close()
        cfg["dns"]["servers"] = [
            {"type": "hosts", "tag": tag, "predefined": {"echo.example.invalid": ["203.0.113.9"]}}
            for tag in ("dns_direct", "dns_resi")
        ]
        cfg.pop("experimental", None)
        cfg["log"] = {"level": "debug"}
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "relay.json"
            config.write_text(json.dumps(cfg))
            with (Path(directory) / "kernel.log").open("w+") as log:
                kernel = subprocess.Popen([sys.argv[1], "run", "-c", str(config)], stdout=log, stderr=log)
                try:
                    client = None
                    for _ in range(100):
                        assert kernel.poll() is None, "relay exited before listening"
                        try:
                            client = socket.create_connection(("127.0.0.1", listen_port), timeout=0.1)
                            break
                        except OSError:
                            time.sleep(0.02)
                    assert client is not None, "relay did not start"
                    client.close()
                    ip = b"\x01\xcb\x00\x71\x09"
                    domain = b"\x03\x14echo.example.invalid"
                    # A SOCKS5 association can carry more than one destination.
                    # The first packet is routed/resolved; later destinations
                    # must also survive the selector's packet-connection wrapper.
                    sessions = [
                        [(ip, "203.0.113.9"), (domain, "echo.example.invalid")],
                        [(domain, "203.0.113.9")],
                    ]
                    for messages in sessions:
                        with socket.create_connection(("127.0.0.1", listen_port), timeout=5) as client, bound(socket.SOCK_DGRAM) as udp:
                            client.settimeout(5)
                            client.sendall(b"\x05\x01\x00")
                            assert exact(client, 2) == b"\x05\x00"
                            client.sendall(b"\x05\x03\x00\x01\x00\x00\x00\x00\x00\x00")
                            assert exact(client, 3) == b"\x05\x00\x00"
                            _, port = address(lambda n: exact(client, n))
                            for target, expected_host in messages:
                                payload = b"bui-udp-roundtrip"
                                packet = b"\x00\x00\x00" + target + struct.pack("!H", 12345) + payload
                                udp.sendto(packet, ("127.0.0.1", port))
                                received, sender = upstream.recvfrom(2048)
                                reader = io.BytesIO(received)
                                assert reader.read(3) == b"\x00\x00\x00"
                                assert address(reader.read) == (expected_host, 12345), received.hex()
                                assert reader.read() == payload
                                upstream.sendto(received, sender)
                                reply, _ = udp.recvfrom(2048)
                                assert reply.endswith(payload), reply.hex()
                    assert not errors, errors
                    print("PASS: residential UDP resolves initial domains, preserves later address changes, and returns replies")
                except Exception:
                    log.flush()
                    log.seek(0)
                    print(log.read(), file=sys.stderr)
                    print("mock errors:", errors, file=sys.stderr)
                    raise
                finally:
                    done.set()
                    kernel.terminate()
                    kernel.wait(timeout=5)


if __name__ == "__main__":
    main()
