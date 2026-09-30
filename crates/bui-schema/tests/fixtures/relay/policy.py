"""Exercise generated policy routing with loopback fakes; never contact the Internet."""
import copy
import io
import json
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path


def exact(sock, size):
    data = b""
    while len(data) < size:
        part = sock.recv(size - len(data))
        if not part:
            raise EOFError("unexpected EOF")
        data += part
    return data


def address(read):
    kind = read(1)[0]
    if kind == 1:
        host = socket.inet_ntop(socket.AF_INET, read(4))
    elif kind == 3:
        host = read(read(1)[0]).decode()
    elif kind == 4:
        host = socket.inet_ntop(socket.AF_INET6, read(16))
    else:
        raise AssertionError(f"bad address type: {kind}")
    return host, struct.unpack("!H", read(2))[0]


def encoded(host, port):
    try:
        data = b"\x01" + socket.inet_pton(socket.AF_INET, host)
    except OSError:
        name = host.encode()
        data = b"\x03" + bytes([len(name)]) + name
    return data + struct.pack("!H", port)


def bound(kind=socket.SOCK_STREAM, port=0):
    sock = socket.socket(socket.AF_INET, kind)
    sock.bind(("127.0.0.1", port))
    sock.settimeout(5)
    return sock


def header(conn):
    data = b""
    while not data.endswith(b"\r\n\r\n"):
        data += exact(conn, 1)
        assert len(data) < 8192, "oversize synthetic HTTP header"
    return data


class Fake:
    def __init__(self, label, protocol="direct", port=0):
        self.label, self.protocol = label, protocol
        self.sock = bound(port=port)
        self.sock.listen()
        self.port = self.sock.getsockname()[1]
        self.stop = threading.Event()
        self.errors, self.udp_seen, self.destinations = [], [], []
        self.banner_hosts = {"banner.invalid"}
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while not self.stop.is_set():
            try:
                conn, _ = self.sock.accept()
                threading.Thread(target=self.handle, args=(conn,), daemon=True).start()
            except (OSError, TimeoutError):
                pass

    def handle(self, conn):
        try:
            with conn:
                conn.settimeout(10)
                if self.protocol == "socks":
                    assert exact(conn, 1) == b"\x05"
                    assert 0 in exact(conn, exact(conn, 1)[0])
                    conn.sendall(b"\x05\x00")
                    version, command, reserved = exact(conn, 3)
                    assert (version, reserved) == (5, 0)
                    destination = address(lambda n: exact(conn, n))
                    self.destinations.append(destination)
                    if command == 3:
                        with bound(socket.SOCK_DGRAM) as udp:
                            conn.sendall(b"\x05\x00\x00" + encoded("127.0.0.1", udp.getsockname()[1]))
                            while not self.stop.is_set():
                                try:
                                    packet, source = udp.recvfrom(65535)
                                except TimeoutError:
                                    continue
                                reader = io.BytesIO(packet)
                                assert reader.read(3) == b"\x00\x00\x00"
                                target = address(reader.read)
                                self.udp_seen.append((target, source, packet[3]))
                                udp.sendto(packet[:reader.tell()] + self.label.encode() + b":" + reader.read(), source)
                        return
                    assert command == 1
                    conn.sendall(b"\x05\x00\x00" + encoded("127.0.0.1", self.port))
                    if destination[0] in self.banner_hosts:
                        conn.sendall(b"SERVER-BANNER\n")
                        return
                elif self.protocol == "http":
                    request = header(conn)
                    assert request.startswith(b"CONNECT "), request
                    self.destinations.append(request.split(b" ")[1].decode())
                    conn.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                while not self.stop.is_set():
                    request = header(conn)
                    path = request.split(b" ")[1]
                    if path == b"/half":
                        assert conn.recv(1) == b"", "client must half-close upload"
                    size = int(path.split(b"/")[-1]) if path.startswith(b"/bytes/") else 0
                    body = b"x" * size if size else self.label.encode()
                    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: " + str(len(body)).encode() + b"\r\nConnection: keep-alive\r\n\r\n" + body)
                    if path == b"/half":
                        return
        except (EOFError, ConnectionResetError, BrokenPipeError, TimeoutError):
            pass  # Clients intentionally close controls and test half-close.
        except Exception as error:
            self.errors.append(repr(error))

    def close(self):
        self.stop.set()
        self.sock.close()


def socks(port, host="service.invalid", target_port=443, udp=False):
    conn = socket.create_connection(("127.0.0.1", port), timeout=5)
    conn.settimeout(5)
    conn.sendall(b"\x05\x01\x00")
    assert exact(conn, 2) == b"\x05\x00"
    conn.sendall(b"\x05" + (b"\x03" if udp else b"\x01") + b"\x00" + encoded(host, target_port))
    assert exact(conn, 3) == b"\x05\x00\x00"
    reply = address(lambda n: exact(conn, n))
    return conn, reply


def http(conn, host="service.invalid", path="/", half=False):
    conn.sendall(f"GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: keep-alive\r\n\r\n".encode())
    if half:
        conn.shutdown(socket.SHUT_WR)
    response = header(conn)
    length = next(int(line.split(b":", 1)[1]) for line in response.split(b"\r\n") if line.lower().startswith(b"content-length:"))
    return exact(conn, length)


class Kernel:
    def __init__(self, cfg, binary, fakes):
        self.cfg = copy.deepcopy(cfg)
        self.directory = tempfile.TemporaryDirectory()
        self.log = (Path(self.directory.name) / "kernel.log").open("w+")
        # Only relocate addresses, supply offline DNS and disable persistent cache.
        # Preserve all generated policy rules, selectors and protocol capabilities.
        old_to_new, self.ports, reservations = {}, {}, []
        for inbound in self.cfg["inbounds"]:
            reserved = bound()
            reservations.append(reserved)
            new = reserved.getsockname()[1]
            old_to_new[inbound["listen_port"]] = new
            inbound["listen_port"] = new
            self.ports[inbound["tag"]] = new
        for outbound in self.cfg["outbounds"]:
            if outbound.get("tag") == "direct" and "DIRECT" in fakes:
                # For public-literal sniff tests, intercept the terminal direct
                # receiver only. Route decisions and original target metadata
                # stay intact, and no TEST-NET address is dialed externally.
                outbound.clear()
                outbound.update(type="socks", tag="direct", server="127.0.0.1",
                                server_port=fakes["DIRECT"].port, version="5")
                continue
            if outbound.get("type") in ("socks", "http"):
                if outbound["tag"].startswith("resi-egress-"):
                    fake = fakes["A" if outbound["tag"].endswith("0001") else "B"]
                    outbound["server_port"] = fake.port
                elif outbound.get("server_port") in old_to_new:
                    outbound["server_port"] = old_to_new[outbound["server_port"]]
                else:
                    raise AssertionError("expected per-upstream policy wrapper")
        api = bound()
        reservations.append(api)
        self.api = "http://127.0.0.1:" + str(api.getsockname()[1])
        self.cfg["experimental"] = {"clash_api": {"external_controller": self.api.removeprefix("http://")}}
        names = {name: ["127.0.0.1"] for name in ("service.invalid", "a-block.invalid", "b-block.invalid")}
        names["echo.invalid"] = ["203.0.113.9"]
        if "DIRECT" in fakes:
            names.update({name: ["203.0.113.9"] for name in ("a-block.invalid", "b-block.invalid")})
        self.cfg["dns"]["servers"] = [{"type": "hosts", "tag": tag, "predefined": names} for tag in ("dns_direct", "dns_resi")]
        self.cfg["log"] = {"level": "error"}
        config = Path(self.directory.name) / "relay.json"
        config.write_text(json.dumps(self.cfg))
        for reservation in reservations:
            reservation.close()
        self.process = subprocess.Popen([binary, "run", "-c", str(config)], stdout=self.log, stderr=self.log)
        try:
            for _ in range(100):
                assert self.process.poll() is None, "kernel exited before listening"
                try:
                    self.call("/version")
                    break
                except OSError:
                    time.sleep(0.02)
            else:
                raise AssertionError("kernel did not start")
        except Exception:
            self.close(failed=True)
            raise

    def call(self, path, data=None):
        request = urllib.request.Request(self.api + path, data=None if data is None else json.dumps(data).encode(),
                                         headers={"Content-Type": "application/json"}, method="GET" if data is None else "PUT")
        return urllib.request.urlopen(request, timeout=2).read()

    def select(self, slot, member):
        self.call(f"/proxies/slot-{slot}-pool", {"name": f"resi-{member}"})

    def request(self, slot, port, host="service.invalid", host_header=None, **kwargs):
        conn, _ = socks(self.ports[f"slot-{slot}"], host, port)
        with conn:
            return http(conn, host_header or host, **kwargs).decode()

    def close(self, failed=False):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)
        if failed:
            self.log.seek(0)
            print(self.log.read(), file=sys.stderr)
        self.log.close()
        self.directory.cleanup()


def udp_session(kernel, slot, fake, first_host, resolved=True):
    control, relay = socks(kernel.ports[f"slot-{slot}"], "0.0.0.0", 0, udp=True)
    with control, bound(socket.SOCK_DGRAM) as udp:
        before = len(fake.udp_seen)
        for index in range(3):
            payload = f"packet-{index}".encode()
            udp.sendto(b"\x00\x00\x00" + encoded(first_host, 12345) + payload, relay)
            response, _ = udp.recvfrom(4096)
            assert response.endswith(fake.label.encode() + b":" + payload), response
        seen = fake.udp_seen[before:]
        assert len(seen) == 3, seen
        assert len({item[1] for item in seen}) == 1, "upstream source port changed within UDP association"
        if resolved:
            assert seen[0][0] == ("203.0.113.9", 12345), seen
            assert seen[0][2] == 1, "initial UDP destination must be IPv4, not ATYP=domain"


def main():
    data, binary = json.load(sys.stdin), sys.argv[1]
    version = subprocess.check_output([binary, "version"], text=True).splitlines()[0]
    assert version == "sing-box version 1.14.2", f"policy regression requires sing-box 1.14.2, got {version}"
    direct = Fake("DIRECT", port=data["target_port"])
    a, b, http_a = Fake("A", "socks"), Fake("B", "socks"), Fake("HTTP-A", "http")
    direct_socks = Fake("DIRECT", "socks")
    try:
        for case in ("ports", "auto", "plain", "mixed", "sniff"):
            fakes = {"A": http_a if case == "mixed" else a, "B": b}
            if case in ("sniff", "plain"):
                fakes["DIRECT"] = direct_socks
            kernel = Kernel(data["configs"]["auto" if case == "sniff" else case], binary, fakes)
            failed = True
            try:
                port = data["target_port"]
                if case == "ports":
                    assert kernel.request(0, port) == "DIRECT"
                    assert kernel.request(1, port) == "B", "A's disallowed port contaminated B"
                    kernel.select(1, 1)
                    assert kernel.request(1, port) == "DIRECT", "borrowed A did not use A's ports"
                    kernel.select(1, 2)
                    assert kernel.request(1, port) == "B"
                elif case == "auto":
                    for slot, a_result, b_result in ((0, "DIRECT", "A"), (1, "B", "DIRECT")):
                        assert kernel.request(slot, port, "a-block.invalid") == a_result
                        assert kernel.request(slot, port, "b-block.invalid") == b_result
                    old, _ = socks(kernel.ports["slot-1"], "service.invalid", port)
                    with old:
                        assert http(old) == b"B"
                        kernel.select(1, 1)
                        assert http(old) == b"B", "hot switch closed or moved the existing B stream"
                        assert kernel.request(1, port, "a-block.invalid") == "DIRECT"
                        assert kernel.request(1, port, "b-block.invalid") == "A"
                        assert kernel.request(1, port) == "A"
                        kernel.select(1, 2)
                        assert http(old) == b"B"
                        assert kernel.request(1, port, "b-block.invalid") == "DIRECT"
                    assert kernel.request(1, port, path="/half", half=True) == "B", "wrapper lost half-close reply"
                    assert kernel.request(1, port, "127.0.0.1") == "DIRECT", "private literal escaped into provider"
                elif case == "plain":
                    started = time.monotonic()
                    banner, _ = socks(kernel.ports["slot-1"], "banner.invalid", port)
                    with banner:
                        assert exact(banner, 14) == b"SERVER-BANNER\n"
                    elapsed = time.monotonic() - started
                    assert elapsed < 3, f"server-first banner delayed {elapsed:.3f}s"
                    print(f"server-first banner: {elapsed * 1000:.2f}ms", flush=True)
                    udp_session(kernel, 1, b, "203.0.113.9")
                    udp_session(kernel, 1, b, "echo.invalid")
                    kernel.select(1, 1)
                    udp_session(kernel, 1, a, "203.0.113.9")
                elif case == "sniff":
                    assert kernel.request(0, port, "203.0.113.9", host_header="a-block.invalid") == "DIRECT"
                    assert kernel.request(0, port, "203.0.113.9", host_header="b-block.invalid") == "A"
                    assert kernel.request(1, port, "203.0.113.9", host_header="a-block.invalid") == "B"
                    assert kernel.request(1, port, "203.0.113.9", host_header="b-block.invalid") == "DIRECT"
                    kernel.select(1, 1)
                    assert kernel.request(1, port, "203.0.113.9", host_header="a-block.invalid") == "DIRECT"
                    before = {fake: len(fake.destinations) for fake in (a, b, direct_socks)}
                    kernel.request(1, port, "service.invalid", host_header="a-block.invalid")
                    destinations = [target for fake, offset in before.items() for target in fake.destinations[offset:]]
                    assert ("service.invalid", port) in destinations, destinations
                    assert all(target[0] != "a-block.invalid" for target in destinations), "sniff rewrote explicit FQDN destination"
                    udp_session(kernel, 0, direct_socks, "a-block.invalid", resolved=False)
                    udp_session(kernel, 0, a, "b-block.invalid")
                    kernel.select(1, 2)
                    udp_session(kernel, 1, b, "a-block.invalid")
                    udp_session(kernel, 1, direct_socks, "b-block.invalid", resolved=False)
                    kernel.select(1, 1)
                    udp_session(kernel, 1, direct_socks, "a-block.invalid", resolved=False)
                else:
                    assert kernel.request(0, port) == "HTTP-A"
                    assert kernel.request(1, port) == "B"
                    kernel.select(1, 1)
                    assert kernel.request(1, port) == "HTTP-A"
                    # Mixed pools retain their existing fail-open UDP policy. An
                    # unrelated HTTP member must not silently become UDP-capable.
                    rules = kernel.cfg["route"]["rules"]
                    assert any(r.get("network") == "udp" and r.get("port") == 443 and r.get("action") == "reject" for r in rules)
                    before = len(a.udp_seen) + len(b.udp_seen)
                    with bound(socket.SOCK_DGRAM) as echo:
                        control, relay = socks(kernel.ports["slot-1"], "0.0.0.0", 0, udp=True)
                        with control, bound(socket.SOCK_DGRAM) as client:
                            packet = b"\x00\x00\x00" + encoded("127.0.0.1", echo.getsockname()[1]) + b"mixed-direct"
                            client.sendto(packet, relay)
                            received, source = echo.recvfrom(1024)
                            assert received == b"mixed-direct"
                            echo.sendto(received, source)
                            assert client.recvfrom(2048)[0].endswith(received)
                    assert len(a.udp_seen) + len(b.udp_seen) == before
                assert not (a.errors + b.errors + direct.errors + http_a.errors + direct_socks.errors), (a.errors, b.errors, direct.errors, http_a.errors, direct_socks.errors)
                failed = False
                print(f"PASS: {case}", flush=True)
            finally:
                kernel.close(failed)
    finally:
        for fake in (direct, a, b, http_a, direct_socks):
            fake.close()


if __name__ == "__main__":
    main()
