/* SPDX-License-Identifier: Apache-2.0 */

/*
 * ESP32 (PICO-D4 / WROVER) Linker Script for FlintOS — flash-XIP layout.
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
  drom_seg    (R)  : ORIGIN = 0x3F400020, LENGTH = 0x3FFFE0   /* rodata in flash, 4M window */
  /* The IROM cache window is 0x400D0000-0x40400000, not 4M. Code placed past
   * 0x40400000 is not mapped by the MMU and faults on fetch. */
  irom_seg    (RX) : ORIGIN = 0x400D0020, LENGTH = 0x32FFE0   /* code in flash */

  /* ── Internal DRAM ───────────────────────────────────────────────────────
   *
   * Everything statically placed must fit below 0x3FFDC200. That bound is not
   * arbitrary: the ROM keeps its own data and stack in the SRAM1 region above
   * it during boot, which is why Espressif's own template caps static DRAM at
   * exactly 0x3FFB0000 + 0x2C200. Memory above the bound is reclaimable as
   * heap once the ROM is finished, but is not safe for compile-time placement.
   *
   * The previous map ran task_stacks to 0x3FFF0000 and put panic_region and
   * dma_pool above that, i.e. squarely in the ROM's working memory.
   *
   * Actual static usage is small -- .data 5,328 B and .bss 8,752 B -- so the
   * whole layout fits in the safe window with room to spare:
   *
   *   0x3FFB0000  dram_seg      64K   .data + .bss + kernel/boot stack
   *   0x3FFC0000  task_stacks   96K   per-task stacks
   *   0x3FFD8000  panic_region   4K   postmortem snapshot
   *   0x3FFD9000  dma_pool       8K   DMA-safe buffers
   *   0x3FFDB000  (4.5K spare up to the 0x3FFDC200 bound)
   */
  dram_seg    (RW) : ORIGIN = 0x3FFB0000, LENGTH = 0x10000    /* 64K data/bss/kstack */
  task_stacks (RW) : ORIGIN = 0x3FFC0000, LENGTH = 0x18000    /* 96K task stacks */
  /* 8-byte aligned so the u64 in PanicSnapshot is legally placed. */
  panic_region(RW) : ORIGIN = 0x3FFD8000, LENGTH = 0x1000     /* survives soft reset */
  /* Must be inside SRAM2 (0x3FFAE000-0x3FFDFFFF) to be reachable by the DMA
   * engines. The old location at 0x3FFF1000 was chosen without regard to that. */
  dma_pool    (RW) : ORIGIN = 0x3FFD9000, LENGTH = 0x2000     /* DMA-safe buffers */
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
    /* Claim the whole region, not just whatever landed in `.dma_buffer`.
     *
     * Nothing in the tree emits that section -- the pool is handed out at
     * runtime by `kernel::dma_broker`, not declared by a static -- so without
     * this the section was empty, start and end were the same address, and
     * every allocation failed with PoolExhausted. On the target only: the
     * broker's host tests state a pool size rather than deriving one from
     * these symbols, so they passed throughout. */
    . = ORIGIN(dma_pool) + LENGTH(dma_pool);
    _dma_pool_end = ABSOLUTE(.);
  } > dma_pool
}

/* Guard: an empty pool makes every DMA allocation fail, on hardware only, in
 * a way no host test can see. Cheaper to catch at link time. */
ASSERT(_dma_pool_end - _dma_pool_start >= 4096,
       "DMA pool is empty or too small -- see .dma_pool in this script");

/* Guard: data RAM must not overflow into the task stack pool. */
ASSERT(_kernel_stack_end <= ORIGIN(task_stacks),
       "dram_seg overflowed into task_stacks - reduce static data or kernel stack")
