#!/usr/bin/env python3
"""Compare rdynarmic's IR opcode names with Eden's Dynarmic snapshot."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


UPSTREAM_OPCODE = re.compile(r"^(OPCODE|A32OPC|A64OPC)\((\w+),", re.MULTILINE)
RUST_OPCODE = re.compile(r"^    ([A-Za-z]\w*),", re.MULTILINE)


def upstream_names(path: Path) -> set[str]:
    names = set()
    for family, name in UPSTREAM_OPCODE.findall(path.read_text()):
        prefix = "A32" if family == "A32OPC" else "A64" if family == "A64OPC" else ""
        names.add(prefix + name)
    return names


def rust_names(path: Path) -> set[str]:
    source = path.read_text()
    enum_body = source.split("pub enum Opcode {", 1)[1].split("\n}", 1)[0]
    return set(RUST_OPCODE.findall(enum_body))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("eden_opcodes", type=Path, help="Eden dynarmic ir/opcodes.inc")
    parser.add_argument(
        "--rust-opcodes",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "src" / "ir" / "opcode.rs",
    )
    parser.add_argument("--strict", action="store_true", help="fail when either set differs")
    args = parser.parse_args()

    upstream = upstream_names(args.eden_opcodes)
    rust = rust_names(args.rust_opcodes)
    missing = sorted(upstream - rust)
    extra = sorted(rust - upstream)

    print(f"Eden Dynarmic opcodes: {len(upstream)}")
    print(f"rdynarmic opcodes: {len(rust)}")
    print(f"missing in rdynarmic: {len(missing)}")
    for name in missing:
        print(f"  {name}")
    print(f"extra in rdynarmic: {len(extra)}")
    for name in extra:
        print(f"  {name}")

    return int(args.strict and bool(missing or extra))


if __name__ == "__main__":
    raise SystemExit(main())
