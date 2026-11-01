//! PE32+ image loader (the executable format of both native Meridian
//! programs and 64-bit Windows programs), written from the published
//! PE/COFF specification.
//!
//! Supported today: x86-64 images, section mapping with per-section page
//! protections, base relocations (`DIR64`), entry point and stack size.
//! Images that need imports, TLS callbacks or exception tables at load time
//! are rejected with a clear error rather than loaded half-working; those
//! features arrive with the Win32 subsystem.

use alloc::vec;

use crate::mm::PAGE_SIZE;
use crate::mm::user::{AddressSpace, Protection, USER_LIMIT, USER_MIN};

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_FILE_EXECUTABLE_IMAGE: u16 = 0x0002;
const PE32_PLUS_MAGIC: u16 = 0x20B;
const DLL_CHARACTERISTICS_DYNAMIC_BASE: u16 = 0x0040;
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;
const DIR_IMPORT: usize = 1;
const DIR_BASERELOC: usize = 5;
const DIR_TLS: usize = 9;
const REL_BASED_ABSOLUTE: u16 = 0;
const REL_BASED_DIR64: u16 = 10;
/// Images with a relocation table are loaded this far from their preferred
/// base, so the relocation path is exercised on every load.
const RELOCATION_SHIFT: u64 = 0x1000_0000;
const MAX_IMAGE_SIZE: u64 = 1 << 30;
const MIN_STACK: u64 = 64 * 1024;
const MAX_STACK: u64 = 8 * 1024 * 1024;

/// Windows subsystem values relevant to Meridian.
pub mod subsystem {
    pub const NATIVE: u16 = 1;
    pub const WINDOWS_GUI: u16 = 2;
    pub const WINDOWS_CUI: u16 = 3;
}

#[derive(Debug)]
pub struct LoadedImage {
    pub base: u64,
    pub entry: u64,
    pub size: u64,
    pub stack_size: u64,
    pub subsystem: u16,
    pub relocated: bool,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16, &'static str> {
    Ok(u16::from_le_bytes(d.get(o..o + 2).ok_or("image is truncated")?.try_into().unwrap()))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32, &'static str> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).ok_or("image is truncated")?.try_into().unwrap()))
}
fn u64_at(d: &[u8], o: usize) -> Result<u64, &'static str> {
    Ok(u64::from_le_bytes(d.get(o..o + 8).ok_or("image is truncated")?.try_into().unwrap()))
}

struct Section {
    rva: u64,
    virtual_size: u64,
    raw_offset: usize,
    raw_size: usize,
    characteristics: u32,
}

/// Validates `image` and maps it into `space`.
pub fn load(space: &AddressSpace, image: &[u8]) -> Result<LoadedImage, &'static str> {
    if image.get(0..2) != Some(b"MZ".as_slice()) {
        return Err("not an executable (missing MZ header)");
    }
    let pe = u32_at(image, 0x3C)? as usize;
    if image.get(pe..pe + 4) != Some(b"PE\0\0".as_slice()) {
        return Err("not a PE image (missing PE signature)");
    }
    let file = pe + 4;
    if u16_at(image, file)? != IMAGE_FILE_MACHINE_AMD64 {
        return Err("image is not for x86-64");
    }
    let section_count = u16_at(image, file + 2)? as usize;
    let optional_size = u16_at(image, file + 16)? as usize;
    if u16_at(image, file + 18)? & IMAGE_FILE_EXECUTABLE_IMAGE == 0 {
        return Err("image is not marked executable");
    }
    let opt = file + 20;
    if u16_at(image, opt)? != PE32_PLUS_MAGIC {
        return Err("image is not PE32+ (64-bit)");
    }
    let entry_rva = u32_at(image, opt + 16)? as u64;
    let preferred_base = u64_at(image, opt + 24)?;
    let section_alignment = u32_at(image, opt + 32)? as u64;
    let image_size = u32_at(image, opt + 56)? as u64;
    let headers_size = u32_at(image, opt + 60)? as usize;
    let subsystem = u16_at(image, opt + 68)?;
    let dll_characteristics = u16_at(image, opt + 70)?;
    let stack_reserve = u64_at(image, opt + 72)?;
    let directory_count = u32_at(image, opt + 108)? as usize;
    let directory = |i: usize| -> Result<(u64, u64), &'static str> {
        if i >= directory_count || opt + 112 + i * 8 + 8 > opt + optional_size {
            return Ok((0, 0));
        }
        Ok((u32_at(image, opt + 112 + i * 8)? as u64, u32_at(image, opt + 112 + i * 8 + 4)? as u64))
    };

    if section_alignment != PAGE_SIZE {
        return Err("image section alignment must be 4 KiB");
    }
    if image_size == 0 || image_size > MAX_IMAGE_SIZE || headers_size > image.len() {
        return Err("image size is invalid");
    }
    if directory(DIR_IMPORT)?.1 != 0 {
        return Err("image imports DLLs, which needs the Win32 subsystem (not implemented yet)");
    }
    if directory(DIR_TLS)?.1 != 0 {
        return Err("image uses thread-local storage callbacks (not implemented yet)");
    }
    if entry_rva == 0 || entry_rva >= image_size {
        return Err("image entry point is invalid");
    }

    let mut sections = vec::Vec::with_capacity(section_count);
    let first_section = opt + optional_size;
    for i in 0..section_count {
        let s = first_section + i * 40;
        let section = Section {
            virtual_size: u32_at(image, s + 8)? as u64,
            rva: u32_at(image, s + 12)? as u64,
            raw_size: u32_at(image, s + 16)? as usize,
            raw_offset: u32_at(image, s + 20)? as usize,
            characteristics: u32_at(image, s + 36)?,
        };
        let mapped = section.virtual_size.max(section.raw_size as u64);
        if section.rva % PAGE_SIZE != 0 || section.rva + mapped > image_size {
            return Err("image section lies outside the image");
        }
        if section.raw_offset.checked_add(section.raw_size).is_none_or(|end| end > image.len()) {
            return Err("image section data is truncated");
        }
        sections.push(section);
    }

    let relocations = directory(DIR_BASERELOC)?;
    let relocatable = dll_characteristics & DLL_CHARACTERISTICS_DYNAMIC_BASE != 0 && relocations.1 != 0;
    let base = if relocatable { preferred_base.wrapping_add(RELOCATION_SHIFT) } else { preferred_base };
    if base % PAGE_SIZE != 0 || base < USER_MIN || base.checked_add(image_size).is_none_or(|end| end > USER_LIMIT) {
        return Err("image base address is outside user space");
    }
    let pages = image_size.div_ceil(PAGE_SIZE);

    // Map everything writable while copying, then apply final protections.
    space.commit_at(base, pages, Protection::ReadWrite).map_err(|_| "cannot reserve the image's address range")?;
    space.write(base, &image[..headers_size]).map_err(|_| "failed to copy image headers")?;
    for s in &sections {
        let n = s.raw_size.min(s.virtual_size.max(s.raw_size as u64) as usize);
        space.write(base + s.rva, &image[s.raw_offset..s.raw_offset + n]).map_err(|_| "failed to copy a section")?;
    }

    if base != preferred_base {
        apply_relocations(space, image, base, preferred_base, image_size, relocations)?;
    }

    space
        .protect(base, headers_size.div_ceil(PAGE_SIZE as usize).max(1) as u64, Protection::ReadOnly)
        .map_err(|_| "failed to protect headers")?;
    for s in &sections {
        let pages = s.virtual_size.max(s.raw_size as u64).div_ceil(PAGE_SIZE);
        if pages == 0 {
            continue;
        }
        let protection = match (s.characteristics & SCN_MEM_EXECUTE != 0, s.characteristics & SCN_MEM_WRITE != 0) {
            (true, true) => Protection::ReadWriteExecute,
            (true, false) => Protection::ReadExecute,
            (false, true) => Protection::ReadWrite,
            (false, false) => Protection::ReadOnly,
        };
        space.protect(base + s.rva, pages, protection).map_err(|_| "failed to protect a section")?;
    }

    Ok(LoadedImage {
        base,
        entry: base + entry_rva,
        size: image_size,
        stack_size: stack_reserve.clamp(MIN_STACK, MAX_STACK).next_multiple_of(PAGE_SIZE),
        subsystem,
        relocated: base != preferred_base,
    })
}

/// Applies base relocations, reading the table from the file image and
/// patching the mapped copy.
fn apply_relocations(
    space: &AddressSpace,
    image: &[u8],
    base: u64,
    preferred: u64,
    image_size: u64,
    (rva, size): (u64, u64),
) -> Result<(), &'static str> {
    let delta = base.wrapping_sub(preferred);
    // The relocation directory is addressed by RVA; find it in the file via
    // the section that contains it.
    let table = rva_to_file(image, rva, size).ok_or("relocation table is not in any section")?;
    let mut off = 0usize;
    while off + 8 <= table.len() {
        let page_rva = u32::from_le_bytes(table[off..off + 4].try_into().unwrap()) as u64;
        let block_size = u32::from_le_bytes(table[off + 4..off + 8].try_into().unwrap()) as usize;
        if block_size < 8 || off + block_size > table.len() {
            return Err("relocation block is malformed");
        }
        for entry in table[off + 8..off + block_size].chunks_exact(2) {
            let value = u16::from_le_bytes([entry[0], entry[1]]);
            let (kind, offset) = (value >> 12, (value & 0xFFF) as u64);
            match kind {
                REL_BASED_ABSOLUTE => {}
                REL_BASED_DIR64 => {
                    let target = page_rva + offset;
                    if target + 8 > image_size {
                        return Err("relocation target is outside the image");
                    }
                    let mut word = [0u8; 8];
                    space.read(base + target, &mut word).map_err(|_| "relocation read failed")?;
                    let patched = u64::from_le_bytes(word).wrapping_add(delta);
                    space.write(base + target, &patched.to_le_bytes()).map_err(|_| "relocation write failed")?;
                }
                _ => return Err("unsupported relocation type"),
            }
        }
        off += block_size;
    }
    Ok(())
}

fn rva_to_file(image: &[u8], rva: u64, size: u64) -> Option<&[u8]> {
    let pe = u32_at(image, 0x3C).ok()? as usize;
    let file = pe + 4;
    let count = u16_at(image, file + 2).ok()? as usize;
    let first = file + 20 + u16_at(image, file + 16).ok()? as usize;
    for i in 0..count {
        let s = first + i * 40;
        let vsize = u32_at(image, s + 8).ok()? as u64;
        let srva = u32_at(image, s + 12).ok()? as u64;
        let raw_size = u32_at(image, s + 16).ok()? as u64;
        let raw = u32_at(image, s + 20).ok()? as u64;
        if rva >= srva && rva + size <= srva + vsize.max(raw_size) && rva + size <= srva + raw_size {
            let start = (raw + rva - srva) as usize;
            return image.get(start..start + size as usize);
        }
    }
    None
}
