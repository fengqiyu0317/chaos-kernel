OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;
    skernel = .;

    stext = .;
    .text.entry : {
        *(.text.entry)
        /* AGENT: retain the context-switch entry before the Rust scheduler has
           a fully runnable user-task path that references it at boot. */
        KEEP(*(.text.context))
    }

    /* AGENT: keep the satp-switching user trap code inside one page so the
       kernel and every user page table can map it at the same virtual address. */
    . = ALIGN(4K);
    strampoline = .;
    .text.trampoline : {
        KEEP(*(.text.trampoline))
    }
    etrampoline = .;
    ASSERT(etrampoline - strampoline <= 4K, "user trampoline exceeds one page")

    .text : {
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
