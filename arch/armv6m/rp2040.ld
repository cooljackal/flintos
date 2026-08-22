/* SPDX-License-Identifier: Apache-2.0 */
/* RP2040 XIP image layout. The board supplies exactly one 256-byte boot2. */

ENTRY(Reset)

MEMORY
{
  FLASH (rx)  : ORIGIN = 0x10000000, LENGTH = 2048K
  RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 264K
}

SECTIONS
{
  .boot2 ORIGIN(FLASH) :
  {
    KEEP(*(.boot2));
  } > FLASH

  .vector_table ORIGIN(FLASH) + 0x100 :
  {
    KEEP(*(.vector_table));
  } > FLASH

  .text :
  {
    *(.text .text.*);
    *(.rodata .rodata.*);
  } > FLASH

  .data : AT(LOADADDR(.text) + SIZEOF(.text))
  {
    _sdata = .;
    *(.data .data.*);
    _edata = .;
  } > RAM
  _sidata = LOADADDR(.data);

  .bss (NOLOAD) :
  {
    _sbss = .;
    *(.bss .bss.* COMMON);
    _ebss = .;
  } > RAM

  .uninit (NOLOAD) :
  {
    *(.uninit .uninit.*);
  } > RAM

  .panic (NOLOAD) : ALIGN(4)
  {
    _panic_region_start = .;
    . += 1K;
    _panic_region_end = .;
  } > RAM

  .task_stacks (NOLOAD) : ALIGN(8)
  {
    _task_stack_start = .;
    . += 64K;
    _task_stack_end = .;
  } > RAM

  .dma_pool (NOLOAD) : ALIGN(4)
  {
    _dma_pool_start = .;
    . += 8K;
    _dma_pool_end = .;
  } > RAM

  _stack_top = ORIGIN(RAM) + LENGTH(RAM);
  /DISCARD/ : { *(.eh_frame .ARM.exidx .ARM.extab) }
}

ASSERT(SIZEOF(.boot2) == 0x100, "RP2040 boot2 must be exactly 256 bytes");
ASSERT(ADDR(.vector_table) == 0x10000100, "RP2040 vectors must follow boot2");
ASSERT(SIZEOF(.vector_table) >= 0xC0, "RP2040 vector table is incomplete");
ASSERT(_ebss <= _stack_top, "RP2040 RAM image exceeds SRAM");
