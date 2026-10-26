# uint32_t psxw_bios_call(a0, a1, a2, a3, table, func)
# Calls BIOS table `table` (0xA0, 0xB0 or 0xC0) function `func` (in $t1)
# with a0..a3. The BIOS is not trusted to keep $gp, so it is saved here.

    .set noreorder
    .section .text.psxw_bios_call, "ax", @progbits
    .global psxw_bios_call
    .type psxw_bios_call, @function
psxw_bios_call:
    addiu $sp, $sp, -32
    sw    $ra, 24($sp)
    sw    $gp, 28($sp)
    lw    $t2, 48($sp)
    lw    $t1, 52($sp)
    jalr  $t2
    nop
    lw    $ra, 24($sp)
    lw    $gp, 28($sp)
    jr    $ra
    addiu $sp, $sp, 32
    .size psxw_bios_call, . - psxw_bios_call
