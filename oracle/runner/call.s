# uint32_t psxw_guest_call(a0, a1, a2, a3, entry, sp)
# Calls `entry` with a0..a3 and $sp = `sp`. The callee is original code
# under test, so every register the runner relies on (s0-s8, gp, sp, ra)
# is saved in psxw_guest_save rather than trusted to o32. v1 goes to
# psxw_guest_v1; v0 is returned.

    .set noreorder
    .set noat
    .section .text.psxw_guest_call, "ax", @progbits
    .global psxw_guest_call
    .type psxw_guest_call, @function
psxw_guest_call:
    lw    $t0, 16($sp)
    lw    $t1, 20($sp)
    la    $t2, psxw_guest_save
    sw    $sp, 0($t2)
    sw    $ra, 4($t2)
    sw    $gp, 8($t2)
    sw    $s0, 12($t2)
    sw    $s1, 16($t2)
    sw    $s2, 20($t2)
    sw    $s3, 24($t2)
    sw    $s4, 28($t2)
    sw    $s5, 32($t2)
    sw    $s6, 36($t2)
    sw    $s7, 40($t2)
    sw    $fp, 44($t2)
    move  $sp, $t1
    jalr  $t0
    nop
    la    $t2, psxw_guest_save
    lw    $sp, 0($t2)
    lw    $ra, 4($t2)
    lw    $gp, 8($t2)
    lw    $s0, 12($t2)
    lw    $s1, 16($t2)
    lw    $s2, 20($t2)
    lw    $s3, 24($t2)
    lw    $s4, 28($t2)
    lw    $s5, 32($t2)
    lw    $s6, 36($t2)
    lw    $s7, 40($t2)
    lw    $fp, 44($t2)
    la    $t2, psxw_guest_v1
    sw    $v1, 0($t2)
    jr    $ra
    nop
    .size psxw_guest_call, . - psxw_guest_call
