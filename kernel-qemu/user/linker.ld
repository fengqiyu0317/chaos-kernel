OUTPUT_ARCH(riscv)
ENTRY(_start)

/* AGENT: emit only loader-supported fixed-address PT_LOAD segments with
   permissions separated between code, constants, and writable state. */
PHDRS
{
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
}

/* AGENT: keep every segment page-congruent for the strict kernel ELF loader. */
SECTIONS
{
    . = 0x10000;

    .text : ALIGN(4K) {
        KEEP(*(.text.entry))
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata : {
        *(.rodata .rodata.*)
    } :rodata

    . = ALIGN(4K);
    .data : {
        *(.sdata .sdata.*)
        *(.data .data.*)
    } :data

    .bss (NOLOAD) : {
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
        *(.riscv.attributes)
    }
}
