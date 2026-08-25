/* SPDX-License-Identifier: Apache-2.0 */

.syntax unified
.cpu cortex-m0plus
.thumb

.section .vector_table, "a", %progbits
.align 8
.global _vector_table_start
_vector_table_start:
    .word _stack_top
    .word Reset
    .word DefaultHandler
    .word HardFault
    .word DefaultHandler
    .word DefaultHandler
    .word DefaultHandler
    .word 0
    .word 0
    .word 0
    .word 0
    .word SVC
    .word DefaultHandler
    .word 0
    .word PendSV
    .word SysTick
    .rept 15
    .word ExternalIrq
    .endr
    .word SioIrq15
    .word SioIrq16
    .rept 15
    .word ExternalIrq
    .endr

.section .text.Reset, "ax", %progbits
.thumb_func
.global Reset
Reset:
    cpsid i
    /* Arm recovery before touching RAM. A panic gets one direct flash reboot
     * to report retained SRAM; other marked watchdogs enter ROM USB boot. */
    ldr r0, =0x40058008
    ldr r1, [r0]
    movs r2, #1
    tst r1, r2
    beq 8f
    ldr r0, =0x4005801c
    ldr r1, [r0]
    ldr r2, =0x6ab73121
    cmp r1, r2
    bne 8f
    ldr r3, =0x40058020
    ldr r1, [r3]
    ldr r2, =0x50414e32
    cmp r1, r2
    bne 7f
    movs r1, #0
    str r1, [r3]
    str r1, [r0]
    b 8f
7:
    movs r1, #0
    str r1, [r0]
    ldr r0, =0x00000014
    ldrh r0, [r0]
    ldr r2, =0x00000018
    ldrh r2, [r2]
    movs r1, #1
    orrs r2, r1
    ldr r1, =0x00004255
    blx r2
    mov r2, r0
    movs r0, #0
    movs r1, #0
    blx r2
8:  /* Match pico-sdk watchdog_enable(): marker, disable, reset selection,
     * debug-pause policy, load, then enable through atomic aliases. */
    ldr r0, =0x4005801c
    ldr r1, =0x6ab73121
    str r1, [r0]
    ldr r0, =0x4005b000
    ldr r1, =0x40000000
    str r1, [r0]
    ldr r0, =0x40012008
    ldr r1, =0x0001fffc
    str r1, [r0]
    ldr r0, =0x4005b000
    ldr r1, =0x07000000
    str r1, [r0]
    ldr r0, =0x4005802c
    ldr r1, =0x0000020c
    str r1, [r0]
    ldr r0, =0x40058004
    ldr r1, =0x005b8d80
    str r1, [r0]
    ldr r0, =0x4005a000
    ldr r1, =0x40000000
    str r1, [r0]
    ldr r0, =0xe000ed08
    ldr r1, =_vector_table_start
    str r1, [r0]
    ldr r0, =0xe000ed20
    ldr r1, =0xc0ff0000
    str r1, [r0]
    ldr r0, =_sidata
    ldr r1, =_sdata
    ldr r2, =_edata
1:  cmp r1, r2
    bcs 2f
    ldr r3, [r0]
    str r3, [r1]
    adds r0, #4
    adds r1, #4
    b 1b
2:  ldr r1, =_sbss
    ldr r2, =_ebss
    movs r0, #0
3:  cmp r1, r2
    bcs 4f
    str r0, [r1]
    adds r1, #4
    b 3b
4:  ldr r1, =0xd0000100
    ldr r2, =0xd0000180
    movs r0, #0
9:  str r0, [r1]
    adds r1, #4
    cmp r1, r2
    bne 9b
    bl _flint_armv6m_boot
    cpsie i
    svc 0
5:  wfi
    b 5b

/* Entered by the RP2040 ROM core-1 launch sequence with MSP already set to
 * _core1_boot_stack_top. Global RAM and spinlock initialization belongs to
 * core 0 and must never be repeated here. */
.section .text.Core1Entry, "ax", %progbits
.thumb_func
.global _flint_armv6m_core1_entry
_flint_armv6m_core1_entry:
    cpsid i
    ldr r0, =0xe000ed08
    ldr r1, =_vector_table_start
    str r1, [r0]
    ldr r0, =0xe000ed20
    ldr r1, =0xc0ff0000
    str r1, [r0]
    bl _flint_armv6m_core1_boot
    cpsie i
    svc 0
10: wfi
    b 10b

.thumb_func
.global SVC
SVC:
    bl _flint_armv6m_start_tick
    bl _flint_armv6m_first_stack
    mov r1, r0
    adds r1, #16
    ldmia r1!, {r4-r7}
    mov r8, r4
    mov r9, r5
    mov r10, r6
    mov r11, r7
    ldmia r0!, {r4-r7}
    msr psp, r1
    ldr r0, =0xfffffffd
    mov lr, r0
    cpsie i
    bx lr

.thumb_func
.global PendSV
PendSV:
    cpsid i
    mrs r0, psp
    subs r0, #32
    stmia r0!, {r4-r7}
    mov r4, r8
    mov r5, r9
    mov r6, r10
    mov r7, r11
    stmia r0!, {r4-r7}
    subs r0, #32
    push {r3, lr}
    bl _flint_armv6m_switch
    pop {r2, r3}
    mov lr, r3
    mov r1, r0
    adds r1, #16
    ldmia r1!, {r4-r7}
    mov r8, r4
    mov r9, r5
    mov r10, r6
    mov r11, r7
    ldmia r0!, {r4-r7}
    msr psp, r1
    cpsie i
    bx lr

.thumb_func
.global SysTick
SysTick:
    push {r0, lr}
    bl _flint_armv6m_systick
    pop {r0, pc}

.thumb_func
.global ExternalIrq
ExternalIrq:
    push {r0, lr}
    mrs r0, ipsr
    subs r0, #16
    bl _flint_armv6m_external_irq
    pop {r0, pc}

.thumb_func
.global SioIrq15
SioIrq15:
    push {r0, lr}
    movs r0, #15
    bl _flint_armv6m_sio_irq
    pop {r0, pc}

.thumb_func
.global SioIrq16
SioIrq16:
    push {r0, lr}
    movs r0, #16
    bl _flint_armv6m_sio_irq
    pop {r0, pc}

.thumb_func
.global HardFault
HardFault:
    mov r2, lr
    mov r0, r2
    movs r1, #4
    tst r0, r1
    beq 7f
    mrs r0, psp
    b 8f
7:  mrs r0, msp
8:  mov r1, r2
    bl _flint_armv6m_hard_fault

.thumb_func
.global DefaultHandler
DefaultHandler:
    cpsid i
6:  wfi
    b 6b
