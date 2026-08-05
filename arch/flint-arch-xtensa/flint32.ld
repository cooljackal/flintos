/*
 * ESP32 (PICO-D4 / WROVER) Linker Script for Flint RTOS — flash-XIP layout.
 *
 * The ESP-IDF 2nd-stage bootloader requires flash-mapped code/rodata segments
 * (IROM/DROM) — a pure-IRAM image makes it abort() in set_cache_and_start_app.
 * So: code runs from flash via the XIP cache (IROM @ 0x400D0020), rodata from
 * DROM (@ 0x3F400020); only the exception vectors and the boot/asm hot path live
 * in internal IRAM; data/bss/stacks live in internal DRAM.
 *
 * The 0x20 offset on the flash segments leaves room for the image header so the
 * 64 KB-aligned cache page maps cleanly (standard ESP32 convention).
 */

ENTRY(_start)

/* Force the ESP-IDF app descriptor into the image past --gc-sections. */
EXTERN(esp_app_desc)

MEMORY
{
  /* Internal IRAM: exception vectors must sit at VECBASE. */
  vectors_seg (RX) : ORIGIN = 0x40080000, LENGTH = 0x400      /* 1K vectors */
  iram_seg    (RX) : ORIGIN = 0x40080400, LENGTH = 0x1FC00    /* boot + asm hot path */

  /* Flash, cached (XIP). */
  drom_seg    (R)  : ORIGIN = 0x3F400020, LENGTH = 0x3FFFE0   /* rodata in flash */
  irom_seg    (RX) : ORIGIN = 0x400D0020, LENGTH = 0x3FFFE0   /* code in flash */

  /* Internal DRAM. */
  dram_seg    (RW) : ORIGIN = 0x3FFB0000, LENGTH = 0x28000    /* 160K data/bss/kstack */
  task_stacks (RW) : ORIGIN = 0x3FFD8000, LENGTH = 0x18000    /* 96K task stacks */
  panic_region(RW) : ORIGIN = 0x3FFF0000, LENGTH = 0x1000     /* survives soft reset */
  dma_pool    (RW) : ORIGIN = 0x3FFF1000, LENGTH = 0x2000     /* DMA-safe buffers */
}

SECTIONS
{
  /* ── DROM (flash rodata) ───────────────────────────────────────── */
  /* The app descriptor MUST be the first content of the first (lowest-address)
   * segment so it lands at image offset 0x20 where the bootloader checks it. */
  .rodata_appdesc : ALIGN(16)
  {
    KEEP(*(.rodata_appdesc))
  } > drom_seg

  .rodata : ALIGN(4)
  {
    *(.rodata .rodata.*)
    *(.srodata .srodata.*)
  } > drom_seg

  /* ── Exception vectors (IRAM @ VECBASE) ────────────────────────── */
  .vectors : ALIGN(1024)
  {
    _vector_table_start = ABSOLUTE(.);
    KEEP(*(.vectors))
    KEEP(*(.vectors.*))
    _vector_table_end = ABSOLUTE(.);
  } > vectors_seg

  /* ── Boot + asm hot path (IRAM) ────────────────────────────────── */
  /* Startup runs before we rely on anything, and the context/trap asm must be
   * resident regardless of flash-cache state. */
  .iram.text : ALIGN(4)
  {
    KEEP(*(.literal.startup .text.startup))
    *(.iram .iram.* .iram1 .iram1.*)
  } > iram_seg

  /* ── Code (flash IROM, XIP) ────────────────────────────────────── */
  .text : ALIGN(4)
  {
    _stext = ABSOLUTE(.);
    *(.literal .text .literal.* .text.*)
    _etext = ABSOLUTE(.);
  } > irom_seg

  /* ── Data (DRAM, loaded from flash by the bootloader) ──────────── */
  .data : ALIGN(4)
  {
    _sdata = ABSOLUTE(.);
    *(.data .data.*)
    *(.sdata .sdata.*)
    _edata = ABSOLUTE(.);
  } > dram_seg

  /* ── BSS (DRAM, zeroed by startup.S) ───────────────────────────── */
  .bss (NOLOAD) : ALIGN(4)
  {
    _sbss = ABSOLUTE(.);
    *(.bss .bss.*)
    *(.sbss .sbss.*)
    *(COMMON)
    _ebss = ABSOLUTE(.);

    . = ALIGN(16);
    _kernel_stack_start = ABSOLUTE(.);
    . += 0x2000;                      /* 8K boot/idle stack */
    _kernel_stack_end = ABSOLUTE(.);
  } > dram_seg

  /* ── Per-task stack pool ───────────────────────────────────────── */
  .task_stacks (NOLOAD) : ALIGN(16)
  {
    _task_stack_start = ABSOLUTE(.);
    . += LENGTH(task_stacks);
    _task_stack_end = ABSOLUTE(.);
  } > task_stacks

  /* ── Panic snapshot region ─────────────────────────────────────── */
  .panic (NOLOAD) : ALIGN(8)
  {
    _panic_region_start = ABSOLUTE(.);
    *(.panic_snapshot)
    _panic_region_end = ABSOLUTE(.);
  } > panic_region

  /* ── DMA buffer pool ───────────────────────────────────────────── */
  .dma_pool (NOLOAD) : ALIGN(4)
  {
    _dma_pool_start = ABSOLUTE(.);
    *(.dma_buffer)
    _dma_pool_end = ABSOLUTE(.);
  } > dma_pool
}

/* Guard: data RAM must not overflow into the task stack pool. */
ASSERT(_kernel_stack_end <= ORIGIN(task_stacks),
       "dram_seg overflowed into task_stacks - reduce static data or kernel stack")
