    .section .text.trap
    .globl __kernel_trap_entry
    .align 2

__kernel_trap_entry:
    addi sp, sp, -320
    sd zero, 0(sp)
    sd x1, 8(sp)
    sd x5, 40(sp)
    addi t0, sp, 320
    sd t0, 16(sp)
    sd x3, 24(sp)
    sd x4, 32(sp)
    sd x6, 48(sp)
    sd x7, 56(sp)
    sd x8, 64(sp)
    sd x9, 72(sp)
    sd x10, 80(sp)
    sd x11, 88(sp)
    sd x12, 96(sp)
    sd x13, 104(sp)
    sd x14, 112(sp)
    sd x15, 120(sp)
    sd x16, 128(sp)
    sd x17, 136(sp)
    sd x18, 144(sp)
    sd x19, 152(sp)
    sd x20, 160(sp)
    sd x21, 168(sp)
    sd x22, 176(sp)
    sd x23, 184(sp)
    sd x24, 192(sp)
    sd x25, 200(sp)
    sd x26, 208(sp)
    sd x27, 216(sp)
    sd x28, 224(sp)
    sd x29, 232(sp)
    sd x30, 240(sp)
    sd x31, 248(sp)

    csrr t0, sstatus
    sd t0, 256(sp)
    csrr t0, sepc
    sd t0, 264(sp)
    sd zero, 272(sp)
    sd zero, 280(sp)
    sd zero, 288(sp)
    sd zero, 296(sp)
    sd zero, 304(sp)

    mv a0, sp
    call rust_kernel_trap

    ld t0, 264(sp)
    csrw sepc, t0
    ld t0, 256(sp)
    csrw sstatus, t0

    ld x1, 8(sp)
    ld x3, 24(sp)
    ld x4, 32(sp)
    ld x5, 40(sp)
    ld x6, 48(sp)
    ld x7, 56(sp)
    ld x8, 64(sp)
    ld x9, 72(sp)
    ld x10, 80(sp)
    ld x11, 88(sp)
    ld x12, 96(sp)
    ld x13, 104(sp)
    ld x14, 112(sp)
    ld x15, 120(sp)
    ld x16, 128(sp)
    ld x17, 136(sp)
    ld x18, 144(sp)
    ld x19, 152(sp)
    ld x20, 160(sp)
    ld x21, 168(sp)
    ld x22, 176(sp)
    ld x23, 184(sp)
    ld x24, 192(sp)
    ld x25, 200(sp)
    ld x26, 208(sp)
    ld x27, 216(sp)
    ld x28, 224(sp)
    ld x29, 232(sp)
    ld x30, 240(sp)
    ld x31, 248(sp)
    ld x2, 16(sp)
    sret

    # AGENT: execute the user satp transition only from this one dual-mapped
    # page; no low-linked kernel code or high-half stack is touched while the
    # process root is active.
    .section .text.trampoline, "ax"
    .globl __user_trap_entry
    .globl __user_trap_return
    .align 2

__user_trap_entry:
    csrrw sp, sscratch, sp
    addi sp, sp, -320
    sd zero, 0(sp)
    sd x1, 8(sp)
    sd x5, 40(sp)
    csrr t0, sscratch
    sd t0, 16(sp)
    sd x3, 24(sp)
    sd x4, 32(sp)
    sd x6, 48(sp)
    sd x7, 56(sp)
    sd x8, 64(sp)
    sd x9, 72(sp)
    sd x10, 80(sp)
    sd x11, 88(sp)
    sd x12, 96(sp)
    sd x13, 104(sp)
    sd x14, 112(sp)
    sd x15, 120(sp)
    sd x16, 128(sp)
    sd x17, 136(sp)
    sd x18, 144(sp)
    sd x19, 152(sp)
    sd x20, 160(sp)
    sd x21, 168(sp)
    sd x22, 176(sp)
    sd x23, 184(sp)
    sd x24, 192(sp)
    sd x25, 200(sp)
    sd x26, 208(sp)
    sd x27, 216(sp)
    sd x28, 224(sp)
    sd x29, 232(sp)
    sd x30, 240(sp)
    sd x31, 248(sp)

    csrr t0, sstatus
    sd t0, 256(sp)
    csrr t0, sepc
    sd t0, 264(sp)

    # AGENT: all three values are read through the supervisor-only TRAP_CONTEXT alias
    # before installing kernel_satp; the next stack access uses kernel_frame.
    ld t0, 272(sp)
    ld t1, 288(sp)
    ld t2, 304(sp)
    csrw satp, t0
    sfence.vma
    mv sp, t1
    mv a0, sp
    jalr ra, t2, 0

    # AGENT: Rust may have replaced the address space during exec, so consume
    # the freshly prepared alias and token before the page-local return jump.
    ld a0, 296(sp)
    ld a1, 280(sp)
    j __user_trap_return

__restore_user_trap_frame:
    ld t0, 264(sp)
    csrw sepc, t0
    ld t0, 256(sp)
    csrw sstatus, t0

    addi t0, sp, 320
    csrw sscratch, t0

    ld x1, 8(sp)
    ld x3, 24(sp)
    ld x4, 32(sp)
    ld x5, 40(sp)
    ld x6, 48(sp)
    ld x7, 56(sp)
    ld x8, 64(sp)
    ld x9, 72(sp)
    ld x10, 80(sp)
    ld x11, 88(sp)
    ld x12, 96(sp)
    ld x13, 104(sp)
    ld x14, 112(sp)
    ld x15, 120(sp)
    ld x16, 128(sp)
    ld x17, 136(sp)
    ld x18, 144(sp)
    ld x19, 152(sp)
    ld x20, 160(sp)
    ld x21, 168(sp)
    ld x22, 176(sp)
    ld x23, 184(sp)
    ld x24, 192(sp)
    ld x25, 200(sp)
    ld x26, 208(sp)
    ld x27, 216(sp)
    ld x28, 224(sp)
    ld x29, 232(sp)
    ld x30, 240(sp)
    ld x31, 248(sp)
    ld x2, 16(sp)
    sret

__user_trap_return:
    csrw satp, a1
    sfence.vma
    mv sp, a0
    j __restore_user_trap_frame
