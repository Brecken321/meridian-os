#!/usr/bin/env python3
"""Independent consistency check of a FAT32 volume inside a GPT disk image
(like a read-only chkdsk), used to verify the kernel's FAT32 writes.

  python tools/fatcheck.py build/test-data.img

Checks: both FAT copies identical; every cluster chain valid, acyclic, not
shared with another file and as long as the file size needs; "." and ".."
entries of every folder; long-name entries matching their short entry;
unique short names per folder; no allocated cluster that nothing uses.
"""

import struct
import sys


def short_checksum(name: bytes) -> int:
    s = 0
    for c in name:
        s = (((s & 1) << 7) | (s >> 1)) + c & 0xFF
    return s


def check(path: str) -> list[str]:
    data = open(path, "rb").read()
    errors: list[str] = []
    if data[512:520] != b"EFI PART":
        return ["no GPT header"]
    entries_lba = struct.unpack_from("<Q", data, 512 + 72)[0]
    first_lba = struct.unpack_from("<Q", data, entries_lba * 512 + 32)[0]
    base = first_lba * 512
    bps, spc, reserved, fats = struct.unpack_from("<HBHB", data, base + 11)
    fat_size, _, _, root = struct.unpack_from("<IHHI", data, base + 36)
    total = struct.unpack_from("<I", data, base + 32)[0]
    data_start = reserved + fats * fat_size
    clusters = (total - data_start) // spc
    cb = bps * spc
    fat_bytes = fat_size * bps
    fat0 = data[base + reserved * bps: base + reserved * bps + fat_bytes]
    for k in range(1, fats):
        other = data[base + (reserved + k * fat_size) * bps: base + (reserved + k * fat_size) * bps + fat_bytes]
        if other != fat0:
            errors.append(f"FAT copy {k} differs from FAT 0")
    fat = struct.unpack_from(f"<{clusters + 2}I", fat0)
    fat = [v & 0x0FFFFFFF for v in fat]
    owner: dict[int, str] = {}

    def chain(first: int, who: str) -> list[int]:
        out = []
        c = first
        while True:
            if not 2 <= c < clusters + 2:
                errors.append(f"{who}: invalid cluster {c:#x} in chain")
                return out
            if c in owner:
                errors.append(f"{who}: cluster {c} is also used by {owner[c]}")
                return out
            owner[c] = who
            out.append(c)
            nxt = fat[c]
            if nxt >= 0x0FFFFFF8:
                return out
            if nxt == 0:
                errors.append(f"{who}: chain runs into free cluster after {c}")
                return out
            c = nxt

    def read(chain_: list[int]) -> bytes:
        return b"".join(data[base + (data_start + (c - 2) * spc) * bps: base + (data_start + (c - 2) * spc) * bps + cb]
                        for c in chain_)

    def walk(first: int, path_: str, parent: int, depth: int) -> None:
        if depth > 32:
            errors.append(f"{path_}: folders nested too deep (loop?)")
            return
        raw = read(chain(first, path_ or "\\"))
        shorts = set()
        lfn: list[bytes] = []
        checksum = None
        for i in range(0, len(raw), 32):
            e = raw[i:i + 32]
            if e[0] == 0:
                break
            if e[0] == 0xE5:
                lfn, checksum = [], None
                continue
            attr = e[11]
            if attr & 0x3F == 0x0F:
                if e[0] & 0x40:
                    lfn, checksum = [], e[13]
                elif checksum is None:
                    errors.append(f"{path_}: orphan long-name entry at slot {i // 32}")
                elif e[13] != checksum:
                    errors.append(f"{path_}: long-name entries with mixed checksums at slot {i // 32}")
                lfn.insert(0, e[1:11] + e[14:26] + e[28:32])
                continue
            if attr & 0x08:
                lfn, checksum = [], None
                continue
            name11 = e[0:11]
            first_c = struct.unpack_from("<H", e, 20)[0] << 16 | struct.unpack_from("<H", e, 26)[0]
            size = struct.unpack_from("<I", e, 28)[0]
            if name11 in (b".          ", b"..         "):
                want = first if name11[1:2] == b" " else (0 if parent == root else parent)
                if first_c != want:
                    errors.append(f"{path_}: '{name11.decode().strip()}' points to {first_c}, expected {want}")
                lfn, checksum = [], None
                continue
            if checksum is not None and checksum != short_checksum(name11):
                errors.append(f"{path_}: long name does not belong to {name11!r}")
            name = name11.decode("latin-1")
            if lfn:
                units = b"".join(lfn).decode("utf-16-le", "replace")
                name = units.split("\x00")[0].rstrip("￿")
            lfn, checksum = [], None
            if name11 in shorts:
                errors.append(f"{path_}: duplicate short name {name11!r}")
            shorts.add(name11)
            full = f"{path_}\\{name}"
            if attr & 0x10:
                if first_c == 0:
                    errors.append(f"{full}: folder without a cluster")
                    continue
                walk(first_c, full, first, depth + 1)
            elif size == 0:
                if first_c != 0:
                    errors.append(f"{full}: empty file owns cluster {first_c}")
            else:
                got = chain(first_c, full)
                need = (size + cb - 1) // cb
                if len(got) != need:
                    errors.append(f"{full}: {len(got)} clusters for {size} bytes (needs {need})")

    walk(root, "", root, 0)
    lost = [c for c in range(2, clusters + 2) if fat[c] != 0 and c not in owner]
    if lost:
        errors.append(f"{len(lost)} lost cluster(s) allocated but unused, e.g. {lost[:5]}")
    return errors


def main() -> int:
    errors = check(sys.argv[1])
    for e in errors:
        print("fatcheck:", e)
    print(f"fatcheck: {sys.argv[1]}: " + ("no errors" if not errors else f"{len(errors)} error(s)"))
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
