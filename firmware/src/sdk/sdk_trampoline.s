.syntax unified
.text

@ Position-independent trampolines that get written on top of existing SDK wrapper functions to make
@ them branch to a runtime-configured `destination` instead of running their original body. See the
@ `sdk` module for how this is used. Two variants exist so the trampoline's instruction set can
@ match the function being patched.
@
@ Both variants are laid out as 8 bytes of code followed by a 4-byte slot that's filled in at
@ runtime with the destination pointer. Only `r12` (`ip`) is clobbered, which is sound since it's a
@ caller-preserved register that isn't used to pass arguments.

@ ---- ARM variant ----
.arm
.global v5gdb_sdk_trampoline_arm
.global v5gdb_sdk_trampoline_arm_end
v5gdb_sdk_trampoline_arm:
    ldr r12, .Larm_dest
    bx r12
v5gdb_sdk_trampoline_arm_end:
    @ Destination pointer.
.Larm_dest:
    .word 0

@ ---- Thumb variant ----
.thumb
.p2align 2
.global v5gdb_sdk_trampoline_thumb
.global v5gdb_sdk_trampoline_thumb_end
.thumb_func
v5gdb_sdk_trampoline_thumb:
    @ A PC-relative only works for multiples of 4, but we don't know how this routine will be
    @ aligned. Instead calculate the offset manually.
    @ r12 = . + 4, so an offset of +4 more will point to the slot
    mov.n r12, pc
    @ This might perform an unaligned read depending on the alignment of the function. VEXos
    @ disables alignment checking (SCTLR.A == 0) so this shouldn't be an issue.
    ldr.w r12, [r12, #4]
    bx.n r12
v5gdb_sdk_trampoline_thumb_end:
    @ Destination pointer.
    .word 0
