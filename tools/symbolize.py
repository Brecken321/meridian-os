#!/usr/bin/env python3
"""Resolve kernel addresses (e.g. from a stop screen stack trace) to
function names using `llvm-nm` from the Rust `llvm-tools` component.

Usage: python tools/symbolize.py [--release] ADDRESS [ADDRESS ...]
"""

import bisect
import glob
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def find_llvm_nm() -> str:
    exe = "llvm-nm.exe" if os.name == "nt" else "llvm-nm"
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True).stdout.strip()
    matches = glob.glob(os.path.join(sysroot, "lib", "rustlib", "*", "bin", exe))
    if not matches:
        sys.exit("llvm-nm not found; run: rustup component add llvm-tools")
    return matches[0]


def main() -> None:
    args = sys.argv[1:]
    profile = "debug"
    if args and args[0] == "--release":
        profile = "release"
        args = args[1:]
    if not args:
        sys.exit(__doc__)
    kernel = ROOT / "target" / "x86_64-unknown-none" / profile / "kernel"
    out = subprocess.run([find_llvm_nm(), "--numeric-sort", "--demangle", "--defined-only", str(kernel)],
                         capture_output=True, text=True, check=True).stdout
    symbols = []
    for line in out.splitlines():
        parts = line.split(maxsplit=2)
        if len(parts) == 3 and parts[1] in "tTwW":
            symbols.append((int(parts[0], 16), parts[2]))
    addresses = [s[0] for s in symbols]
    for arg in args:
        addr = int(arg, 16)
        i = bisect.bisect_right(addresses, addr) - 1
        if i < 0:
            print(f"{addr:#018x}  ??")
        else:
            base, name = symbols[i]
            print(f"{addr:#018x}  {name} + {addr - base:#x}")


if __name__ == "__main__":
    main()
