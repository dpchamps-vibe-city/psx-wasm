# psxw runner entry point. Replaces PSn00bSDK's _start: nothing of the SDK
# runtime (heap, interrupt handlers, BIOS calls) is initialised.
#
#   - interrupts off (SR.IEc and SR.IM2 cleared), GTE on (SR.CU2);
#   - stack just below the top of RAM, above the mailbox;
#   - BSS cleared;
#   - runner_main() never returns.

    .set noreorder
    .set noat
    .section .text._start, "ax", @progbits
    .global _start
    .type _start, @function
_start:
    mfc0  $t0, $12
    nop
    li    $t1, 0xFFFFFBFE
    and   $t0, $t0, $t1
    lui   $t1, 0x4000
    or    $t0, $t0, $t1
    mtc0  $t0, $12
    nop
    li    $sp, 0x801FFF00
    la    $t0, __bss_start
    la    $t1, _end
1:
    sltu  $t2, $t0, $t1
    beqz  $t2, 2f
    nop
    sw    $zero, 0($t0)
    b     1b
    addiu $t0, $t0, 4
2:
    jal   runner_main
    nop
3:
    b     3b
    nop
    .size _start, . - _start
