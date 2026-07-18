OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    skernel = .;

    stext = .;
    .text : {
        *(.text.entry)
        /* AGENT: retain the context-switch entry before the Rust scheduler has
           a fully runnable user-task path that references it at boot. */
        KEEP(*(.text.context))
        *(.text .text.*)
    }
    . = ALIGN(4K);
    etext = .;

    srodata = .;
    .rodata : {
        *(.rodata .rodata.*)
    }
    . = ALIGN(4K);
    erodata = .;

    PROVIDE(__global_pointer$ = . + 0x800);

    sdata = .;
    .data : {
        *(.sdata .sdata.*)
        *(.data .data.*)
    }
    . = ALIGN(4K);
    edata = .;

    sbss = .;
    .bss (NOLOAD) : {
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
    }
    . = ALIGN(4K);
    ebss = .;

    .bss.stack (NOLOAD) : {
        . = ALIGN(16);
        boot_stack_bottom = .;
        . += 64K;
        boot_stack_top = .;
    }

    . = ALIGN(4K);
    sheap = .;
    .bss.heap (NOLOAD) : {
        . += 1M;
    }

    . = ALIGN(4K);
    eheap = .;

    . = ALIGN(4K);
    ekernel = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
    }
}
