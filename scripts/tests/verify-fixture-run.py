#!/usr/bin/env python3
"""Reject empty Cargo selections and incomplete private snapshot acceptance evidence."""
import hashlib
import json
from pathlib import Path
import re
import sys


def verify(kind, log, input_dir=None, output_dir=None):
    lines = Path(log).read_text().splitlines()
    name = ("modules::panel::stock_gate_fixture::stock_residential_gate_restore_and_payloads"
            if kind == "stock" else "offline_renderer_snapshot::offline_renderer_snapshot")
    if (lines.count("running 1 test") != 1
            or lines.count(f"test {name} ... ok") != 1
            or sum(bool(re.fullmatch(r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9.]+s", line)) for line in lines) != 1
            or sum(line.startswith("running ") for line in lines) != 1
            or sum(line.startswith("test result:") for line in lines) != 1):
        raise ValueError("exactly one passing fixture must execute")
    if kind == "stock":
        marker = "stock_gate_fixture version=1.14.2 sha256=1a60ac17d93042c5a12410cfe83472ddee5084131dfae7a9b9806926ffb84447 gates=5 active=2 denied=3 restart_new_requests=verified"
        if lines.count(marker) != 1 or sum(line.startswith("stock_gate_fixture ") for line in lines) != 1:
            raise ValueError("missing unique stock acceptance marker")
        return
    matches = [re.fullmatch(r"offline_renderer_snapshot artifacts=([1-9][0-9]*) kernels=4 raw_plan_empty=true metadata_plan_empty=true startup_noop=true unknown_accesses=0", line) for line in lines if line.startswith("offline_renderer_snapshot ")]
    if len(matches) != 1 or matches[0] is None:
        raise ValueError("missing unique snapshot acceptance marker")
    output = Path(output_dir)
    required = {"proof.safe.json", "inventory.safe.json", "plan.safe.json", "artifacts.private.txt", "plan.private.txt"}
    if {p.name for p in output.iterdir()} != required | {"payload"}:
        raise ValueError("incomplete snapshot output")
    for name in required:
        path = output / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError("missing nonempty snapshot evidence")
    inventory = json.loads((output / "inventory.safe.json").read_bytes())
    proof = json.loads((output / "proof.safe.json").read_bytes())
    plan = json.loads((output / "plan.safe.json").read_bytes())
    if not isinstance(inventory, list) or len(inventory) != int(matches[0][1]):
        raise ValueError("artifact count mismatch")
    compact = json.dumps(inventory, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()
    if (proof.get("input_sha256") != hashlib.sha256((Path(input_dir) / "inputs.private.json").read_bytes()).hexdigest()
            or proof.get("inventory_sha256") != hashlib.sha256(compact).hexdigest()
            or not all(proof.get(key) is True for key in ("raw_plan_empty", "metadata_plan_empty", "startup_noop", "current_managed_bytes_equal"))
            or proof.get("kernel_count") != 4 or proof.get("unknown_accesses") != 0
            or plan.get("changes") != 0 or plan.get("unknown_accesses") != 0):
        raise ValueError("invalid snapshot acceptance proof")
    payloads = set()
    for entry in inventory:
        relative = entry.get("payload")
        if relative is None:
            continue
        if not isinstance(relative, str) or not re.fullmatch(r"payload/[0-9]{3}\.bin", relative):
            raise ValueError("unsafe artifact payload reference")
        path = output / relative
        if path.is_symlink() or not path.is_file():
            raise ValueError("missing artifact payload")
        value = path.read_bytes()
        if len(value) != entry.get("size") or hashlib.sha256(value).hexdigest() != entry.get("payload_sha256"):
            raise ValueError("artifact payload mismatch")
        payloads.add(path.name)
    directory = output / "payload"
    if directory.is_symlink() or not directory.is_dir() or not payloads or {p.name for p in directory.iterdir()} != payloads:
        raise ValueError("incomplete artifact payload directory")


if __name__ == "__main__":
    if len(sys.argv) not in (3, 5) or sys.argv[1] not in ("stock", "snapshot"):
        sys.exit("usage: verify-fixture-run.py stock LOG | snapshot LOG INPUT OUTPUT")
    try:
        verify(*sys.argv[1:])
    except (ValueError, OSError, KeyError, TypeError, AttributeError):
        # Exceptions can contain private JSON and payload values; emit a fixed label only.
        sys.exit("FAIL: fixture execution or snapshot acceptance evidence rejected")
