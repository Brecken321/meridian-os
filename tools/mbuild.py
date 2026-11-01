#!/usr/bin/env python3
"""Meridian OS build tool.

  python tools/mbuild.py build        compile everything and write build/meridian.img
  python tools/mbuild.py run          build, then boot in a QEMU window (serial log in this terminal)
  python tools/mbuild.py test         build, boot headless with `selftest`, report results (used by CI)
  python tools/mbuild.py screenshot   build, boot headless, save the screen to build/screenshot.png
  python tools/mbuild.py clean        delete build outputs

Common options: --release, --smp N, --memory 512M, --cmdline "extra options".
Environment overrides: MERIDIAN_QEMU (path to qemu-system-x86_64),
MERIDIAN_OVMF_CODE / MERIDIAN_OVMF_VARS (UEFI firmware images).

The disk image is a GPT disk with one EFI System Partition (FAT32) holding
  EFI/BOOT/BOOTX64.EFI   Meridian Boot Manager
  MERIDIAN/KERNEL.ELF    the kernel
  MERIDIAN/CMDLINE.TXT   kernel command line
  MERIDIAN/INIT.EXE      first user-mode program (PE32+)
It never touches real disks: it is only ever written to build/meridian.img.
"""

import argparse
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import time
import uuid
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
SECTOR = 512
DISK_SIZE = 48 * 1024 * 1024
PARTITION_START = 2048
ESP_TYPE = uuid.UUID("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")
# Fixed GUIDs and timestamps keep the image byte-for-byte reproducible.
DISK_GUID = uuid.uuid5(uuid.NAMESPACE_URL, "https://github.com/Brecken321/meridian-os/disk")
PART_GUID = uuid.uuid5(uuid.NAMESPACE_URL, "https://github.com/Brecken321/meridian-os/esp")
QEMU_EXIT_SUCCESS = (0x10 << 1) | 1
QEMU_EXIT_FAILURE = (0x11 << 1) | 1
BOOT_MARKER = "boot: kernel initialised"


# --------------------------------------------------------------------------
# Compilation
# --------------------------------------------------------------------------

# Native Meridian programs are PE32+ images built with the MSVC-ABI target
# and linked by Rust's bundled lld (no Microsoft tools or libraries).
USER_TARGET = "x86_64-pc-windows-msvc"
USER_PROGRAMS = [("meridian-init", "init", "INIT.EXE")]
USER_LINK_FLAGS = [
    "-C", "linker=rust-lld", "-C", "linker-flavor=lld-link",
    "-C", "link-arg=-nodefaultlib", "-C", "link-arg=-entry:meridian_start",
    "-C", "link-arg=-subsystem:native",
]


QUIET = False


def show(*parts) -> None:
    """Prints a progress line unless --quiet was given."""
    if not QUIET:
        print(*parts, flush=True)


def cargo_build(release: bool) -> tuple[Path, Path, dict[str, Path]]:
    profile = "release" if release else "debug"
    flags = (["--release"] if release else []) + (["--quiet"] if QUIET else [])
    for package, target in (("meridian-boot", "x86_64-unknown-uefi"), ("meridian-kernel", "x86_64-unknown-none")):
        cmd = ["cargo", "build", "-p", package, "--target", target, *flags]
        show("+", " ".join(cmd))
        subprocess.run(cmd, cwd=ROOT, check=True)
    programs = {}
    for package, binary, image_name in USER_PROGRAMS:
        cmd = ["cargo", "rustc", "-p", package, "--target", USER_TARGET, *flags, "--", *USER_LINK_FLAGS]
        show("+", " ".join(cmd))
        subprocess.run(cmd, cwd=ROOT, check=True)
        programs[image_name] = ROOT / "target" / USER_TARGET / profile / f"{binary}.exe"
    boot = ROOT / "target" / "x86_64-unknown-uefi" / profile / "bootx64.efi"
    kernel = ROOT / "target" / "x86_64-unknown-none" / profile / "kernel"
    return boot, kernel, programs


# --------------------------------------------------------------------------
# FAT32 + GPT image writer
# --------------------------------------------------------------------------

VALID_83 = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789$%'-_@~`!(){}^#&")


def fits_83(name: str) -> bool:
    """True if `name` can be stored as a plain upper-case 8.3 entry."""
    base, _, ext = name.partition(".")
    return "." not in ext and 1 <= len(base) <= 8 and len(ext) <= 3 and all(c in VALID_83 for c in base + ext)


def short_name(name: str) -> bytes:
    base, _, ext = name.partition(".")
    return base.ljust(8).encode("ascii") + ext.ljust(3).encode("ascii")


def short_alias(name: str, taken: set) -> bytes:
    """Windows-style numeric-tail alias, e.g. "Welcome to Meridian.txt" -> WELCOM~1.TXT."""
    base, _, ext = name.rpartition(".") if "." in name else (name, "", "")
    clean = lambda s: "".join(c for c in s.upper() if c in VALID_83)
    stem, suffix = clean(base) or "FILE", clean(ext)[:3]
    for n in range(1, 10):
        candidate = (stem[:6] + f"~{n}").ljust(8).encode("ascii") + suffix.ljust(3).encode("ascii")
        if candidate not in taken:
            return candidate
    raise ValueError(f"too many similar names for {name!r}")


def lfn_checksum(short11: bytes) -> int:
    s = 0
    for c in short11:
        s = (((s & 1) << 7) + (s >> 1) + c) & 0xFF
    return s


def utf16_units(name: str) -> list:
    data = name.encode("utf-16-le")
    return [int.from_bytes(data[i:i + 2], "little") for i in range(0, len(data), 2)]


def lfn_slots(name: str) -> int:
    return 0 if fits_83(name) else -(-len(utf16_units(name)) // 13)


def lfn_entries(name: str, short11: bytes) -> list:
    """VFAT long-name entries for `name`, in on-disk order (last part first)."""
    units = utf16_units(name)
    n = lfn_slots(name)
    if len(units) % 13:
        units = units + [0x0000] + [0xFFFF] * (n * 13 - len(units) - 1)
    pack = lambda us: b"".join(u.to_bytes(2, "little") for u in us)
    checksum = lfn_checksum(short11)
    entries = []
    for seq in range(n, 0, -1):
        part = units[(seq - 1) * 13:seq * 13]
        order = seq | (0x40 if seq == n else 0)
        entries.append(struct.pack("<B10sBBB12sH4s", order, pack(part[0:5]), 0x0F, 0, checksum,
                                   pack(part[5:11]), 0, pack(part[11:13])))
    return entries


def dir_slots(tree: dict, is_root: bool) -> int:
    return (1 if is_root else 2) + sum(1 + lfn_slots(name) for name in tree)


FAT_DATE = ((2026 - 1980) << 9) | (1 << 5) | 1  # 2026-01-01


def dir_entry(name11: bytes, attr: int, cluster: int, size: int) -> bytes:
    return struct.pack(
        "<11sBBBHHHHHHHI",
        name11, attr, 0, 0, 0, FAT_DATE, FAT_DATE,
        cluster >> 16, 0, FAT_DATE, cluster & 0xFFFF, size,
    )


class Fat32:
    RESERVED = 32
    FATS = 2
    EOC = 0x0FFFFFFF

    def __init__(self, sectors: int, label: str):
        self.sectors = sectors
        self.label = label.upper().ljust(11)[:11].encode("ascii")
        fat_sectors = 1
        while True:
            clusters = sectors - self.RESERVED - self.FATS * fat_sectors
            need = ((clusters + 2) * 4 + SECTOR - 1) // SECTOR
            if need <= fat_sectors:
                break
            fat_sectors = need
        if clusters < 65525:
            raise ValueError("partition too small for FAT32")
        self.fat_sectors = fat_sectors
        self.clusters = clusters
        self.fat = [0] * (clusters + 2)
        self.fat[0] = 0x0FFFFFF8
        self.fat[1] = self.EOC
        self.data = bytearray(clusters * SECTOR)
        self.next_free = 2

    def alloc(self, nbytes: int) -> int:
        count = max(1, (nbytes + SECTOR - 1) // SECTOR)
        first = self.next_free
        if first + count > self.clusters + 2:
            raise ValueError("image is full")
        for c in range(first, first + count):
            self.fat[c] = c + 1
        self.fat[first + count - 1] = self.EOC
        self.next_free += count
        return first

    def write(self, cluster: int, payload: bytes) -> None:
        offset = (cluster - 2) * SECTOR
        self.data[offset:offset + len(payload)] = payload

    def add_directory(self, tree: dict, cluster: int, parent: int, is_root: bool) -> None:
        entries = []
        if is_root:
            entries.append(dir_entry(self.label, 0x08, 0, 0))
        else:
            entries.append(dir_entry(b".          ", 0x10, cluster, 0))
            entries.append(dir_entry(b"..         ", 0x10, parent, 0))
        taken = set()
        for name in sorted(tree):
            node = tree[name]
            if fits_83(name):
                short = short_name(name)
            else:
                short = short_alias(name, taken)
                entries.extend(lfn_entries(name, short))
            taken.add(short)
            if isinstance(node, dict):
                child = self.alloc(dir_slots(node, False) * 32)
                entries.append(dir_entry(short, 0x10, child, 0))
                # ".." of a root child must be 0, per the FAT specification.
                self.add_directory(node, child, 0 if is_root else cluster, False)
            else:
                first = self.alloc(len(node)) if node else 0
                if node:
                    self.write(first, node)
                entries.append(dir_entry(short, 0x20, first, len(node)))
        self.write(cluster, b"".join(entries))

    def build(self, tree: dict, hidden_sectors: int) -> bytes:
        root = self.alloc(32 * dir_slots(tree, True))
        self.add_directory(tree, root, 0, True)

        bpb = bytearray(SECTOR)
        struct.pack_into(
            "<3s8sHBHBHHBHHHII", bpb, 0,
            b"\xEB\x58\x90", b"MERIDIAN", SECTOR, 1, self.RESERVED, self.FATS,
            0, 0, 0xF8, 0, 32, 64, hidden_sectors, self.sectors,
        )
        struct.pack_into("<IHHIHH12sBBBI11s8s", bpb, 36,
                         self.fat_sectors, 0, 0, root, 1, 6, bytes(12),
                         0x80, 0, 0x29, 0x4D455249, self.label, b"FAT32   ")
        bpb[510:512] = b"\x55\xAA"

        free = self.clusters + 2 - self.next_free
        fsinfo = bytearray(SECTOR)
        struct.pack_into("<I", fsinfo, 0, 0x41615252)
        struct.pack_into("<III", fsinfo, 484, 0x61417272, free, self.next_free)
        struct.pack_into("<I", fsinfo, 508, 0xAA550000)

        reserved = bytearray(self.RESERVED * SECTOR)
        reserved[0:SECTOR] = bpb
        reserved[SECTOR:2 * SECTOR] = fsinfo
        reserved[6 * SECTOR:7 * SECTOR] = bpb
        reserved[7 * SECTOR:8 * SECTOR] = fsinfo

        fat = bytearray(self.fat_sectors * SECTOR)
        struct.pack_into(f"<{len(self.fat)}I", fat, 0, *self.fat)
        return bytes(reserved) + bytes(fat) * self.FATS + bytes(self.data)


def gpt_image(partition: bytes, disk_size: int) -> bytes:
    disk_sectors = disk_size // SECTOR
    part_sectors = len(partition) // SECTOR
    last_usable = disk_sectors - 34
    part_end = PARTITION_START + part_sectors - 1
    assert part_end <= last_usable

    name = "EFI System Partition".encode("utf-16-le").ljust(72, b"\0")
    entry = struct.pack("<16s16sQQQ72s", ESP_TYPE.bytes_le, PART_GUID.bytes_le, PARTITION_START, part_end, 0, name)
    entries = entry.ljust(128 * 128, b"\0")
    entries_crc = zlib.crc32(entries)

    def header(my_lba: int, alt_lba: int, entries_lba: int) -> bytes:
        fields = [b"EFI PART", 0x00010000, 92, 0, 0, my_lba, alt_lba, 34, last_usable,
                  DISK_GUID.bytes_le, entries_lba, 128, 128, entries_crc]
        raw = struct.pack("<8sIIIIQQQQ16sQIII", *fields)
        fields[3] = zlib.crc32(raw)
        return struct.pack("<8sIIIIQQQQ16sQIII", *fields).ljust(SECTOR, b"\0")

    mbr = bytearray(SECTOR)
    mbr[446:462] = struct.pack("<B3sB3sII", 0, b"\x00\x02\x00", 0xEE, b"\xFF\xFF\xFF", 1,
                               min(disk_sectors - 1, 0xFFFFFFFF))
    mbr[510:512] = b"\x55\xAA"

    disk = bytearray(disk_size)
    disk[0:SECTOR] = mbr
    disk[SECTOR:2 * SECTOR] = header(1, disk_sectors - 1, 2)
    disk[2 * SECTOR:2 * SECTOR + len(entries)] = entries
    start = PARTITION_START * SECTOR
    disk[start:start + len(partition)] = partition
    backup_entries = (disk_sectors - 33) * SECTOR
    disk[backup_entries:backup_entries + len(entries)] = entries
    disk[(disk_sectors - 1) * SECTOR:] = header(disk_sectors - 1, 1, disk_sectors - 33)
    return bytes(disk)


WELCOME_TEXT = """Welcome to Meridian OS!

You are reading this file from the boot disk through Meridian's own AHCI
disk driver, GPT partition reader and FAT32 file system (with long file
names), all written from scratch for this kernel. Storage is read-only for
now, so nothing here can change a disk.

Things to try in this shell:
  dir C:\\MERIDIAN           list the system folder
  run C:\\MERIDIAN\\INIT.EXE  run a program from disk in user mode
  run init 1                watch a crashing program get stopped safely
  threads, ps, cpu, mem     look inside the kernel
  disks, vol                storage devices and drive letters
  shutdown                  power off

Meridian is early, honest work in progress: see docs/STATUS.md.
"""


def make_image(boot: Path, kernel: Path, programs: dict[str, Path], cmdline: str, out: Path) -> None:
    meridian = {"KERNEL.ELF": kernel.read_bytes(), "CMDLINE.TXT": cmdline.encode("utf-8")}
    for name, path in programs.items():
        meridian[name] = path.read_bytes()
    tree = {
        "EFI": {"BOOT": {"BOOTX64.EFI": boot.read_bytes()}},
        "MERIDIAN": meridian,
        "Welcome to Meridian.txt": WELCOME_TEXT.encode("utf-8"),
    }
    part_sectors = DISK_SIZE // SECTOR - PARTITION_START - 34
    fs = Fat32(part_sectors, "MERIDIAN")
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(gpt_image(fs.build(tree, PARTITION_START), DISK_SIZE))
    show(f"wrote {out.relative_to(ROOT)} (kernel {kernel.stat().st_size // 1024} KiB, cmdline \"{cmdline}\")")


# --------------------------------------------------------------------------
# QEMU
# --------------------------------------------------------------------------

def find_qemu() -> str:
    candidates = [os.environ.get("MERIDIAN_QEMU"), shutil.which("qemu-system-x86_64")]
    if os.name == "nt":
        candidates.append(r"C:\Program Files\qemu\qemu-system-x86_64.exe")
    for c in candidates:
        if c and Path(c).exists():
            return c
    sys.exit("error: qemu-system-x86_64 not found (install QEMU or set MERIDIAN_QEMU)")


def find_firmware(qemu: str) -> tuple[Path, Path]:
    if os.environ.get("MERIDIAN_OVMF_CODE") and os.environ.get("MERIDIAN_OVMF_VARS"):
        return Path(os.environ["MERIDIAN_OVMF_CODE"]), Path(os.environ["MERIDIAN_OVMF_VARS"])
    share = Path(qemu).resolve().parent / "share"
    pairs = [
        (share / "edk2-x86_64-code.fd", share / "edk2-i386-vars.fd"),
        (Path("/usr/share/OVMF/OVMF_CODE_4M.fd"), Path("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        (Path("/usr/share/OVMF/OVMF_CODE.fd"), Path("/usr/share/OVMF/OVMF_VARS.fd")),
        (Path("/usr/share/edk2/x64/OVMF_CODE.4m.fd"), Path("/usr/share/edk2/x64/OVMF_VARS.4m.fd")),
        (Path("/usr/share/edk2/ovmf/OVMF_CODE.fd"), Path("/usr/share/edk2/ovmf/OVMF_VARS.fd")),
        (Path("/usr/share/qemu/edk2-x86_64-code.fd"), Path("/usr/share/qemu/edk2-i386-vars.fd")),
    ]
    for code, variables in pairs:
        if code.exists() and variables.exists():
            return code, variables
    sys.exit("error: UEFI firmware (OVMF/edk2) not found; set MERIDIAN_OVMF_CODE and MERIDIAN_OVMF_VARS")


def qemu_command(args, headless: bool, extra: list[str]) -> list[str]:
    qemu = find_qemu()
    code, variables = find_firmware(qemu)
    vars_copy = BUILD / "ovmf-vars.fd"
    shutil.copyfile(variables, vars_copy)
    cmd = [
        qemu,
        "-machine", "q35",
        "-cpu", args.cpu,
        "-smp", str(args.smp),
        "-m", args.memory,
        "-drive", f"if=pflash,format=raw,unit=0,readonly=on,file={code}",
        "-drive", "if=pflash,format=raw,unit=1,file=ovmf-vars.fd",
        "-drive", "format=raw,file=meridian.img",
        "-device", "isa-debug-exit,iobase=0xf4,iosize=0x04",
        "-no-reboot",
    ]
    if args.accel:
        cmd += ["-accel", args.accel]
    if headless:
        cmd += ["-display", "none", "-monitor", "none"]
    return cmd + extra


def prepare(args, cmdline: str) -> None:
    boot, kernel, programs = cargo_build(args.release)
    make_image(boot, kernel, programs, cmdline.strip(), BUILD / "meridian.img")


def cmd_build(args) -> int:
    prepare(args, f"{args.video} {args.cmdline}")
    return 0


RUN_HELP = """
Meridian OS is starting in a QEMU window.
  * Click inside the window to type into the Meridian shell.
  * Press Ctrl+Alt+G to release the mouse from the window.
  * Try: help   type "C:\\Welcome to Meridian.txt"   dir C:\\MERIDIAN   threads   shutdown
  * The kernel log also appears below, and you can type commands here too.
  * Close the QEMU window (or type shutdown) to stop.
"""


def cmd_run(args) -> int:
    if QUIET:
        print("Building Meridian OS (the first build takes about a minute)...", flush=True)
    prepare(args, f"{args.video} {args.cmdline}")
    cmd = qemu_command(args, headless=args.headless, extra=["-serial", "stdio"])
    show("+", " ".join(cmd))
    if QUIET and not args.headless:
        print(RUN_HELP, flush=True)
    return subprocess.call(cmd, cwd=BUILD)


def wait_for_log(log: Path, proc: subprocess.Popen, needles: list[str], timeout: float) -> str | None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        text = log.read_text(errors="replace") if log.exists() else ""
        for n in needles:
            if n in text:
                return n
        if proc.poll() is not None:
            return None
        time.sleep(0.25)
    return None


def cmd_test(args) -> int:
    prepare(args, f"selftest {args.video} {args.cmdline}")
    log = BUILD / "serial-test.log"
    log.unlink(missing_ok=True)
    cmd = qemu_command(args, headless=True, extra=["-serial", f"file:{log.name}"])
    print("+", " ".join(cmd), flush=True)
    start = time.time()
    proc = subprocess.Popen(cmd, cwd=BUILD)
    try:
        code = proc.wait(timeout=args.timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        code = None
    elapsed = time.time() - start
    text = log.read_text(errors="replace") if log.exists() else ""
    results = [line for line in text.splitlines() if line.startswith("[selftest]")]
    for line in results:
        print(line)
    passed = code == QEMU_EXIT_SUCCESS and "SELFTEST: ALL PASSED" in text
    if not passed:
        print("\n---- last 60 lines of the serial log ----")
        print("\n".join(text.splitlines()[-60:]))
        reason = "timed out" if code is None else f"QEMU exit code {code}"
        print(f"\nSELF TESTS FAILED ({reason}, {elapsed:.1f} s); full log: {log}")
        return 1
    print(f"\nSELF TESTS PASSED in {elapsed:.1f} s (log: {log.relative_to(ROOT)})")
    return 0


def qmp(port: int, commands: list[dict]) -> None:
    with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
        f = s.makefile("rw", encoding="utf-8")
        f.readline()  # greeting
        for c in [{"execute": "qmp_capabilities"}, *commands]:
            f.write(json.dumps(c) + "\n")
            f.flush()
            while True:
                reply = json.loads(f.readline())
                if "return" in reply or "error" in reply:
                    if "error" in reply:
                        raise RuntimeError(f"QMP {c['execute']} failed: {reply['error']}")
                    break


def cmd_screenshot(args) -> int:
    prepare(args, f"{args.video} {args.cmdline}")
    log = BUILD / "serial-screenshot.log"
    log.unlink(missing_ok=True)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    cmd = qemu_command(args, headless=True,
                       extra=["-serial", f"file:{log.name}", "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off"])
    print("+", " ".join(cmd), flush=True)
    proc = subprocess.Popen(cmd, cwd=BUILD)
    try:
        if wait_for_log(log, proc, [BOOT_MARKER], args.timeout) is None:
            print(log.read_text(errors="replace") if log.exists() else "(no serial output)")
            print("error: the kernel did not finish booting")
            return 1
        time.sleep(args.settle)
        out = Path(args.out).resolve()
        out.parent.mkdir(parents=True, exist_ok=True)
        qmp(port, [{"execute": "screendump", "arguments": {"filename": str(out), "format": "png"}}])
        time.sleep(0.5)
        print(f"saved {out}")
        return 0
    finally:
        proc.kill()
        proc.wait()


QCODES = {" ": "spc", "\n": "ret", "-": "minus", "=": "equal", ".": "dot", "/": "slash", ",": "comma",
          "\\": "backslash", ";": "semicolon", "'": "apostrophe"}
SHIFTED = {":": "semicolon", '"': "apostrophe", "_": "minus", "~": "grave_accent", "!": "1"}


def key_events(text: str) -> list[dict]:
    """QMP send-key commands that type `text` on the guest's PS/2 keyboard."""
    commands = []
    for ch in text:
        if ch.isascii() and ch.isalnum():
            keys = [ch.lower()] if not ch.isupper() else ["shift", ch.lower()]
        elif ch in QCODES:
            keys = [QCODES[ch]]
        elif ch in SHIFTED:
            keys = ["shift", SHIFTED[ch]]
        else:
            raise ValueError(f"cannot type {ch!r}")
        commands.append({"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": k} for k in keys]}})
    return commands


def cmd_exec(args) -> int:
    """Boot headless, type shell commands on the virtual keyboard, print the
    serial output. With --expect-poweroff, succeed only if the guest powers
    itself off (e.g. after `shutdown`)."""
    prepare(args, f"{args.video} {args.cmdline}")
    log = BUILD / "serial-exec.log"
    log.unlink(missing_ok=True)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    cmd = qemu_command(args, headless=True,
                       extra=["-serial", f"file:{log.name}", "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off"])
    print("+", " ".join(cmd), flush=True)
    proc = subprocess.Popen(cmd, cwd=BUILD)
    try:
        if wait_for_log(log, proc, [BOOT_MARKER], args.timeout) is None:
            print(log.read_text(errors="replace") if log.exists() else "(no serial output)")
            print("error: the kernel did not finish booting")
            return 1
        time.sleep(0.5)
        for command in args.commands:
            if proc.poll() is not None:
                break
            qmp(port, key_events(command + "\n"))
            time.sleep(args.settle)
        if args.screenshot and proc.poll() is None:
            out = Path(args.out).resolve()
            qmp(port, [{"execute": "screendump", "arguments": {"filename": str(out), "format": "png"}}])
            time.sleep(0.5)
            print(f"saved {out}")
        exited = False
        if args.expect_poweroff:
            try:
                proc.wait(timeout=15)
                exited = True
            except subprocess.TimeoutExpired:
                pass
        text = log.read_text(errors="replace")
        marker = text.find(BOOT_MARKER)
        print(text[marker:] if marker >= 0 else text)
        if args.expect_poweroff:
            print("guest powered off" if exited else "error: guest did not power off")
            return 0 if exited and proc.returncode == 0 else 1
        return 0
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait()


def cmd_clean(_args) -> int:
    shutil.rmtree(BUILD, ignore_errors=True)
    subprocess.run(["cargo", "clean"], cwd=ROOT, check=False)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Meridian OS build tool")
    parser.add_argument("command", choices=["build", "run", "test", "screenshot", "exec", "clean"])
    parser.add_argument("commands", nargs="*", help="exec: shell commands to type, in order")
    parser.add_argument("--expect-poweroff", action="store_true", help="exec: require the guest to power off")
    parser.add_argument("--screenshot", action="store_true", help="exec: save a screenshot after the commands")
    parser.add_argument("--release", action="store_true", help="optimised build")
    parser.add_argument("--smp", type=int, default=4, help="virtual processors (default 4)")
    parser.add_argument("--memory", default="512M", help="guest memory (default 512M)")
    parser.add_argument("--cpu", default="max", help="QEMU CPU model (default max)")
    parser.add_argument("--accel", help="QEMU accelerator, e.g. kvm or whpx (default: emulation)")
    parser.add_argument("--video", default="video=1280x800", help="display mode option for the kernel")
    parser.add_argument("--cmdline", default="", help="extra kernel command line options")
    parser.add_argument("--headless", action="store_true", help="run: no QEMU window")
    parser.add_argument("--quiet", action="store_true", help="only print errors and short instructions")
    parser.add_argument("--timeout", type=float, default=180, help="test/screenshot timeout in seconds")
    parser.add_argument("--settle", type=float, default=1.0, help="screenshot: seconds to wait after boot")
    parser.add_argument("--out", default=str(BUILD / "screenshot.png"), help="screenshot output file")
    args = parser.parse_args()
    global QUIET
    QUIET = args.quiet
    BUILD.mkdir(exist_ok=True)
    return {"build": cmd_build, "run": cmd_run, "test": cmd_test, "screenshot": cmd_screenshot,
            "exec": cmd_exec, "clean": cmd_clean}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
