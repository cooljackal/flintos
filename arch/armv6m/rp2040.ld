/* SPDX-License-Identifier: Apache-2.0 */
/* RP2040 XIP image layout. The board supplies exactly one 256-byte boot2. */

ENTRY(Reset)

MEMORY
{
  /* Final four sectors belong to board::active::NVS_PARTITION, not firmware. */
  FLASH (rx)  : ORIGIN = 0x10000000, LENGTH = 2032K
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

  /* Explicit user-only input sections. Kernel code and constants never fall
   * into this grant, including when the user image is smaller than a region. */
  .user_text : ALIGN(4096)
  {
    _user_text_start = .;
    KEEP(*(.user.text .user.text.* .user.rodata .user.rodata.*));
    _user_text_payload_end = .;
    . = _user_text_start + 4096;
    _user_text_end = .;
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
    . = ALIGN(4);
    _sram_func_start = .;
    *(.ram_func .ram_func.*);
    _sram_func_end = .;
    . = ALIGN(4);
    _edata = .;
  } > RAM
  _sidata = LOADADDR(.data);
  _nvs_partition_start = ORIGIN(FLASH) + LENGTH(FLASH);
  _nvs_partition_end = 0x10200000;

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

  /* The RP2040 ROM installs this stack before branching to core 1. Keep it
   * disjoint from core 0's downward-growing reset stack. */
  _core1_boot_stack_top = ORIGIN(RAM) + LENGTH(RAM);
  _core1_boot_stack_bottom = _core1_boot_stack_top - 1K;
  _stack_top = _core1_boot_stack_bottom;
  /DISCARD/ : { *(.eh_frame .ARM.exidx .ARM.extab) }
}

ASSERT(SIZEOF(.boot2) == 0x100, "RP2040 boot2 must be exactly 256 bytes");
ASSERT(_user_text_payload_end <= _user_text_end, "RP2040 isolated code exceeds its 4K grant");
ASSERT(ADDR(.vector_table) == 0x10000100, "RP2040 vectors must follow boot2");
ASSERT(SIZEOF(.vector_table) >= 0xC0, "RP2040 vector table is incomplete");
ASSERT(_dma_pool_end <= _stack_top, "RP2040 RAM image overlaps reset stacks");
ASSERT(LOADADDR(.data) + SIZEOF(.data) <= ORIGIN(FLASH) + LENGTH(FLASH),
       "RP2040 firmware overlaps reserved NVS sectors");
