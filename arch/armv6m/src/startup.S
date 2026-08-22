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
    .word DefaultHandler
    .word DefaultHandler
    .word DefaultHandler
    .word DefaultHandler
    .word 0
    .word 0
    .word 0
    .word 0
    .word DefaultHandler
    .word DefaultHandler
    .word 0
    .word PendSV
    .word SysTick
    .rept 32
    .word DefaultHandler
    .endr

.section .text.Reset, "ax", %progbits
.thumb_func
.global Reset
Reset:
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
4:  bl flint_app_main
5:  wfi
    b 5b

.thumb_func
.global PendSV
PendSV:
    mrs r0, psp
    subs r0, #32
    stmia r0!, {r4-r7}
    mov r4, r8
    mov r5, r9
    mov r6, r10
    mov r7, r11
    stmia r0!, {r4-r7}
    subs r0, #32
    bl _flint_armv6m_switch
    mov r1, r0
    adds r1, #16
    ldmia r1!, {r4-r7}
    mov r8, r4
    mov r9, r5
    mov r10, r6
    mov r11, r7
    ldmia r0!, {r4-r7}
    msr psp, r0
    bx lr

.thumb_func
.global SysTick
SysTick:
    push {lr}
    bl _flint_armv6m_systick
    pop {pc}
    bx lr

.thumb_func
.global DefaultHandler
DefaultHandler:
    cpsid i
6:  wfi
    b 6b
