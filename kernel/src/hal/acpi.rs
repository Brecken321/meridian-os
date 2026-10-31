//! ACPI static table parsing: RSDP/XSDT discovery, MADT (processors and
//! interrupt controllers), FADT (power management registers), HPET and
//! MCFG. AML is not interpreted yet; the sleep type for soft-off (`\_S5`) is
//! extracted from the DSDT with a targeted byte-pattern parse.

use alloc::vec::Vec;
use core::ptr::read_unaligned;

use crate::mm::phys_to_virt;
use crate::sync::Once;

/// A firmware "generic address structure" register location.
#[derive(Clone, Copy, Debug, Default)]
pub struct GenericAddress {
    /// 0 = system memory, 1 = system I/O.
    pub space: u8,
    pub bit_width: u8,
    pub address: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct CpuEntry {
    pub acpi_id: u32,
    pub apic_id: u32,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct IoApicEntry {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct InterruptOverride {
    pub source: u8,
    pub gsi: u32,
    pub flags: u16,
}

#[derive(Debug, Default)]
pub struct Madt {
    pub lapic_base: u64,
    /// The platform also has dual 8259 PICs.
    pub pcat_compat: bool,
    pub cpus: Vec<CpuEntry>,
    pub ioapics: Vec<IoApicEntry>,
    pub overrides: Vec<InterruptOverride>,
}

impl Madt {
    /// Maps a legacy ISA IRQ to (GSI, level-triggered, active-low).
    pub fn isa_irq_to_gsi(&self, irq: u8) -> (u32, bool, bool) {
        match self.overrides.iter().find(|o| o.source == irq) {
            Some(o) => {
                let polarity = o.flags & 0b11;
                let trigger = (o.flags >> 2) & 0b11;
                (o.gsi, trigger == 0b11, polarity == 0b11)
            }
            None => (irq as u32, false, false),
        }
    }
}

#[derive(Debug, Default)]
pub struct Fadt {
    pub sci_interrupt: u16,
    pub smi_command: u32,
    pub acpi_enable: u8,
    pub pm1a_control: u32,
    pub pm1b_control: u32,
    pub flags: u32,
    pub reset_register: Option<(GenericAddress, u8)>,
    pub dsdt: u64,
    pub boot_arch_flags: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct McfgEntry {
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

#[derive(Debug, Default)]
pub struct Acpi {
    pub revision: u8,
    pub oem_id: [u8; 6],
    /// Signature and physical address of every table in the XSDT/RSDT.
    pub tables: Vec<([u8; 4], u64)>,
    pub madt: Option<Madt>,
    pub fadt: Option<Fadt>,
    pub hpet_base: Option<u64>,
    pub mcfg: Vec<McfgEntry>,
    /// SLP_TYPa / SLP_TYPb values for the S5 (soft-off) state.
    pub s5: Option<(u16, u16)>,
}

static ACPI: Once<Acpi> = Once::new();

pub fn get() -> Option<&'static Acpi> {
    ACPI.get()
}

fn u8_at(addr: u64) -> u8 {
    // SAFETY: callers only pass addresses inside validated tables.
    unsafe { read_unaligned(phys_to_virt(addr) as *const u8) }
}
fn u16_at(addr: u64) -> u16 {
    unsafe { read_unaligned(phys_to_virt(addr) as *const u16) }
}
fn u32_at(addr: u64) -> u32 {
    unsafe { read_unaligned(phys_to_virt(addr) as *const u32) }
}
fn u64_at(addr: u64) -> u64 {
    unsafe { read_unaligned(phys_to_virt(addr) as *const u64) }
}
fn bytes_at(addr: u64, len: usize) -> &'static [u8] {
    // SAFETY: ACPI tables live in reserved memory that is never reused.
    unsafe { core::slice::from_raw_parts(phys_to_virt(addr) as *const u8, len) }
}

fn checksum_ok(addr: u64, len: usize) -> bool {
    bytes_at(addr, len).iter().fold(0u8, |a, &b| a.wrapping_add(b)) == 0
}

fn generic_address(addr: u64) -> GenericAddress {
    GenericAddress { space: u8_at(addr), bit_width: u8_at(addr + 1), address: u64_at(addr + 4) }
}

const HEADER_LEN: u64 = 36;

/// Parses all supported tables starting from the RSDP at `rsdp`.
pub fn init(rsdp: u64) -> Result<&'static Acpi, &'static str> {
    if rsdp == 0 {
        return Err("firmware provided no ACPI RSDP");
    }
    if bytes_at(rsdp, 8) != b"RSD PTR " || !checksum_ok(rsdp, 20) {
        return Err("ACPI RSDP signature or checksum is invalid");
    }
    let mut acpi = Acpi { revision: u8_at(rsdp + 15), ..Default::default() };
    acpi.oem_id.copy_from_slice(bytes_at(rsdp + 9, 6));

    // ACPI 2.0+ provides the 64-bit XSDT; fall back to the 32-bit RSDT.
    let (root, entry_size) = if acpi.revision >= 2 && u64_at(rsdp + 24) != 0 {
        (u64_at(rsdp + 24), 8)
    } else {
        (u32_at(rsdp + 16) as u64, 4)
    };
    let root_len = u32_at(root + 4) as u64;
    if root_len < HEADER_LEN || !checksum_ok(root, root_len as usize) {
        return Err("ACPI root table is corrupt");
    }
    let mut entry = root + HEADER_LEN;
    while entry + entry_size <= root + root_len {
        let table = if entry_size == 8 { u64_at(entry) } else { u32_at(entry) as u64 };
        entry += entry_size;
        if table == 0 {
            continue;
        }
        let len = u32_at(table + 4) as usize;
        let mut signature = [0u8; 4];
        signature.copy_from_slice(bytes_at(table, 4));
        if len < HEADER_LEN as usize || !checksum_ok(table, len) {
            crate::log::warn(format_args!(
                "ACPI: skipping table {} with a bad checksum",
                core::str::from_utf8(&signature).unwrap_or("????")
            ));
            continue;
        }
        acpi.tables.push((signature, table));
        match &signature {
            b"APIC" => acpi.madt = Some(parse_madt(table, len as u64)),
            b"FACP" => acpi.fadt = Some(parse_fadt(table, len as u64)),
            b"HPET" if len >= 56 => acpi.hpet_base = Some(generic_address(table + 40).address),
            b"MCFG" => acpi.mcfg = parse_mcfg(table, len as u64),
            _ => {}
        }
    }
    if let Some(fadt) = &acpi.fadt {
        if fadt.dsdt != 0 {
            acpi.s5 = find_s5(fadt.dsdt);
        }
    }
    Ok(ACPI.call_once(|| acpi))
}

fn parse_madt(table: u64, len: u64) -> Madt {
    let mut madt =
        Madt { lapic_base: u32_at(table + 36) as u64, pcat_compat: u32_at(table + 40) & 1 != 0, ..Default::default() };
    let mut p = table + 44;
    while p + 2 <= table + len {
        let kind = u8_at(p);
        let entry_len = u8_at(p + 1) as u64;
        if entry_len < 2 || p + entry_len > table + len {
            break;
        }
        match kind {
            0 if entry_len >= 8 => {
                let flags = u32_at(p + 4);
                madt.cpus.push(CpuEntry {
                    acpi_id: u8_at(p + 2) as u32,
                    apic_id: u8_at(p + 3) as u32,
                    enabled: flags & 1 != 0,
                });
            }
            1 if entry_len >= 12 => {
                madt.ioapics.push(IoApicEntry { id: u8_at(p + 2), address: u32_at(p + 4), gsi_base: u32_at(p + 8) })
            }
            2 if entry_len >= 10 => madt.overrides.push(InterruptOverride {
                source: u8_at(p + 3),
                gsi: u32_at(p + 4),
                flags: u16_at(p + 8),
            }),
            5 if entry_len >= 12 => madt.lapic_base = u64_at(p + 4),
            9 if entry_len >= 16 => {
                let flags = u32_at(p + 8);
                madt.cpus.push(CpuEntry { acpi_id: u32_at(p + 12), apic_id: u32_at(p + 4), enabled: flags & 1 != 0 });
            }
            _ => {}
        }
        p += entry_len;
    }
    madt
}

fn parse_fadt(table: u64, len: u64) -> Fadt {
    let mut fadt = Fadt {
        sci_interrupt: u16_at(table + 46),
        smi_command: u32_at(table + 48),
        acpi_enable: u8_at(table + 52),
        pm1a_control: u32_at(table + 64),
        pm1b_control: u32_at(table + 68),
        dsdt: u32_at(table + 40) as u64,
        ..Default::default()
    };
    if len >= 116 {
        fadt.flags = u32_at(table + 112);
        fadt.boot_arch_flags = u16_at(table + 109);
    }
    // Flag bit 10: RESET_REG_SUP.
    if len >= 129 && fadt.flags & (1 << 10) != 0 {
        let reg = generic_address(table + 116);
        if reg.address != 0 {
            fadt.reset_register = Some((reg, u8_at(table + 128)));
        }
    }
    if len >= 148 && u64_at(table + 140) != 0 {
        fadt.dsdt = u64_at(table + 140);
    }
    if len >= 184 {
        let x = generic_address(table + 172);
        if x.space == 1 && x.address != 0 {
            fadt.pm1a_control = x.address as u32;
        }
    }
    fadt
}

fn parse_mcfg(table: u64, len: u64) -> Vec<McfgEntry> {
    let mut entries = Vec::new();
    let mut p = table + 44;
    while p + 16 <= table + len {
        entries.push(McfgEntry {
            base: u64_at(p),
            segment: u16_at(p + 8),
            start_bus: u8_at(p + 10),
            end_bus: u8_at(p + 11),
        });
        p += 16;
    }
    entries
}

/// Finds `Name(_S5_, Package() { SLP_TYPa, SLP_TYPb, ... })` in the DSDT.
fn find_s5(dsdt: u64) -> Option<(u16, u16)> {
    let len = u32_at(dsdt + 4) as usize;
    if &bytes_at(dsdt, 4) != b"DSDT" || len < HEADER_LEN as usize {
        return None;
    }
    let aml = &bytes_at(dsdt, len)[HEADER_LEN as usize..];
    let pos = aml.windows(4).position(|w| w == b"_S5_")?;
    // Expect NameOp (0x08), optionally followed by a root prefix '\'.
    let name_op = pos.checked_sub(1).map(|i| aml[i]);
    let rooted_name_op = pos.checked_sub(2).map(|i| (aml[i], aml[i + 1]));
    if name_op != Some(0x08) && rooted_name_op != Some((0x08, b'\\')) {
        return None;
    }
    let mut p = pos + 4;
    if *aml.get(p)? != 0x12 {
        return None; // PackageOp
    }
    p += 1;
    p += ((aml.get(p)? >> 6) & 0b11) as usize + 1; // PkgLength
    p += 1; // NumElements
    let read_value = |p: &mut usize| -> Option<u16> {
        let v = match *aml.get(*p)? {
            0x00 => 0, // ZeroOp
            0x01 => 1, // OneOp
            0x0A => {
                *p += 1;
                *aml.get(*p)? as u16 // BytePrefix
            }
            b => b as u16,
        };
        *p += 1;
        Some(v)
    };
    let a = read_value(&mut p)?;
    let b = read_value(&mut p).unwrap_or(0);
    Some((a, b))
}
