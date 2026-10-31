//! Just enough ELF64 parsing to load the kernel's PT_LOAD segments.

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 0x3E;

pub struct Elf<'a> {
    data: &'a [u8],
    pub entry: u64,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

pub struct Segment<'a> {
    pub vaddr: u64,
    pub memsz: u64,
    pub file: &'a [u8],
    pub writable: bool,
    pub executable: bool,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.get(0..4) != Some(b"\x7FELF".as_slice()) {
            return Err("kernel is not an ELF file");
        }
        if data.get(4) != Some(&2) || data.get(5) != Some(&1) {
            return Err("kernel is not a 64-bit little-endian ELF");
        }
        let bad = "kernel ELF header is truncated";
        if u16_at(data, 16).ok_or(bad)? != ET_EXEC {
            return Err("kernel ELF is not a static executable");
        }
        if u16_at(data, 18).ok_or(bad)? != EM_X86_64 {
            return Err("kernel ELF is not built for x86-64");
        }
        let elf = Elf {
            data,
            entry: u64_at(data, 24).ok_or(bad)?,
            phoff: u64_at(data, 32).ok_or(bad)? as usize,
            phentsize: u16_at(data, 54).ok_or(bad)? as usize,
            phnum: u16_at(data, 56).ok_or(bad)? as usize,
        };
        if elf.phentsize < 56 {
            return Err("kernel ELF program headers are malformed");
        }
        for i in 0..elf.phnum {
            elf.segment(i).ok_or("kernel ELF segment is out of bounds")?;
        }
        Ok(elf)
    }

    fn segment(&self, index: usize) -> Option<Option<Segment<'a>>> {
        let h = self.phoff.checked_add(index.checked_mul(self.phentsize)?)?;
        let d = self.data;
        if u32_at(d, h)? != PT_LOAD {
            return Some(None);
        }
        let flags = u32_at(d, h + 4)?;
        let offset = u64_at(d, h + 8)? as usize;
        let vaddr = u64_at(d, h + 16)?;
        let filesz = u64_at(d, h + 32)? as usize;
        let memsz = u64_at(d, h + 40)?;
        if filesz as u64 > memsz {
            return None;
        }
        Some(Some(Segment {
            vaddr,
            memsz,
            file: d.get(offset..offset.checked_add(filesz)?)?,
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        }))
    }

    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        (0..self.phnum).filter_map(|i| self.segment(i).flatten())
    }

    /// Lowest and highest (exclusive) virtual addresses covered by PT_LOAD.
    pub fn span(&self) -> Option<(u64, u64)> {
        let lo = self.segments().map(|s| s.vaddr).min()?;
        let hi = self.segments().map(|s| s.vaddr + s.memsz).max()?;
        Some((lo, hi))
    }
}
