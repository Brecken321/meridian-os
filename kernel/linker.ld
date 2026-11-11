/* Meridian kernel linker script.
 *
 * The image is linked into the top 2 GiB of the address space and split into
 * three page-aligned segments so the boot manager can map them with the right
 * permissions: text (R-X), rodata (R--) and data/bss (RW-).
 */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(kernel_entry)

PHDRS
{
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0xFFFFFFFF80000000;
    __kernel_start = .;

    .text : ALIGN(4K) {
        *(.text.entry)
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata
    .meridian_fixup : ALIGN(8) {
        __fixup_start = .;
        KEEP(*(.meridian_fixup))
        __fixup_end = .;
    } :rodata
    .eh_frame_hdr : { *(.eh_frame_hdr) } :rodata
    .eh_frame : { *(.eh_frame) } :rodata

    . = ALIGN(4K);
    .data : {
        *(.data .data.*)
    } :data
    .got : { *(.got .got.*) } :data
    .bss : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    } :data

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ : { *(.comment) *(.note .note.*) }
}
