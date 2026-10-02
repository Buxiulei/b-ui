"""Execute the required residential exit contract against stock sing-box offline.

Only listener addresses and terminal transports are relocated. Route matchers,
selectors, target metadata, DNS detours and protocol capabilities remain the
renderer output. The observed VPS transport detects direct fallback without
ever contacting a public TEST-NET target.
"""
import copy
import hashlib
import io
import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from policy import address, bound, exact, header


PUBLIC = "203.0.113.9"
PUBLIC6 = "2001:db8::9"
SIZE = 131071


def encoded(host, port):
    for family, kind in ((socket.AF_INET, 1), (socket.AF_INET6, 4)):
        try:
            return bytes([kind]) + socket.inet_pton(family, host) + struct.pack("!H", port)
        except OSError:
            pass
    name = host.encode()
    return b"\x03" + bytes([len(name)]) + name + struct.pack("!H", port)


def payload(nonce):
    block = hashlib.sha256(nonce.encode()).digest()
    return (block * (SIZE // len(block) + 1))[:SIZE]


def dns_query(nonce):
    labels = [nonce.encode(), b"required", b"invalid"]
    return b"\x71\x92\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00" + b"".join(
        bytes([len(label)]) + label for label in labels
    ) + b"\x00\x00\x01\x00\x01"


def dns_reply(query):
    assert len(query) >= 17, "synthetic DNS query is incomplete"
    end = 12
    while query[end] != 0:
        end += 1 + query[end]
        assert end < len(query), "synthetic DNS query has truncated labels"
    end += 5  # terminating label, QTYPE, QCLASS; exclude optional EDNS records.
    return query[:2] + b"\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00" + query[12:end] + (
        b"\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x01\x00\x04"
        + socket.inet_pton(socket.AF_INET, PUBLIC)
    )


class Receiver:
    """A terminal fake that receives the complete original target and payload."""

    def __init__(self, label, protocol="socks"):
        self.label, self.protocol = label, protocol
        self.sock = bound()
        self.sock.listen()
        self.port = self.sock.getsockname()[1]
        self.stop = threading.Event()
        self.targets, self.business, self.udp, self.dns, self.errors = [], [], [], [], []
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while not self.stop.is_set():
            try:
                conn, _ = self.sock.accept()
                threading.Thread(target=self.handle, args=(conn,), daemon=True).start()
            except (OSError, TimeoutError):
                pass

    def counts(self):
        return len(self.targets), len(self.business), len(self.udp), len(self.dns)

    def udp_association(self, conn):
        with bound(socket.SOCK_DGRAM) as udp:
            udp.settimeout(0.1)
            conn.sendall(b"\x05\x00\x00" + encoded("127.0.0.1", udp.getsockname()[1]))
            while not self.stop.is_set():
                try:
                    packet, source = udp.recvfrom(65535)
                except TimeoutError:
                    continue
                reader = io.BytesIO(packet)
                assert reader.read(3) == b"\x00\x00\x00"
                target = address(reader.read)
                data = reader.read()
                self.udp.append((target, data))
                if target[1] == 53:
                    self.dns.append((target, data))
                    response = dns_reply(data)
                else:
                    response = self.label.encode() + b":" + data
                udp.sendto(b"\x00\x00\x00" + encoded(*target) + response, source)

    def handle(self, conn):
        try:
            with conn:
                conn.settimeout(3)
                if self.protocol == "socks":
                    assert exact(conn, 1) == b"\x05"
                    assert 0 in exact(conn, exact(conn, 1)[0])
                    conn.sendall(b"\x05\x00")
                    version, command, reserved = exact(conn, 3)
                    assert (version, reserved) == (5, 0)
                    target = address(lambda size: exact(conn, size))
                    if command == 3:
                        self.udp_association(conn)
                        return
                    assert command == 1
                    self.targets.append(target)
                    conn.sendall(b"\x05\x00\x00" + encoded("127.0.0.1", self.port))
                else:
                    request = header(conn)
                    assert request.startswith(b"CONNECT "), request
                    authority = request.split(b" ")[1].decode()
                    host, port = authority.rsplit(":", 1)
                    target = host.strip("[]"), int(port)
                    self.targets.append(target)
                    conn.sendall(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                if target[1] == 53:
                    while not self.stop.is_set():
                        size = struct.unpack("!H", exact(conn, 2))[0]
                        query = exact(conn, size)
                        self.dns.append((target, query))
                        reply = dns_reply(query)
                        conn.sendall(struct.pack("!H", len(reply)) + reply)
                    return
                request = header(conn)
                assert request.startswith(b"POST /required/"), request
                nonce = request.split(b" ")[1].decode().rsplit("/", 1)[1]
                size = next(
                    int(line.split(b":", 1)[1]) for line in request.split(b"\r\n")
                    if line.lower().startswith(b"content-length:")
                )
                data = exact(conn, size)
                assert data == payload(nonce), "receiver observed corrupted/truncated nonce upload"
                self.business.append((target, nonce, hashlib.sha256(data).hexdigest()))
                response = self.label.encode() + b":" + data[::-1]
                conn.sendall(
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: "
                    + str(len(response)).encode() + b"\r\n\r\n" + response
                )
        except (EOFError, ConnectionResetError, BrokenPipeError, TimeoutError):
            pass  # Kernel/client shutdown deliberately closes long-lived controls.
        except Exception as error:
            self.errors.append(repr(error))

    def close(self):
        self.stop.set()
        self.sock.close()


def socks(port, host, target_port, udp=False):
    conn = socket.create_connection(("127.0.0.1", port), timeout=2)
    conn.settimeout(2)
    try:
        conn.sendall(b"\x05\x01\x00")
        assert exact(conn, 2) == b"\x05\x00", "SOCKS listener did not negotiate"
        conn.sendall(b"\x05" + (b"\x03" if udp else b"\x01") + b"\x00" + encoded(host, target_port))
        reply = exact(conn, 3)
        assert reply[0] == 5 and reply[2] == 0, reply
        endpoint = address(lambda size: exact(conn, size))
        if reply[1] != 0:
            raise EOFError(f"SOCKS refused request with status {reply[1]}")
        return conn, endpoint
    except Exception:
        conn.close()
        raise


def exchange(port, host, target_port, nonce, host_header=None):
    conn, _ = socks(port, host, target_port)
    with conn:
        data = payload(nonce)
        conn.sendall(
            f"POST /required/{nonce} HTTP/1.1\r\nHost: {host_header or host}\r\n"
            f"Content-Length: {len(data)}\r\nConnection: close\r\n\r\n".encode() + data
        )
        response = header(conn)
        size = next(
            int(line.split(b":", 1)[1]) for line in response.split(b"\r\n")
            if line.lower().startswith(b"content-length:")
        )
        body = exact(conn, size)
        # EOF is part of the oracle: the entire response has crossed the relay.
        assert conn.recv(1) == b"", "synthetic response must terminate after complete bytes"
        label, result = body.split(b":", 1)
        assert result == data[::-1], "client received corrupted/truncated nonce response"
        return label.decode()


class Kernel:
    def __init__(self, cfg, binary, a, b, vps):
        self.cfg = copy.deepcopy(cfg)
        self.directory = tempfile.TemporaryDirectory(prefix="required-relay-")
        self.log = (Path(self.directory.name) / "kernel.log").open("w+")
        relocations, reservations, self.ports = {}, [], {}
        for inbound in self.cfg["inbounds"]:
            reservation = bound()
            reservations.append(reservation)
            port = reservation.getsockname()[1]
            relocations[inbound["listen_port"]] = port
            inbound["listen_port"] = port
            self.ports[inbound["tag"]] = port
        for outbound in self.cfg["outbounds"]:
            if outbound["type"] == "direct":
                # Keep its tag, so the renderer's unchanged route decision is
                # observed as a VPS connection. No public target is ever dialed.
                tag = outbound["tag"]
                outbound.clear()
                outbound.update(type="socks", tag=tag, server="127.0.0.1",
                                server_port=vps.port, version="5")
            elif outbound.get("type") in ("socks", "http"):
                if outbound["tag"].startswith("resi-egress-"):
                    outbound["server_port"] = a.port if outbound["tag"].endswith("0001") else b.port
                else:
                    assert outbound["server_port"] in relocations, "unexpected unobserved transport"
                    outbound["server_port"] = relocations[outbound["server_port"]]
        # Bootstrap resolution alone is offline. Business DNS transports keep
        # their original detours and are answered by the selected fake provider.
        for index, server in enumerate(self.cfg["dns"]["servers"]):
            if server["tag"] == "dns_direct":
                self.cfg["dns"]["servers"][index] = {
                    "type": "hosts", "tag": "dns_direct", "predefined": {
                        name: [PUBLIC] for name in (
                            "service.invalid", "outside-split.invalid", "manual-block.invalid",
                            "a-block.invalid", "b-block.invalid", "echo.invalid",
                        )
                    },
                }
        api = bound()
        reservations.append(api)
        self.api = "http://127.0.0.1:" + str(api.getsockname()[1])
        self.cfg["experimental"] = {"clash_api": {"external_controller": self.api.removeprefix("http://")}}
        self.cfg["log"] = {"level": "error"}
        config = Path(self.directory.name) / "relay.json"
        config.write_text(json.dumps(self.cfg))
        check = subprocess.run([binary, "check", "-c", str(config)], capture_output=True, text=True)
        assert check.returncode == 0, f"stock kernel rejected rendered config: {check.stderr}"
        for reservation in reservations:
            reservation.close()
        self.process = subprocess.Popen([binary, "run", "-c", str(config)], stdout=self.log, stderr=self.log)
        try:
            for _ in range(100):
                assert self.process.poll() is None, "kernel exited before API reachability control"
                try:
                    self.call("/version")
                    break
                except OSError:
                    time.sleep(0.02)
            else:
                raise AssertionError("kernel never passed API reachability control")
        except Exception:
            self.close(True)
            raise

    def call(self, path, data=None):
        request = urllib.request.Request(
            self.api + path, data=None if data is None else json.dumps(data).encode(),
            headers={"Content-Type": "application/json"}, method="GET" if data is None else "PUT",
        )
        return urllib.request.urlopen(request, timeout=2).read()

    def select(self, slot, member):
        self.call(f"/proxies/slot-{slot}-pool", {"name": f"resi-{member}"})

    def close(self, failed=False):
        if self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=5)
        if failed:
            self.log.seek(0)
            print(self.log.read(), file=sys.stderr)
        self.log.close()
        self.directory.cleanup()


def positive(kernel, receivers, slot, port, expected, host=PUBLIC, host_header=None):
    before = {receiver: receiver.counts() for receiver in receivers}
    nonce = os.urandom(12).hex()
    result = exchange(kernel.ports[f"slot-{slot}"], host, port, nonce, host_header)
    assert result == expected, f"expected residential {expected}, got {result}"
    expected_receiver = next(receiver for receiver in receivers if receiver.label == expected)
    records = expected_receiver.business[before[expected_receiver][1]:]
    assert len(records) == 1 and records[0][1] == nonce, records
    assert records[0][0] == (host, port), "routing/sniffing rewrote the original destination"
    vps = receivers[-1]
    assert vps.counts() == before[vps], "healthy residential traffic reached VPS transport"


def negative(kernel, receivers, slot, port, host=PUBLIC, host_header=None):
    before = {receiver: receiver.counts() for receiver in receivers}
    started = time.monotonic()
    try:
        result = exchange(kernel.ports[f"slot-{slot}"], host, port, os.urandom(12).hex(), host_header)
    except (EOFError, ConnectionResetError, BrokenPipeError):
        pass
    else:
        raise AssertionError(f"required rejection returned a complete response from {result}")
    assert time.monotonic() - started < 1.5, "denied request stalled instead of failing explicitly"
    time.sleep(0.05)
    assert all(receiver.counts() == before[receiver] for receiver in receivers), (
        "denied request reached a terminal receiver",
        {receiver.label: (before[receiver], receiver.counts()) for receiver in receivers},
    )


def udp_request(kernel, receivers, slot, port, expected=None, host=PUBLIC):
    before = {receiver: receiver.counts() for receiver in receivers}
    control, relay = socks(kernel.ports[f"slot-{slot}"], "0.0.0.0", 0, udp=True)
    nonce = os.urandom(12).hex()
    body = dns_query(nonce) if port == 53 else nonce.encode()
    with control, bound(socket.SOCK_DGRAM) as client:
        client.settimeout(0.3 if expected is None else 2)
        client.sendto(b"\x00\x00\x00" + encoded(host, port) + body, relay)
        if expected is None:
            try:
                response, _ = client.recvfrom(4096)
            except TimeoutError:
                pass
            else:
                raise AssertionError(f"denied UDP received a response: {response!r}")
            assert all(receiver.counts() == before[receiver] for receiver in receivers), (
                "denied UDP reached VPS or residential receiver",
                {receiver.label: (before[receiver], receiver.counts()) for receiver in receivers},
            )
        else:
            response, _ = client.recvfrom(4096)
            reader = io.BytesIO(response)
            assert reader.read(3) == b"\x00\x00\x00"
            address(reader.read)
            expected_body = dns_reply(body) if port == 53 else expected.encode() + b":" + body
            assert reader.read() == expected_body, "UDP nonce/DNS response lost bytes"
            receiver = next(receiver for receiver in receivers if receiver.label == expected)
            seen = receiver.udp[before[receiver][2]:]
            assert len(seen) == 1 and seen[0] == ((PUBLIC, port), body), seen
            assert receivers[-1].counts() == before[receivers[-1]], "UDP/DNS escaped through VPS"
            if host != PUBLIC:
                queries = receiver.dns[before[receiver][3]:]
                assert queries, "domain UDP control never exercised residential DNS detour"


def exercise(case, cfg, binary, target_port):
    a = Receiver("HTTP-A", "http") if case == "mixed" else Receiver("A")
    b, vps = Receiver("B"), Receiver("VPS")
    receivers = a, b, vps
    # Prove the forbidden receiver would detect fallback before invoking BUI.
    assert exchange(vps.port, PUBLIC, target_port, os.urandom(12).hex()) == "VPS"
    kernel = Kernel(cfg, binary, a, b, vps)
    failed = True
    try:
        if case in ("empty", "disabled"):
            negative(kernel, receivers, 0, target_port)
            negative(kernel, receivers, 0, target_port, host=PUBLIC6)
        elif case == "pins_domain":
            positive(kernel, receivers, 0, target_port, "A")
            negative(kernel, receivers, 0, target_port, host="manual-block.invalid")
            negative(kernel, receivers, 0, target_port, host_header="manual-block.invalid")
        elif case == "pins_port":
            positive(kernel, receivers, 0, 443, "A")
            negative(kernel, receivers, 0, target_port)
        elif case == "auto":
            positive(kernel, receivers, 0, target_port, "A")
            negative(kernel, receivers, 0, target_port, host="a-block.invalid")
            negative(kernel, receivers, 0, target_port, host_header="a-block.invalid")
            positive(kernel, receivers, 1, target_port, "B", host="a-block.invalid")
            kernel.select(1, 1)
            negative(kernel, receivers, 1, target_port, host="a-block.invalid")
            positive(kernel, receivers, 1, target_port, "A", host="b-block.invalid")
        elif case == "ports":
            positive(kernel, receivers, 0, 443, "A")
            negative(kernel, receivers, 0, target_port)
            positive(kernel, receivers, 1, target_port, "B")
            kernel.select(1, 1)
            negative(kernel, receivers, 1, target_port)
            kernel.select(1, 2)
            positive(kernel, receivers, 1, target_port, "B")
        elif case == "mixed":
            positive(kernel, receivers, 0, target_port, "HTTP-A")
            positive(kernel, receivers, 1, target_port, "B")
            kernel.select(1, 1)
            positive(kernel, receivers, 1, target_port, "HTTP-A")
            for port in (53, 443, target_port):
                udp_request(kernel, receivers, 1, port)
        else:
            positive(kernel, receivers, 0, target_port, "A")
            positive(kernel, receivers, 1, target_port, "B", host=PUBLIC6)
            if case == "split":
                positive(kernel, receivers, 0, target_port, "A", host="outside-split.invalid")
            else:
                for host in ("127.0.0.1", "10.8.0.1", "203.0.113.50"):
                    negative(kernel, receivers, 0, target_port, host=host)
                udp_request(kernel, receivers, 0, target_port, "A")
                udp_request(kernel, receivers, 1, target_port, "B", host="echo.invalid")
                udp_request(kernel, receivers, 0, 53, "A")
        assert all(not receiver.errors for receiver in receivers), {
            receiver.label: receiver.errors for receiver in receivers
        }
        failed = False
        print(f"PASS required residential traffic: {case}", flush=True)
    finally:
        kernel.close(failed)
        for receiver in receivers:
            receiver.close()


def mutation(cfg, binary, port, case):
    mutated = copy.deepcopy(cfg)
    changed = 0
    for rule in mutated["route"]["rules"]:
        if rule.get("action") == "reject":
            rule.pop("action")
            rule.pop("method", None)
            rule["outbound"] = "direct"
            changed += 1
    assert changed, f"{case} mutation has no executed reject branch"
    a, b, vps = Receiver("A"), Receiver("B"), Receiver("VPS")
    kernel = Kernel(mutated, binary, a, b, vps)
    failed = True
    try:
        before = len(vps.business)
        try:
            negative(kernel, (a, b, vps), 0, port,
                     host="manual-block.invalid" if case == "pins_domain" else PUBLIC)
        except AssertionError:
            assert len(vps.business) == before + 1, "mutation failed for an unrelated fixture reason"
        else:
            raise AssertionError("reject-to-direct mutation escaped the traffic oracle")
        failed = False
        print(f"PASS reject-to-direct mutation detected: {case}", flush=True)
    finally:
        kernel.close(failed)
        for receiver in (a, b, vps):
            receiver.close()


def main():
    data, binary = json.load(sys.stdin), sys.argv[1]
    version = subprocess.check_output([binary, "version"], text=True).splitlines()[0]
    assert version == "sing-box version 1.14.2", version
    with tempfile.TemporaryDirectory(prefix="required-config-check-") as directory:
        for name, cfg in data["check_configs"].items():
            path = Path(directory) / f"{name}.json"
            path.write_text(json.dumps(cfg))
            checked = subprocess.run([binary, "check", "-c", str(path)], capture_output=True, text=True)
            assert checked.returncode == 0, f"original renderer {name} failed stock check: {checked.stderr}"
            print(f"PASS original stock config check: {name}", flush=True)
    for case in ("plain", "split", "disabled", "empty", "pins_domain", "pins_port", "auto", "ports", "mixed"):
        exercise(case, data["configs"][case], binary, data["target_port"])
    for case in ("disabled", "pins_domain"):
        mutation(data["configs"][case], binary, data["target_port"], case)
    print("COMPLETE required residential traffic: 9 cases, 2 mutations, 17 original config checks, full nonce byte oracles", flush=True)


if __name__ == "__main__":
    main()
