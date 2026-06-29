/* bare_uart.ld — the Phase 0 linker script for QEMU mps2-an385 (Cortex-M3).
   Code/vectors live at 0x00000000 (the board's boot ZBT region), data/bss in
   SRAM at 0x20000000. _estack is the top of RAM (the reset vector's SP). */
ENTRY(Reset_Handler)

MEMORY {
  FLASH (rx) : ORIGIN = 0x00000000, LENGTH = 4M
  RAM  (rwx) : ORIGIN = 0x20000000, LENGTH = 4M
}

_estack = ORIGIN(RAM) + LENGTH(RAM);

SECTIONS {
  .isr_vector : { KEEP(*(.isr_vector)) } > FLASH
  .text   : { *(.text*) *(.rodata*) } > FLASH
  .data   : { *(.data*) } > RAM
  .bss    : { *(.bss*)  } > RAM
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) }
}
