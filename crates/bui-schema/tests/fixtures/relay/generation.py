"""Exercise the generated front/two-bank topology entirely on loopback.

Connection inventories are useful evidence, not a listener/dial barrier: this
fixture also demonstrates a SOCKS greeting absent from Clash /connections.
"""
import copy
import io
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from policy import address, bound, encoded, exact, header, http, socks

# Fixed independent oracles paired with relay_generation_kernel.rs; do not
# derive expected identities from potentially incorrect renderer output.
UPSTREAM_IDS = {
    1: "00000000-0000-0000-0000-000000000001",
    2: "00000000-0000-0000-0000-000000000002",
}
GENERATION_IDS = {"a": 17, "b": 18}


def kill_group(process, sig=signal.SIGKILL):
    try:
        os.killpg(process.pid, sig)
    except ProcessLookupError:
        pass


def bounded_capture(command, seconds):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=seconds)
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    finally:
        # communicate's timeout alone only kills the immediate process; kill
        # its group as well so a shell wrapper cannot leave sleep/children alive.
        kill_group(process)
        process.wait(timeout=5)


class Supplier:
    def __init__(self, label, credentials=None):
        self.label = label.encode()
        self.credentials = credentials
        self.listener = bound()
        self.listener.listen(256)
        self.port = self.listener.getsockname()[1]
        self.closed = threading.Event()
        self.half_started = threading.Event()
        self.release_half = threading.Event()
        self.errors, self.connections = [], []
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while not self.closed.is_set():
            try:
                connection, _ = self.listener.accept()
                self.connections.append(connection)
                threading.Thread(target=self.serve, args=(connection,), daemon=True).start()
            except (OSError, TimeoutError):
                pass

    def serve(self, connection):
        try:
            with connection:
                connection.settimeout(60)
                assert exact(connection, 1) == b"\x05"
                methods = exact(connection, exact(connection, 1)[0])
                if self.credentials is None:
                    assert 0 in methods
                    connection.sendall(b"\x05\x00")
                else:
                    assert 2 in methods, "new immutable supplier lost its credentials"
                    connection.sendall(b"\x05\x02")
                    assert exact(connection, 1) == b"\x01"
                    username = exact(connection, exact(connection, 1)[0])
                    password = exact(connection, exact(connection, 1)[0])
                    assert (username, password) == self.credentials, "candidate supplier credentials differ from its immutable config"
                    connection.sendall(b"\x01\x00")
                version, command, reserved = exact(connection, 3)
                assert (version, reserved) == (5, 0)
                address(lambda size: exact(connection, size))
                if command == 3:
                    with bound(socket.SOCK_DGRAM) as udp:
                        udp.settimeout(0.1)
                        connection.sendall(b"\x05\x00\x00" + encoded("127.0.0.1", udp.getsockname()[1]))
                        control_closed = threading.Event()

                        def control():
                            try:
                                while connection.recv(1):
                                    pass
                            except OSError:
                                pass
                            control_closed.set()

                        threading.Thread(target=control, daemon=True).start()
                        while not (control_closed.is_set() or self.closed.is_set()):
                            try:
                                packet, source = udp.recvfrom(65535)
                            except TimeoutError:
                                continue
                            reader = io.BytesIO(packet)
                            assert reader.read(3) == b"\x00\x00\x00"
                            target = address(reader.read)
                            prefix = packet[:reader.tell()]
                            payload = reader.read()
                            # Echo the supplier's actual decoded destination in
                            # the payload too; a reverse address mapping must not
                            # hide an incorrectly reused first destination.
                            udp.sendto(prefix + self.label + b":" + encoded(*target) + payload, source)
                    return
                assert command == 1
                connection.sendall(b"\x05\x00\x00" + encoded("127.0.0.1", self.port))
                while not self.closed.is_set():
                    request = header(connection)
                    if request.startswith(b"POST /delayed-half "):
                        size = next(int(line.split(b":", 1)[1]) for line in request.split(b"\r\n") if line.lower().startswith(b"content-length:"))
                        self.half_started.set()
                        assert self.release_half.wait(30), "half-close fixture was never released"
                        body = exact(connection, size)
                        assert connection.recv(1) == b"", "upload did not retain its half-close"
                        payload = self.label + b":" + body
                        connection.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: " + str(len(payload)).encode() + b"\r\nConnection: close\r\n\r\n" + payload)
                        return
                    connection.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: " + str(len(self.label)).encode() + b"\r\nConnection: keep-alive\r\n\r\n" + self.label)
        except (EOFError, ConnectionResetError, BrokenPipeError, TimeoutError):
            pass
        except Exception as error:
            self.errors.append(repr(error))

    def close(self):
        self.closed.set()
        self.release_half.set()
        self.listener.close()
        for connection in self.connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()


def json_call(api, path, data=None):
    request = urllib.request.Request(api + path, data=None if data is None else json.dumps(data).encode(),
                                     headers={"Content-Type": "application/json"},
                                     method="GET" if data is None else "PUT")
    with urllib.request.urlopen(request, timeout=3) as response:
        body = response.read()
    return None if not body else json.loads(body)


class Kernel:
    def __init__(self, cfg, binary, directory, name):
        self.cfg = cfg
        self.name = name
        self.api = "http://" + cfg["experimental"]["clash_api"]["external_controller"]
        self.ports = {item["tag"]: item["listen_port"] for item in cfg["inbounds"]}
        self.log = (Path(directory) / (name + ".log")).open("w+")
        path = Path(directory) / (name + ".json")
        path.write_text(json.dumps(cfg))
        try:
            checked = bounded_capture([binary, "check", "-c", str(path)], 10)
        except Exception:
            self.log.close()
            raise
        if checked.returncode != 0:
            self.log.close()
            raise AssertionError(checked.stderr)
        self.process = subprocess.Popen([binary, "run", "-c", str(path)], stdout=self.log, stderr=self.log, start_new_session=True)
        try:
            for _ in range(200):
                assert self.process.poll() is None, self.logs()
                try:
                    json_call(self.api, "/version")
                    break
                except OSError:
                    time.sleep(0.01)
            else:
                raise AssertionError("kernel did not start: " + self.logs())
        except Exception:
            self.close()
            raise

    def logs(self):
        self.log.seek(0)
        return self.log.read()

    def inventory(self):
        data = json_call(self.api, "/connections")
        assert isinstance(data, dict) and isinstance(data.get("connections"), list), "invalid connection inventory"
        for item in data["connections"]:
            assert item["metadata"]["network"] in ("tcp", "udp"), item
            assert isinstance(item.get("chains"), list) and item["chains"], item
        return data["connections"]

    def stop(self):
        if self.process.poll() is None:
            kill_group(self.process, signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                kill_group(self.process)
                self.process.wait(timeout=5)
        kill_group(self.process)

    def close(self):
        self.stop()
        self.log.close()


def relocate(data, suppliers):
    configs = {"front": copy.deepcopy(data["front"]), **{bank: copy.deepcopy(data["backends"][bank]) for bank in ("a", "b")}}
    reservations, ports = [], {}
    for cfg in configs.values():
        for inbound in cfg["inbounds"]:
            old = inbound["listen_port"]
            assert old not in ports, "bank listeners overlap; generations cannot coexist"
            reservation = bound()
            reservations.append(reservation)
            ports[old] = reservation.getsockname()[1]
            inbound["listen_port"] = ports[old]
        api = bound()
        reservations.append(api)
        cfg["experimental"] = {"clash_api": {"external_controller": f"127.0.0.1:{api.getsockname()[1]}"}}
        if "dns" in cfg:
            tags = [server["tag"] for server in cfg["dns"].get("servers", [])]
            names = {name: ["203.0.113.9"] for name in ("service.invalid", "second.invalid", "a-block.invalid", "b-block.invalid")}
            cfg["dns"]["servers"] = [{"type": "hosts", "tag": tag, "predefined": names} for tag in tags]
        cfg["log"] = {"level": "error"}
    for name, cfg in configs.items():
        for outbound in cfg["outbounds"]:
            if outbound["type"] == "direct":
                tag = outbound["tag"]
                outbound.clear()
                outbound.update(type="socks", tag=tag, server="127.0.0.1", server_port=suppliers["direct-" + name].port, version="5")
            elif outbound["type"] in ("socks", "http"):
                old = outbound["server_port"]
                if old in ports:
                    assert outbound["server"] == "127.0.0.1", "generated interbank hop must be loopback"
                    outbound["server_port"] = ports[old]
                else:
                    assert name in ("a", "b") and old in (1080, 1081), "unexpected terminal outbound"
                    outbound["server"] = "127.0.0.1"
                    outbound["server_port"] = suppliers[name + str(old - 1079)].port
    for reservation in reservations:
        reservation.close()
    return configs


def bank_member(front, member, bank):
    matches = [item for item in front.cfg["outbounds"] if item["tag"] == f"resi-{member}"]
    assert len(matches) == 1, "member selector must have one exact identity"
    outbound = matches[0]
    assert outbound["type"] == "selector", "resi-N must select between generations"
    expected = {f"resi-policy-bank-{candidate}-{UPSTREAM_IDS[member]}" for candidate in ("a", "b")}
    assert set(outbound["outbounds"]) == expected, "member bank endpoints have incorrect full UUID identities"
    assert len(outbound["outbounds"]) == 2, "both banks must coexist before publication"
    return f"resi-policy-bank-{bank}-{UPSTREAM_IDS[member]}"


def select(front, member, bank):
    tag = bank_member(front, member, bank)
    json_call(front.api, f"/proxies/resi-{member}", {"name": tag})
    assert json_call(front.api, f"/proxies/resi-{member}")["now"] == tag


def tcp(front, slot, host="service.invalid", port=443):
    connection, _ = socks(front.ports[f"slot-{slot}"], host, port)
    return connection


def request(front, slot, expected, host="service.invalid", port=443):
    with tcp(front, slot, host, port) as connection:
        assert http(connection, host) == expected, (slot, host, expected)


class UdpSession:
    def __init__(self, front, slot):
        self.control, self.relay = socks(front.ports[f"slot-{slot}"], "0.0.0.0", 0, udp=True)
        self.client = bound(socket.SOCK_DGRAM)

    def round_trip(self, expected, sequence, target="203.0.113.9", port=12345):
        payload = str(sequence).encode()
        self.client.sendto(b"\x00\x00\x00" + encoded(target, port) + payload, self.relay)
        packet, _ = self.client.recvfrom(4096)
        reader = io.BytesIO(packet)
        assert reader.read(3) == b"\x00\x00\x00"
        returned_target = address(reader.read)
        assert returned_target == (target, port), (target, port, returned_target)
        assert reader.read(len(expected) + 1) == expected + b":", (expected, packet)
        supplier_target = address(reader.read)
        assert supplier_target == (target, port), (target, port, supplier_target)
        assert reader.read() == payload, packet

    def close(self):
        self.client.close()
        self.control.close()


def udp_request(front, slot, expected, sequence):
    session = UdpSession(front, slot)
    try:
        session.round_trip(expected, sequence)
    finally:
        session.close()


def counts(kernel):
    flows = kernel.inventory()
    return {network: sum(item["metadata"]["network"] == network for item in flows) for network in ("tcp", "udp")}


def raw_tag(kernel, member):
    generation = GENERATION_IDS[kernel.name]
    expected = {f"resi-egress-g{generation}-{upstream}" for upstream in UPSTREAM_IDS.values()}
    actual = [outbound["tag"] for outbound in kernel.cfg["outbounds"] if outbound["tag"].startswith("resi-egress-g")]
    assert len(actual) == 2 and set(actual) == expected, "backend has incorrect expected generation/full UUID identities"
    return f"resi-egress-g{generation}-{UPSTREAM_IDS[member]}"


def assert_chain(kernel, network, tag, minimum):
    flows = kernel.inventory()
    matching = [flow for flow in flows if flow["metadata"]["network"] == network and tag in flow["chains"]]
    assert len(matching) >= minimum, (network, tag, minimum, len(matching))


def assert_old_chains(front, backend):
    for member in (1, 2):
        for network in ("tcp", "udp"):
            assert_chain(front, network, bank_member(front, member, "a"), 25)
            assert_chain(backend, network, raw_tag(backend, member), 25)


def wait_inventory(kernel, predicate, reason):
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline:
        inventory = kernel.inventory()
        if predicate(inventory):
            return inventory
        time.sleep(0.02)
    raise AssertionError(f"{reason}: {counts(kernel)}")


def main():
    data, binary = json.load(sys.stdin), sys.argv[1]
    version_probe = bounded_capture([binary, "version"], 5)
    assert version_probe.returncode == 0, version_probe.stderr
    version = version_probe.stdout.splitlines()[0]
    assert version == "sing-box version 1.14.2", f"generation regression requires 1.14.2, got {version}"
    assert set(data.get("backends", {})) == {"a", "b"}, "two immutable backend banks are required"
    suppliers = {name: Supplier(name.upper(), (b"generation-18", b"synthetic-only") if name == "b1" else None)
                 for name in ("a1", "a2", "b1", "b2", "direct-a", "direct-b", "direct-front")}
    kernels, tcp_flows, udp_flows, extras, extra_udp = [], [], [], [], []
    failed = True
    try:
        configs = relocate(data, suppliers)
        with tempfile.TemporaryDirectory() as directory:
            a = Kernel(configs["a"], binary, directory, "a")
            kernels.append(a)
            front = Kernel(configs["front"], binary, directory, "front")
            kernels.append(front)
            for member in (1, 2):
                assert json_call(front.api, f"/proxies/resi-{member}")["now"] == bank_member(front, member, "a")
            # Policy behavior comes from generated routes: A1 blocks the A domain
            # and the disallowed port, B1 changes both policy decisions.
            request(front, 0, b"DIRECT-A", "a-block.invalid")
            request(front, 0, b"A1", "b-block.invalid")
            request(front, 0, b"DIRECT-A", port=2222)
            for index in range(50):
                slot = index % 2
                connection = tcp(front, slot)
                assert http(connection) == f"A{slot + 1}".encode()
                tcp_flows.append((connection, slot))
                udp = UdpSession(front, slot)
                udp.round_trip(f"A{slot + 1}".encode(), index)
                udp_flows.append((udp, slot))
            wait_inventory(a, lambda flows: sum(item["metadata"]["network"] == "tcp" for item in flows) == 50 and sum(item["metadata"]["network"] == "udp" for item in flows) == 50, "A did not inventory 50 TCP and 50 UDP flows")
            wait_inventory(front, lambda flows: sum(item["metadata"]["network"] == "tcp" for item in flows) == 50 and sum(item["metadata"]["network"] == "udp" for item in flows) == 50, "front did not inventory 50 TCP and 50 UDP flows")
            assert_old_chains(front, a)
            # Prepare a new real process while the old generation carries all
            # 100 flows, then prove preparation did not close or move them.
            b = Kernel(configs["b"], binary, directory, "b")
            kernels.append(b)
            assert counts(b) == {"tcp": 0, "udp": 0}
            for index, ((connection, slot), (udp, _)) in enumerate(zip(tcp_flows, udp_flows)):
                assert http(connection) == f"A{slot + 1}".encode(), "candidate preparation interrupted an old TCP flow"
                udp.round_trip(f"A{slot + 1}".encode(), f"prepared-{index}")
            assert_old_chains(front, a)
            half = tcp(front, 0)
            extras.append(half)
            half_body = b"deferred-body-" * 256
            half.sendall(b"POST /delayed-half HTTP/1.1\r\nHost: service.invalid\r\nContent-Length: " + str(len(half_body)).encode() + b"\r\n\r\n" + half_body)
            half.shutdown(socket.SHUT_WR)
            assert suppliers["a1"].half_started.wait(3), "supplier did not receive deferred request"
            select(front, 1, "b")
            # One PUT publishes only one member, not an atomic global update.
            request(front, 0, b"B1")
            request(front, 1, b"A2")
            new_b_udp, new_a_udp = UdpSession(front, 0), UdpSession(front, 1)
            extra_udp.extend((new_b_udp, new_a_udp))
            new_b_udp.round_trip(b"B1", "new-partial-B")
            new_a_udp.round_trip(b"A2", "new-partial-A")
            request(front, 0, b"B1", "a-block.invalid")
            request(front, 0, b"DIRECT-B", "b-block.invalid")
            request(front, 0, b"B1", port=2222)
            new_b, new_a = tcp(front, 0), tcp(front, 1)
            extras.extend((new_b, new_a))
            assert http(new_b) == b"B1" and http(new_a) == b"A2"
            assert counts(a)["tcp"] >= 51
            assert counts(b)["tcp"] >= 1
            assert_old_chains(front, a)
            for network in ("tcp", "udp"):
                assert_chain(front, network, bank_member(front, 1, "b"), 1)
                assert_chain(b, network, raw_tag(b, 1), 1)
                assert_chain(front, network, bank_member(front, 2, "a"), 26)
                assert_chain(a, network, raw_tag(a, 2), 26)
            for index, ((connection, slot), (udp, _)) in enumerate(zip(tcp_flows, udp_flows)):
                assert http(connection) == f"A{slot + 1}".encode(), "partial publication interrupted an old TCP flow"
                udp.round_trip(f"A{slot + 1}".encode(), f"partial-{index}")
            select(front, 2, "b")
            request(front, 0, b"B1")
            request(front, 1, b"B2")
            udp_request(front, 0, b"B1", "new-full-B1")
            udp_request(front, 1, b"B2", "new-full-B2")
            new_b_udp.round_trip(b"B1", "retained-partial-B")
            new_a_udp.round_trip(b"A2", "retained-partial-A")
            for index, ((connection, slot), (udp, _)) in enumerate(zip(tcp_flows, udp_flows)):
                assert http(connection) == f"A{slot + 1}".encode(), "publication interrupted an old TCP flow"
                udp.round_trip(f"A{slot + 1}".encode(), f"full-{index}")
            assert_old_chains(front, a)
            # On one retained association, send another domain and another IP,
            # then return to its first target; its bank must remain A throughout.
            for target, port in (("second.invalid", 12346), ("203.0.113.10", 12345), ("203.0.113.9", 12345)):
                udp_flows[0][0].round_trip(b"A1", "multi-" + target, target, port)
            suppliers["a1"].release_half.set()
            response = header(half)
            length = next(int(line.split(b":", 1)[1]) for line in response.split(b"\r\n") if line.lower().startswith(b"content-length:"))
            assert exact(half, length) == b"A1:" + half_body, "delayed half-close lost response across publication"
            for connection, _ in tcp_flows:
                connection.close()
            for udp, _ in udp_flows:
                udp.close()
            for udp in extra_udp:
                udp.close()
            for connection in extras:
                connection.close()
            wait_inventory(front, lambda flows: not flows, "front did not drain")
            wait_inventory(a, lambda flows: not flows, "A did not drain")
            wait_inventory(b, lambda flows: not flows, "B did not drain completed test requests")
            # A pending listener handshake is invisible even with inventories at
            # zero. Keep it open during the observation and explicitly discard
            # it before stopping A; repeated empty API reads are not a barrier.
            handshake = socket.create_connection(("127.0.0.1", next(iter(a.ports.values()))), timeout=3)
            extras.append(handshake)
            handshake.sendall(b"\x05")
            front_handshake = socket.create_connection(("127.0.0.1", front.ports["slot-0"]), timeout=3)
            extras.append(front_handshake)
            front_handshake.sendall(b"\x05")
            time.sleep(0.05)
            assert a.inventory() == [], "incomplete SOCKS greeting unexpectedly became a tracked flow"
            assert front.inventory() == [], "front incomplete SOCKS greeting unexpectedly became a tracked flow"
            assert b.inventory() == [], "unprocessed front greeting unexpectedly reached a backend"
            handshake.close()
            front_handshake.close()
            # The harness owns all clients and has now closed every old socket;
            # this is stronger fixture knowledge, not a production GC proof.
            a.stop()
            request(front, 0, b"B1")
            request(front, 1, b"B2")
            udp_request(front, 1, b"B2", "after-stop")
            assert not any(supplier.errors for supplier in suppliers.values()), [supplier.errors for supplier in suppliers.values()]
            failed = False
            print("PASS generation: old_tcp=50 old_udp=50 preparation=preserved partial_members=2 network_generation_uuid_chains=verified half_close=preserved udp_multi_target=preserved handshake_untracked=front+backend stop_old=new_bank_healthy", flush=True)
    finally:
        signal.alarm(0)
        for connection, _ in tcp_flows:
            connection.close()
        for udp, _ in udp_flows:
            udp.close()
        for udp in extra_udp:
            udp.close()
        for connection in extras:
            connection.close()
        for kernel in reversed(kernels):
            if failed:
                print(kernel.logs(), file=sys.stderr)
            kernel.close()
        for supplier in suppliers.values():
            supplier.close()


if __name__ == "__main__":
    def deadline(_signal, _frame):
        raise TimeoutError("generation fixture exceeded its 120-second deadline")

    signal.signal(signal.SIGALRM, deadline)
    signal.alarm(120)
    try:
        main()
    finally:
        signal.alarm(0)
