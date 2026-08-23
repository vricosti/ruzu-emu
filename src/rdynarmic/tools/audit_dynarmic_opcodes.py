#!/usr/bin/env python3
"""Compare rdynarmic's IR opcodes with Eden's Dynarmic snapshot."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


UPSTREAM_OPCODE = re.compile(
    r"^(OPCODE|A32OPC|A64OPC)\((\w+),\s*(\w+),\s*(.*?)\s*\)\s*$",
    re.MULTILINE,
)
RUST_OPCODE = re.compile(r"^    ([A-Za-z]\w*),", re.MULTILINE)
RUST_METADATA_ARM = re.compile(
    r"(?P<variants>(?:[A-Za-z]\w*\s*\|\s*)*[A-Za-z]\w*)\s*=>\s*"
    r"OpcodeInfo\s*\{\s*ret:\s*(?P<ret>(?:Type::)?\w+),\s*"
    r"args:\s*&\[(?P<args>[^\]]*)\]\s*\}",
    re.MULTILINE,
)
RUST_TYPE_ALIASES = {
    "V": "Void",
    "NZCV": "NZCV",
    "COND": "Cond",
    "A64R": "A64Reg",
    "A64V": "A64Vec",
    "A32R": "A32Reg",
    "A32E": "A32ExtReg",
    "OPQ": "Opaque",
    "ACC": "AccType",
    "COPROC": "CoprocInfo",
}


Signature = tuple[str, tuple[str, ...]]


def normalize_rust_type(type_name: str) -> str:
    type_name = type_name.removeprefix("Type::")
    return RUST_TYPE_ALIASES.get(type_name, type_name)


def upstream_signatures(path: Path) -> tuple[dict[str, Signature], set[str]]:
    signatures = {}
    duplicates = set()
    for family, name, return_type, arguments in UPSTREAM_OPCODE.findall(path.read_text()):
        prefix = "A32" if family == "A32OPC" else "A64" if family == "A64OPC" else ""
        full_name = prefix + name
        signature = (return_type, tuple(arg.strip() for arg in arguments.split(",") if arg.strip()))
        if full_name in signatures:
            duplicates.add(full_name)
        signatures[full_name] = signature
    return signatures, duplicates


def rust_names(path: Path) -> set[str]:
    source = path.read_text()
    enum_body = source.split("pub enum Opcode {", 1)[1].split("\n}", 1)[0]
    return set(RUST_OPCODE.findall(enum_body))


def rust_signatures(path: Path) -> tuple[dict[str, Signature], set[str]]:
    source = re.sub(r"//.*", "", path.read_text())
    info_body = source.split("fn info(self) -> OpcodeInfo {", 1)[1]
    signatures = {}
    duplicates = set()
    for arm in RUST_METADATA_ARM.finditer(info_body):
        signature = (
            normalize_rust_type(arm.group("ret")),
            tuple(
                normalize_rust_type(arg.strip())
                for arg in arm.group("args").split(",")
                if arg.strip()
            ),
        )
        for name in re.findall(r"[A-Za-z]\w*", arm.group("variants")):
            if name in signatures:
                duplicates.add(name)
            signatures[name] = signature
    return signatures, duplicates


def format_signature(signature: Signature) -> str:
    return f"{signature[0]}({', '.join(signature[1])})"


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

    upstream, upstream_duplicates = upstream_signatures(args.eden_opcodes)
    rust_enum = rust_names(args.rust_opcodes)
    rust, rust_duplicates = rust_signatures(args.rust_opcodes)
    upstream_names = set(upstream)
    rust_enum_names = set(rust_enum)
    missing = sorted(upstream_names - rust_enum_names)
    extra = sorted(rust_enum_names - upstream_names)
    missing_metadata = sorted(rust_enum_names - set(rust))
    unknown_metadata = sorted(set(rust) - rust_enum_names)
    signature_mismatches = sorted(
        name
        for name in upstream_names & rust_enum_names & set(rust)
        if upstream[name] != rust[name]
    )

    print(f"Eden Dynarmic opcodes: {len(upstream_names)}")
    print(f"rdynarmic opcodes: {len(rust_enum_names)}")
    print(f"missing in rdynarmic: {len(missing)}")
    for name in missing:
        print(f"  {name}")
    print(f"extra in rdynarmic: {len(extra)}")
    for name in extra:
        print(f"  {name}")
    print(f"shared signature mismatches: {len(signature_mismatches)}")
    for name in signature_mismatches:
        print(
            f"  {name}: Eden {format_signature(upstream[name])}; "
            f"rdynarmic {format_signature(rust[name])}"
        )
    print(f"rdynarmic opcodes without metadata: {len(missing_metadata)}")
    for name in missing_metadata:
        print(f"  {name}")
    print(f"metadata entries absent from the rdynarmic enum: {len(unknown_metadata)}")
    for name in unknown_metadata:
        print(f"  {name}")
    print(f"duplicate Eden opcode declarations: {len(upstream_duplicates)}")
    for name in sorted(upstream_duplicates):
        print(f"  {name}")
    print(f"duplicate rdynarmic metadata entries: {len(rust_duplicates)}")
    for name in sorted(rust_duplicates):
        print(f"  {name}")

    differences = (
        missing
        or extra
        or signature_mismatches
        or missing_metadata
        or unknown_metadata
        or upstream_duplicates
        or rust_duplicates
    )
    return int(args.strict and bool(differences))


if __name__ == "__main__":
    raise SystemExit(main())
