    .section .text.entry
    .globl _start

_start:
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop

    la sp, boot_stack_top
    call rust_main

1:
    wfi
    j 1b
