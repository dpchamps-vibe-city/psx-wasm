# uint32_t psxw_change_thread(uint32_t handle, struct psxw_call *c)
# ChangeThread (B0h:10h) with c->sp_before and c->gp set to sp and gp as
# the BIOS function sees them, and c->sp_after to sp once it returns
# (which, for a thread switched away, is when something switches back).
#
# void psxw_thread_entry(void)
# The thread entry BIOS_THREADS copies to its pc: passes sp and gp as the
# BIOS started the thread with them to psxw_thread_body, which never
# returns. Position-independent but for the absolute jump.

    .set noreorder
    .section .text.psxw_change_thread, "ax", @progbits
    .global psxw_change_thread
    .type psxw_change_thread, @function
psxw_change_thread:
    addiu $sp, $sp, -32
    sw    $ra, 24($sp)
    sw    $gp, 28($sp)
    sw    $s0, 20($sp)
    move  $s0, $a1
    sw    $sp, 0($s0)
    sw    $gp, 8($s0)
    li    $t2, 0xB0
    jalr  $t2
    li    $t1, 0x10
    sw    $sp, 4($s0)
    lw    $ra, 24($sp)
    lw    $s0, 20($sp)
    lw    $gp, 28($sp)  # also covers ra's load delay
    jr    $ra
    addiu $sp, $sp, 32
    .size psxw_change_thread, . - psxw_change_thread

    .section .text.psxw_thread_entry, "ax", @progbits
    .global psxw_thread_entry
    .type psxw_thread_entry, @function
psxw_thread_entry:
    move  $a0, $sp
    move  $a1, $gp
    lui   $t0, %hi(psxw_thread_body)
    addiu $t0, $t0, %lo(psxw_thread_body)
    jr    $t0
    nop
    .size psxw_thread_entry, . - psxw_thread_entry
