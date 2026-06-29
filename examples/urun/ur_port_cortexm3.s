@ ur_port_cortexm3.s — ARM Cortex-M3 PORT for the URun vertical slice.
@ The irreducible asm is the exception machinery: the vector table, the PendSV
@ context switch, and the first-thread launch. The RTOS POLICY (TCBs, the
@ priority ready list, suspend/resume, semaphore, queue) is authored in CAAP
@ (the ur_*.caap fragments). PendSV calls the CAAP scheduler `_ur_thread_schedule` as an
@ ordinary AAPCS callback: r0 = preempted PSP -> r0 = next thread's PSP.
@
@ Preemption is EVENT-DRIVEN (a blocking/posting API pends PendSV itself after
@ readying a higher-priority thread, exactly as URun does) AND TIME-DRIVEN:
@ SysTick (enabled by ur_systick_config) calls the CAAP `_ur_timer_interrupt`
@ on each tick to advance the time base, wake sleeps / expire object waits, and
@ run application timers, then pends PendSV so the scheduler re-evaluates.
  .syntax unified
  .cpu cortex-m3
  .thumb

  .section .isr_vector,"a",%progbits
  .word _estack            @ 0: initial MSP
  .word Reset_Handler      @ 1: reset
  .word Fault_Handler      @ 2: NMI
  .word Fault_Handler      @ 3: HardFault
  .word Fault_Handler      @ 4: MemManage
  .word Fault_Handler      @ 5: BusFault
  .word Fault_Handler      @ 6: UsageFault
  .word 0                  @ 7
  .word 0                  @ 8
  .word 0                  @ 9
  .word 0                  @ 10
  .word Fault_Handler      @ 11: SVCall
  .word 0                  @ 12: DebugMon
  .word 0                  @ 13
  .word PendSV_Handler     @ 14: PendSV
  .word SysTick_Handler    @ 15: SysTick

  .text
  .thumb_func
  .global Reset_Handler
Reset_Handler:
  bl c_entry
  b .

  @ Fault_Handler — emit 'F' to UART0 and spin (diagnostic).
  .thumb_func
  .global Fault_Handler
Fault_Handler:
  ldr r0, =UART0_DATA      @ board addr supplied by the build via --defsym
  movs r1, #70
  str r1, [r0]
  b .

  @ start_first_task(uint32 psp0) — launch the first thread on its PSP from
  @ thread mode. r0 = its initial PSP (points at the saved r4-r11 of a full
  @ 16-word frame).
  .thumb_func
  .global start_first_task
start_first_task:
  ldmia r0!, {r4-r11}      @ restore software-saved regs; r0 -> hw frame
  msr psp, r0
  movs r1, #2
  msr control, r1          @ thread mode now uses PSP
  isb
  pop {r0-r3, r12, lr}     @ pop r0-r3, r12, lr from the hw frame
  pop {r1}                 @ r1 = stacked PC (entry, thumb bit set)
  pop {r2}                 @ r2 = stacked xPSR (discard)
  cpsie i                  @ enable interrupts
  bx r1                    @ enter the first thread

  @ SysTick_Handler — the kernel tick. Run the CAAP timer interrupt
  @ (_ur_timer_interrupt: advance time, wake delayed threads, fire app timers)
  @ as an AAPCS callback, then pend PendSV so the scheduler runs at tail.
  .thumb_func
  .global SysTick_Handler
SysTick_Handler:
  push {lr}
  bl _ur_timer_interrupt
  pop {lr}
  ldr r0, =ICSR           @ board addrs supplied by the build via --defsym
  ldr r1, =PENDSVSET
  str r1, [r0]
  bx lr

  @ PendSV_Handler — the context switch. Save r4-r11 below the running PSP,
  @ hand the saved PSP to the CAAP scheduler (_ur_thread_schedule), restore the
  @ chosen thread's r4-r11, exception-return. lr (EXC_RETURN) is preserved
  @ across the AAPCS callback.
  .thumb_func
  .global PendSV_Handler
PendSV_Handler:
  mrs r0, psp
  stmdb r0!, {r4-r11}
  push {lr}
  bl _ur_thread_schedule   @ r0 = preempted PSP -> r0 = next thread's PSP
  pop {lr}
  ldmia r0!, {r4-r11}
  msr psp, r0
  bx lr

  .section .note.GNU-stack,"",%progbits
