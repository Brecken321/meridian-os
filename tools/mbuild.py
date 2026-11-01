#!/usr/bin/env python3
"""Meridian OS build tool.

  python tools/mbuild.py build        compile everything and write build/meridian.img
  python tools/mbuild.py run          build, then boot in a QEMU window (serial log in this terminal)
  python tools/mbuild.py test         build, boot headless with `selftest`, report results (used by CI)
  python tools/mbuild.py screenshot   build, boot headless, save the screen to build/screenshot.png
  python tools/mbuild.py ui           boot the desktop headless, click and type through it, save screenshots
  python tools/mbuild.py demo         open Meridian's text shell in a window and type a guided tour into it
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
DESKTOP_MARKER = "desktop: ready"


# --------------------------------------------------------------------------
# Compilation
# --------------------------------------------------------------------------

# Native Meridian programs are PE32+ images built with the MSVC-ABI target
# and linked by Rust's bundled lld (no Microsoft tools or libraries).
USER_TARGET = "x86_64-pc-windows-msvc"
USER_PROGRAMS = [("meridian-init", "init", "INIT.EXE"), ("meridian-desktop", "desktop", "DESKTOP.EXE")]
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

Things to try on the desktop:
  - Open Start (the taskbar button or the Windows key) and type to search.
  - File Explorer: browse C:\\ and open files and programs.
  - Terminal: dir, type, ps, threads, mem, run C:\\MERIDIAN\\INIT.EXE
  - Task Manager: live processes, kernel threads and processor use.
  - Settings: dark or light mode, accent colour, wallpaper, taskbar alignment.
  - Drag a window by its title bar; double-click the title bar to maximise.
  - Alt+Tab switches windows, Alt+F4 closes one.
  - Start > Power shuts Meridian down or restarts it.

In the kernel's text shell (boot with "nodesktop", or type in the serial
console), "help" lists the kernel commands: threads, cpu, mem, disks, vol...

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
        "-rtc", "base=localtime",
    ]
    if args.accel:
        cmd += ["-accel", args.accel]
    if headless:
        # Automated runs stop instead of rebooting, so a crash cannot loop.
        cmd += ["-display", "none", "-monitor", "none", "-no-reboot"]
    return cmd + extra


def prepare(args, cmdline: str) -> None:
    boot, kernel, programs = cargo_build(args.release)
    make_image(boot, kernel, programs, cmdline.strip(), BUILD / "meridian.img")


def cmd_build(args) -> int:
    prepare(args, f"{args.video} {args.cmdline}")
    return 0


RUN_HELP = """
Meridian OS is starting in a QEMU window; the desktop appears in a few seconds.
  * Use the mouse and keyboard in that window as on any PC: the Start button,
    File Explorer, Terminal, Task Manager, Settings and Notepad all work.
  * If the window ever captures the mouse, press Ctrl+Alt+G to release it.
  * The kernel log appears below, and you can type kernel shell commands here.
  * Start > Power > Shut down (or closing the window) stops Meridian.
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


def qmp(port: int, commands: list[dict], pause: float = 0.0) -> None:
    """Sends QMP commands in order, waiting `pause` seconds between them."""
    with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
        f = s.makefile("rw", encoding="utf-8")
        f.readline()  # greeting
        for i, c in enumerate([{"execute": "qmp_capabilities"}, *commands]):
            f.write(json.dumps(c) + "\n")
            f.flush()
            while True:
                reply = json.loads(f.readline())
                if "return" in reply or "error" in reply:
                    if "error" in reply:
                        raise RuntimeError(f"QMP {c['execute']} failed: {reply['error']}")
                    break
            if pause and i > 0:
                time.sleep(pause)


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
    marker = BOOT_MARKER if "nodesktop" in args.cmdline.split() else DESKTOP_MARKER
    try:
        if wait_for_log(log, proc, [marker], args.timeout) is None:
            print(log.read_text(errors="replace") if log.exists() else "(no serial output)")
            print(f"error: never saw \"{marker}\" on the serial log")
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


# The desktop walk-through `ui` runs when given no actions (also used by CI).
# Coordinates are for the default 1280x800 screen.
UI_STEPS = [
    "shot desktop",
    "click 520 776", "wait 0.5", "shot start-menu",
    "type task", "wait 0.3", "shot start-search",
    "key ret", "wait 1.5", "shot task-manager",
    "click 337 115", "wait 3", "shot performance",
    "dclick 56 158", "wait 1", "shot welcome-file",
    "click 568 776", "wait 1", "click 262 254", "wait 1", "shot explorer",
    "key meta_l", "wait 0.5", "key esc", "wait 0.3",
    "click 616 776", "wait 1", r"type dir C:\\MERIDIAN\n", "wait 1", "shot terminal",
    "key alt+f4", "wait 0.5",
    "click 712 776", "wait 1", "click 879 290", "wait 1.5", "shot light-mode",
    "dclick 538 171", "wait 1", "shot maximized",
    "click 520 776", "wait 0.5", "click 900 704", "wait 0.5", "shot power-menu",
    "click 830 608", "wait 3",
]


def unescape(text: str) -> str:
    """`ui` type text: \\n is Enter and \\\\ a backslash; other backslashes stay."""
    out, i = [], 0
    while i < len(text):
        if text[i] == "\\" and text[i + 1:i + 2] in ("n", "\\"):
            out.append("\n" if text[i + 1] == "n" else "\\")
            i += 2
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def qemu_abs(x: int, y: int, width: int, height: int) -> list[dict]:
    """Absolute pointer position for QMP (0..0x7FFF on each axis)."""
    return [{"type": "abs", "data": {"axis": "x", "value": -(-x * 0x8000 // width)}},
            {"type": "abs", "data": {"axis": "y", "value": -(-y * 0x8000 // height)}}]


def ui_commands(action: str, width: int, height: int) -> list[dict]:
    """QMP commands for one `ui` action (see cmd_ui)."""
    verb, _, rest = action.partition(" ")
    nums = [int(v) for v in rest.split()] if verb in ("click", "dclick", "rclick", "move", "drag", "wheel") else []

    def send(*events):
        return {"execute": "input-send-event", "arguments": {"events": list(events)}}

    def button(name, down):
        return {"type": "btn", "data": {"down": down, "button": name}}

    if verb == "move":
        return [send(*qemu_abs(*nums, width, height))]
    if verb in ("click", "dclick", "rclick"):
        name = "right" if verb == "rclick" else "left"
        clicks = 2 if verb == "dclick" else 1
        return [send(*qemu_abs(*nums, width, height))] + [send(button(name, d)) for _ in range(clicks) for d in (True, False)]
    if verb == "drag":
        x1, y1, x2, y2 = nums
        steps = [send(*qemu_abs(x1, y1, width, height)), send(button("left", True))]
        for i in range(1, 9):
            steps.append(send(*qemu_abs(x1 + (x2 - x1) * i // 8, y1 + (y2 - y1) * i // 8, width, height)))
        return steps + [send(button("left", False))]
    if verb == "wheel":
        x, y, n = nums
        name = "wheel-down" if n > 0 else "wheel-up"
        return [send(*qemu_abs(x, y, width, height))] + [send(button(name, d)) for _ in range(abs(n)) for d in (True, False)]
    if verb == "type":
        return key_events(unescape(rest))
    if verb == "key":
        return [{"execute": "send-key", "arguments": {"keys": [{"type": "qcode", "data": k} for k in rest.split("+")]}}]
    raise ValueError(f"unknown ui action {action!r}")


def cmd_ui(args) -> int:
    """Boots the desktop headless and drives it with the virtual mouse and
    keyboard, saving a screenshot at every `shot NAME` to build/ui/. Actions:
    click X Y, dclick X Y, rclick X Y, move X Y, drag X1 Y1 X2 Y2,
    wheel X Y N, type TEXT, key QCODE[+QCODE], wait SECONDS, shot NAME.
    Fails if the desktop never starts or the kernel log reports a crash."""
    prepare(args, f"{args.video} {args.cmdline}")
    width, height = (int(v) for v in args.video.split("=")[1].split("x"))
    log = BUILD / "serial-ui.log"
    log.unlink(missing_ok=True)
    shots = BUILD / "ui"
    shots.mkdir(exist_ok=True)
    for old in shots.glob("*.png"):
        old.unlink()
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    cmd = qemu_command(args, headless=True,
                       extra=["-serial", f"file:{log.name}", "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off"])
    show("+", " ".join(cmd))
    proc = subprocess.Popen(cmd, cwd=BUILD)
    count = 0
    try:
        if wait_for_log(log, proc, [DESKTOP_MARKER], args.timeout) is None:
            print(log.read_text(errors="replace") if log.exists() else "(no serial output)")
            print("error: the desktop did not start")
            return 1
        time.sleep(args.settle)
        for action in args.commands or UI_STEPS:
            if proc.poll() is not None:
                break
            verb, _, rest = action.partition(" ")
            if verb == "wait":
                time.sleep(float(rest))
            elif verb == "shot":
                count += 1
                out = shots / f"{count:02}-{rest}.png"
                qmp(port, [{"execute": "screendump", "arguments": {"filename": str(out), "format": "png"}}])
                time.sleep(0.3)
                print(f"saved {out.relative_to(ROOT)}")
            else:
                try:
                    qmp(port, ui_commands(action, width, height), pause=0.05)
                except OSError:
                    # The guest may have powered off (Start > Shut down).
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        raise RuntimeError(f"lost the QMP connection during {action!r}") from None
        time.sleep(0.5)
        powered_off = proc.poll() is not None
        if powered_off:
            print(f"the guest powered off (QEMU exit code {proc.returncode})")
    finally:
        proc.kill()
        proc.wait()
    text = log.read_text(errors="replace") if log.exists() else ""
    crash = [line for line in text.splitlines() if any(w in line.lower() for w in ("panic", "fault", "exception", "terminated"))]
    if crash:
        print("\n".join(crash))
        print("UI TEST FAILED: the log reports a crash (full log: build/serial-ui.log)")
        return 1
    if args.expect_poweroff and not powered_off:
        print("UI TEST FAILED: the guest did not power off")
        return 1
    print(f"UI test finished: {count} screenshot(s) in build/ui/")
    return 0


def cmd_exec(args) -> int:
    """Boot headless, type shell commands on the virtual keyboard, print the
    serial output. With --expect-poweroff, succeed only if the guest powers
    itself off (e.g. after `shutdown`). The desktop is not started, so the
    keystrokes reach the text shell."""
    prepare(args, f"nodesktop {args.video} {args.cmdline}")
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


# The guided tour: (command, what it shows, seconds it may take).
DEMO_STEPS = [
    ("ver", "kernel version, firmware, display and keyboard", 20),
    ("cpu", "the processor and its 4 running cores", 20),
    ('type "C:\\Welcome to Meridian.txt"', "a long-named file read from the FAT32 boot disk", 30),
    ("dir C:\\MERIDIAN", "the system folder on drive C:", 30),
    ("disks", "the SATA disk and its GPT partition", 20),
    ("run C:\\MERIDIAN\\INIT.EXE", "a PE32+ program loaded from disk and run in user mode", 60),
    ("run init 1", "a program that crashes on purpose; only it is stopped", 60),
    ("threads", "kernel threads spread over the processors", 20),
    ("mem", "physical memory and kernel pool use", 20),
    ("selftest", "all kernel self tests, run live", 300),
]
PROMPT = "meridian\x1b[0m> "


def cmd_demo(args) -> int:
    """Opens Meridian in a QEMU window, types a guided tour into its shell
    (saving a screenshot of every step to build/demo/) and leaves the
    window open for you to keep exploring."""
    print("Building Meridian OS...", flush=True)
    prepare(args, f"nodesktop {args.video} {args.cmdline}")
    log = BUILD / "serial-demo.log"
    log.unlink(missing_ok=True)
    shots = BUILD / "demo"
    shots.mkdir(exist_ok=True)
    for old in shots.glob("*.png"):
        old.unlink()
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    cmd = qemu_command(args, headless=args.headless, extra=[
        "-serial", f"file:{log.name}", "-qmp", f"tcp:127.0.0.1:{port},server=on,wait=off", "-name", "Meridian OS"])
    show("+", " ".join(cmd))
    # Detached, so the window stays open after this tool exits.
    flags = 0x0000_0008 | 0x0000_0200 if os.name == "nt" else 0  # DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
    proc = subprocess.Popen(cmd, cwd=BUILD, creationflags=flags, stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=os.name != "nt")
    print("Meridian OS is booting in a QEMU window...", flush=True)
    if wait_for_log(log, proc, [BOOT_MARKER], args.timeout) is None:
        print("error: Meridian did not finish booting; see build/serial-demo.log")
        return 1

    def prompts() -> int:
        return log.read_text(errors="replace").count(PROMPT) if log.exists() else 0

    deadline = time.time() + 60
    while prompts() == 0 and time.time() < deadline:
        time.sleep(0.25)
    time.sleep(1.5)
    failures = 0
    for number, (command, what, limit) in enumerate(DEMO_STEPS, 1):
        print(f"[{number}/{len(DEMO_STEPS)}] {command:<34} {what}", flush=True)
        before = prompts()
        qmp(port, key_events(command + "\n"), pause=0.04)
        end = time.time() + limit
        while prompts() <= before and time.time() < end and proc.poll() is None:
            time.sleep(0.25)
        if prompts() <= before:
            print("        did not finish in time", flush=True)
            failures += 1
        time.sleep(args.settle + 1.5)
        name = "".join(c if c.isalnum() else "-" for c in command.lower()).strip("-")[:28]
        qmp(port, [{"execute": "screendump",
                    "arguments": {"filename": str((shots / f"{number:02d}-{name}.png").resolve()), "format": "png"}}])
    text = log.read_text(errors="replace")
    passed = "SELFTEST: ALL PASSED" in text
    print()
    print("Self tests:", "all passed" if passed else "FAILURES (see build/serial-demo.log)")
    print(f"Screenshots of every step: {shots.relative_to(ROOT)}")
    print("The Meridian window stays open: click inside it to type your own commands,")
    print("press Ctrl+Alt+G to release the mouse, and close the window (or type shutdown) when done.")
    return 0 if passed and failures == 0 else 1


def cmd_clean(_args) -> int:
    shutil.rmtree(BUILD, ignore_errors=True)
    subprocess.run(["cargo", "clean"], cwd=ROOT, check=False)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="Meridian OS build tool")
    parser.add_argument("command", choices=["build", "run", "test", "screenshot", "exec", "ui", "demo", "clean"])
    parser.add_argument("commands", nargs="*", help="exec: shell commands to type; ui: desktop actions")
    parser.add_argument("--expect-poweroff", action="store_true", help="exec/ui: require the guest to power off")
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
            "exec": cmd_exec, "ui": cmd_ui, "demo": cmd_demo, "clean": cmd_clean}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
