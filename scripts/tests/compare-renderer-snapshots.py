#!/usr/bin/env python3
"""Compare private adapter outputs without printing payloads or value-level differences."""
import hashlib
import json
from pathlib import Path
import re
import sys


def compare(baseline: Path, candidate: Path) -> dict:
    inventories = [json.loads((p / "inventory.safe.json").read_text()) for p in (baseline, candidate)]
    if inventories[0] != inventories[1]:
        raise ValueError("artifact inventory or semantics differ")
    for entry in inventories[0]:
        relative = entry.get("payload")
        if relative is not None:
            if not re.fullmatch(r"payload/[0-9]{3}\.bin", relative):
                raise ValueError("unsafe artifact payload reference")
            values = [(p / relative).read_bytes() for p in (baseline, candidate)]
            if values[0] != values[1]:
                raise ValueError("artifact payload bytes differ")
            if any(hashlib.sha256(v).hexdigest() != entry["payload_sha256"] for v in values):
                raise ValueError("artifact payload digest mismatch")
    # Debug forms stay private and capture every Artifact semantic, including actual URLs.
    for name in ("artifacts.private.txt", "plan.private.txt"):
        if (baseline / name).read_bytes() != (candidate / name).read_bytes():
            raise ValueError("complete private artifact or Plan semantics differ")
    proofs = [json.loads((p / "proof.safe.json").read_text()) for p in (baseline, candidate)]
    if proofs[0]["input_sha256"] != proofs[1]["input_sha256"]:
        raise ValueError("snapshot inputs differ")
    for proof in proofs:
        if not all(proof.get(key) is True for key in (
            "raw_plan_empty", "metadata_plan_empty", "startup_noop", "current_managed_bytes_equal"
        )) or proof.get("unknown_accesses") != 0 or proof.get("kernel_count") != 4:
            raise ValueError("missing strict snapshot acceptance proof")
    return {
        "artifact_count": len(inventories[0]),
        "input_sha256": proofs[0]["input_sha256"],
        "inventory_sha256": hashlib.sha256((candidate / "inventory.safe.json").read_bytes()).hexdigest(),
        "exact_artifact_bytes_and_semantics_equal": True,
        "both_raw_plans_empty": True,
        "both_metadata_plans_empty": True,
        "both_startup_noop": True,
        "current_managed_bytes_equal": True,
        "unknown_accesses": 0,
    }


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: compare-renderer-snapshots.py BASELINE_OUTPUT CANDIDATE_OUTPUT")
    try:
        print(json.dumps(compare(Path(sys.argv[1]), Path(sys.argv[2])), sort_keys=True))
    except (ValueError, OSError, KeyError, TypeError):
        # Do not emit exceptions that could contain private JSON or paths from a payload.
        sys.exit("FAIL: renderer snapshot comparison rejected; inspect private evidence")
