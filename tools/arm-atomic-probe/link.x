/* SPDX-License-Identifier: Apache-2.0 */

ENTRY(reset)

MEMORY
{
  FLASH : ORIGIN = 0x10000100, LENGTH = 2048K - 256
  RAM   : ORIGIN = 0x20000000, LENGTH = 264K
}

SECTIONS
{
  .text : { *(.text.reset) *(.text .text.*) } > FLASH
  .rodata : { *(.rodata .rodata.*) } > FLASH
  .data : { *(.data .data.*) } > RAM AT > FLASH
  .bss (NOLOAD) : { *(.bss .bss.*) *(COMMON) } > RAM
  /DISCARD/ : { *(.ARM.exidx*) }
}

