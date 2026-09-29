"""Offline packet regression, invoked inside a fresh network namespace by kernel_nft.rs."""
import itertools
import json
import os
import socket
import subprocess
import sys
import threading


SOURCE_PORTS = itertools.count(51000)


def run(*args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stderr}")
    return result


def listen(port, label):
    for family, host in [(socket.AF_INET, "0.0.0.0"), (socket.AF_INET6, "::")]:
        sock = socket.socket(family, socket.SOCK_DGRAM)
        if family == socket.AF_INET6:
            sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        sock.bind((host, port))

        def echo(s=sock):
            while True:
                _, addr = s.recvfrom(128)
                s.sendto(label.encode(), addr)

        threading.Thread(target=echo, daemon=True).start()


def probe(host, port):
    family = socket.AF_INET6 if ":" in host else socket.AF_INET
    with socket.socket(family, socket.SOCK_DGRAM) as sock:
        # Keep replies outside the redirect range, and give each probe a fresh
        # conntrack tuple when replacing the deliberately broken NAT rules.
        sock.bind(("::" if family == socket.AF_INET6 else "0.0.0.0", next(SOURCE_PORTS)))
        sock.settimeout(2)
        sock.sendto(b"probe", (host, port))
        try:
            return sock.recvfrom(128)[0].decode()
        except TimeoutError as e:
            raise TimeoutError(f"UDP {host}:{port} did not reply") from e


def worker():
    print(os.getpid(), flush=True)
    for line in sys.stdin:
        req = json.loads(line)
        if req[0] == "listen":
            listen(req[1], req[2])
            value = "ready"
        else:
            value = probe(req[1], req[2])
        print(json.dumps(value), flush=True)


def request(peer, *args):
    peer.stdin.write(json.dumps(args) + "\n")
    peer.stdin.flush()
    line = peer.stdout.readline()
    assert line, ("peer stopped while handling", args)
    return json.loads(line)


def main():
    rules = sys.stdin.read()
    peers = []
    run("ip", "link", "set", "lo", "up")
    try:
        for i in (1, 2):
            peer = subprocess.Popen(
                ["unshare", "--net", sys.executable, __file__, "worker"],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,
            )
            peers.append(peer)
            pid = peer.stdout.readline().strip()
            prefix = ["nsenter", "-t", pid, "-n"]
            v4 = {1: "192.0.2", 2: "198.51.100"}
            run("ip", "link", "add", f"v{i}", "type", "veth", "peer", "name", f"p{i}")
            run("ip", "link", "set", f"p{i}", "netns", pid)
            for cmd, dev, tail in [([], f"v{i}", 1), (prefix, f"p{i}", 2)]:
                run(*cmd, "ip", "link", "set", "lo", "up")
                run(*cmd, "ip", "addr", "add", f"{v4[i]}.{tail}/24", "dev", dev)
                run(*cmd, "ip", "-6", "addr", "add", f"2001:db8:{i}::{tail}/64", "dev", dev, "nodad")
                run(*cmd, "ip", "link", "set", dev, "up")
            # Avoid waiting for link-local DAD/neighbor discovery in this
            # short-lived topology: the regression exercises NAT, not NDP.
            for cmd, dev, remote_cmd, remote_dev, tail in [
                ([], f"v{i}", prefix, f"p{i}", 2),
                (prefix, f"p{i}", [], f"v{i}", 1),
            ]:
                mac = json.loads(run(*remote_cmd, "ip", "-j", "link", "show", remote_dev).stdout)[0]["address"]
                for addr in (f"{v4[i]}.{tail}", f"2001:db8:{i}::{tail}"):
                    run(*cmd, "ip", "neigh", "replace", addr, "lladdr", mac, "nud", "permanent", "dev", dev)
            other = 3 - i
            run(*prefix, "ip", "route", "add", f"{v4[other]}.0/24", "via", f"{v4[i]}.1")
            run(*prefix, "ip", "-6", "route", "add", f"2001:db8:{other}::/64", "via", f"2001:db8:{i}::1")
            for port in (45000, 40001):
                assert request(peer, "listen", port, f"peer{i}") == "ready"

        for path in ("/proc/sys/net/ipv4/ip_forward", "/proc/sys/net/ipv6/conf/all/forwarding"):
            with open(path, "w") as f:
                f.write("1")
        listen(40000, "local")
        for host in ("192.0.2.2", "2001:db8:1::2"):
            assert probe(host, 45000) == "peer1", ("baseline", host)
        for host in ("198.51.100.2", "2001:db8:2::2"):
            assert request(peers[0], "probe", host, 45000) == "peer2", ("baseline forwarded", host)
        # Reproduce the shipped bug first: external UDP is delivered to our HY2 listener.
        old = rules.replace("fib daddr type local ", "")
        run("nft", "-f", "-", input=old)
        for host in ("192.0.2.2", "2001:db8:1::2"):
            try:
                got = probe(host, 45000)
            except TimeoutError:
                got = "timeout"
            assert got == "local", ("old rule did not intercept", host, got)

        run("nft", "-c", "-f", "-", input=rules)
        run("nft", "-f", "-", input=rules)
        for port in (45000, 40001):
            for host in ("192.0.2.2", "2001:db8:1::2"):
                assert probe(host, port) == "peer1", ("external", host, port)
            for host in ("127.0.0.1", "::1", "192.0.2.1", "2001:db8:1::1"):
                assert probe(host, port) == "local", ("local", host, port)
            for host in ("192.0.2.1", "2001:db8:1::1"):
                assert request(peers[0], "probe", host, port) == "local", ("incoming", host, port)
            for host in ("198.51.100.2", "2001:db8:2::2"):
                assert request(peers[0], "probe", host, port) == "peer2", ("forwarded", host, port)
        print("PASS: reproduced old UDP interception; fixed IPv4/IPv6 external, forwarded, loopback, incoming hop and compat traffic")
    finally:
        for peer in peers:
            peer.terminate()
            peer.wait(timeout=5)


if __name__ == "__main__":
    worker() if sys.argv[1:] == ["worker"] else main()
