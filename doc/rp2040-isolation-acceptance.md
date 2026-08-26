<!-- SPDX-License-Identifier: Apache-2.0 -->
# RP2040 opt-in task isolation (#139)

## Contract and threat boundary

`kernel/task-isolation` enables private compute tasks created by the trusted
application through `kernel::isolation::spawn`. Ordinary `api::task` tasks
remain privileged. This is **not** a switch that makes existing drivers,
queues, callbacks or application globals safe for untrusted code.

| Resource | Grant to an isolated task |
|---|---|
| Executable image | Shared, read-only 4 KiB `.user_text` region; only explicit `.user.text*` / `.user.rodata*` inputs |
| Stack | Exclusive power-of-two allocation, 1–16 KiB; bottom eighth inaccessible, remaining 7/8 RW and execute-never |
| Data | Optional exclusive power-of-two allocation, 256–4096 bytes, zeroed, RW and execute-never |
| Other memory | No unprivileged grant: kernel code/data, other tasks, ROM, aliases, DMA, SIO, peripherals and core-control registers |
| Kernel entry | Pointer-free supervisor operations: yield, exit, own task/core ID, low 32-bit tick count, own data base/size; unknown operations return `u32::MAX` |
| DMA | No isolated-task DMA API or register grant. Trusted drivers and bus masters are outside the CPU MPU boundary. |

The layout contract lives in `hal::isolation`, policy/allocation in the kernel,
and native MPU registers/encoding in `arch/armv6m`. The eight hardware slots
are budgeted as code, stack, optional data and five disabled slots. Every domain
change replaces all slots on the receiving core. Trusted-to-trusted switches
reuse the already-empty user map. Both cores initialize their own MPU; a missing
or unexpected hardware configuration is fatal before user execution.

The allocator never expands a requested grant, consumes neither allocation on
exhaustion, and never reuses isolated stack/data memory before reboot. A task
entry outside the user code region, misaligned/non-power-of-two layout, bad
affinity or exhausted pool is rejected. The linker refuses an oversized user
image. Stack pointers are checked before privileged software saves or SVC frame
access. `CONTROL` is restored from trusted task policy, not the user frame.

An isolated function must place every reachable helper/constant into its
explicit user section or inline it. Ordinary Rust library helpers can remain
in kernel `.text`; calling them faults. The restricted API wrappers inline.
Returning from an entry invokes a user-page exit veneer. Suspension on exit
retains the allocation and TCB; general-purpose userspace IPC, allocation,
task restart and arbitrary shared-memory grants are not implemented.
This is a memory-permission boundary, not an availability guarantee: unexpected
faults remain system-fatal, and these tests do not establish recovery from every
possible malformed exception stack, CPU lockup or malicious instruction stream.

## Hardware inventory and references

| Supported processor | Hardware and backend disposition |
|---|---|
| RP2040 Cortex-M0+ (Pico and Wio) | Eight configurable MPU regions, minimum 256 bytes, eight subregions and execute-never. Implemented backend; physical tests use the Pico. |
| Classic ESP32 Xtensa LX6 (all current ESP32 boards) | ESP-IDF's CPU region capabilities describe coarse fixed 512 MiB regions, but the SoC TRM separately documents per-core PID controllers and PID-based protection of memory/peripherals. These are not the RP2040 backend. Flint's isolation backend is unimplemented and enabling the feature fails compilation; hardware absence has **not** been established. PMS/PID assessment and any viable implementation remain under #139. |

Primary sources checked against implementation:

- [RP2040 datasheet](https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf),
  §2.4.6 and M0PLUS MPU register descriptions. Violations enter HardFault;
  ARMv6-M does not provide the ARMv7-M CFSR/MMFAR fault-cause registers.
- Pico SDK 2.1.1, `src/rp2040/hardware_regs/include/hardware/regs/m0plus.h`:
  MPU_TYPE, control/background-map policy, region registers and subregion mask.
- [Zephyr Cortex-M architecture](https://docs.zephyrproject.org/latest/hardware/arch/arm_cortex_m.html),
  plus `arch/arm/core/cortex_m/swap_helper.S` and `svc.S`: aligned user stacks,
  privilege restoration and privileged kernel-entry stacks. Flint's bounded
  calls stay in handler mode on MSP; blocking/general Zephyr-style syscalls
  are intentionally not exposed.
- [ESP-IDF ESP32 capabilities](https://github.com/espressif/esp-idf/blob/master/components/soc/esp32/include/soc/soc_caps.h)
  and [LX6 core configuration](https://github.com/espressif/esp-idf/blob/master/components/xtensa/esp32/include/xtensa/config/core-isa.h).
- [Espressif's privilege-separation design](https://developer.espressif.com/blog/introducing-esp-privilege-separation/)
  reports limitations in the classic ESP32 prototype and the addition of new
  permission/world-controller hardware in ESP32-C3. This supports withholding
  an LX6 isolation claim, not claiming every classic ESP32 protection mechanism
  is absent or proving no narrower PMS/PID design could work.
- [ESP32 technical reference](https://documentation.espressif.com/ESP32_Technical_Reference_Manual_en.pdf),
  chapters 4 and 13: per-core process identifiers and memory/peripheral
  protection are distinct from the Xtensa CPU region-capability macros. No
  classic ESP32 target result is claimed here; that part of #139 remains open.

## Fault evidence and reproducible tests

Production does not call every HardFault an MPU violation. A per-core protected
active-domain snapshot identifies the task without taking the shared scheduler
lock. The retained 12-word record includes task/core, frame pointer, exact PC,
xPSR, exception return, region bases and r0/r1. Unexpected faults take the
normal fatal panic/watchdog-reboot path; the previous-boot report uses the
actual fault PC rather than the stale last context-switch frame.

`kernel/isolation-test` is a separate **test-only** feature. Its recovery hook
accepts only the protected, one-shot task/PC/address manifest for a deliberately
denied instruction. The production undefined-instruction variant does not link
that hook and must reboot/report the fault, not count it as protection success.

```sh
make test-arm-isolation ARM_USB_LOCATION="<Pico physical USB location>"
make test-arm-isolation ARM_ISOLATION_HZ=125000000 ARM_USB_LOCATION="<location>"
make test-arm-isolation-fault ARM_USB_LOCATION="<location>"
make test-arm-isolation-fault ARM_ISOLATION_FAULT_CORE=1 ARM_USB_LOCATION="<location>"
```

The host harness takes exclusive fixture ownership, derives the UF2 from the
selected ELF, and uses a fresh random nonce. It resets to ROM before updating:
the observed SWD flash attempt from a live isolated image failed. A subsequent
watchdog reset cleared MPU state. ROM USB enumeration was intermittent during
these runs; the harness permits one bounded SWD-download alternative, never
a retry of bad test evidence. Neither recovery path requires manual BOOTSEL.
This is measured recovery behavior, not a claim that live MPU state was the
sole cause of the original flashing failure.

The positive/negative suite runs four private-memory workers (two per core),
800 checked iterations, and counts actual user-domain activations. Twelve
denials per core cover kernel read/write, other data/stack write, guard write,
data execution, code write, SIO/DMA/MPU-register reads, XIP alias read and kernel
code execution. It also attempts to clear user privilege and invokes SVC zero
with an unknown operation; neither may enter the boot dispatcher or elevate.
Three invalid spawn configurations must be refused.

During development, the first fault reporter used `scheduler::try_with`.
Core 0's ten denial cases passed, but core 1's first denial reached the generic
HardFault recorder with no isolated-task record. Replacing that shared-lock
dependency with the per-core snapshot allowed both cores' denial cases to pass.
The test controller polls every 10 ms: its earlier higher-priority 1 ms polling
starved the workers at the 12 MHz debug-build profile. No tighter isolation
context-switch latency guarantee is claimed.

At 125 MHz an initial run completed all 24 denials and 800 iterations but
recorded only 132/116 user activations, correctly failing the 200/core floor.
The scheduler shares its round-robin cursor between cores, so a remote dispatch
can make a yield select the same local task. Workers now start behind a gate
and use a distinct equal-priority peer ring on each core; the test still judges
actual activations rather than counting calls to yield. This does not change
the production scheduler or assert cross-core round-robin fairness.

Measured on 2026-08-26 on the Pico with probe `4150325537323116`, UART `COM9` and ROM serial
`E0C912D24340`; logs below are local, ignored build artifacts. All four positive
runs reported 24 denials, 3 rejected spawns and 800 checked worker iterations.

| Check | Measured result | Local evidence |
|---|---|---|
| 125 MHz, run 1 | PASS; nonce 655849296; user activations 423 / 415 | `target/arm139-final-125000000-1.log` |
| 125 MHz, run 2 | PASS; nonce 1946114218; user activations 423 / 415 | `target/arm139-final-125000000-2.log` |
| 12 MHz, run 1 | PASS; nonce 2123604072; user activations 681 / 414 | `target/arm139-final-12000000-1.log` |
| 12 MHz, run 2 | PASS; nonce 2036834783; user activations 687 / 414 | `target/arm139-final-12000000-2.log` |
| Unexpected instruction, core 0 | PASS; nonce 1523772986; task 3, PC `0x1000100c`, retained report after automatic reboot | `target/arm139-fault-core0.log` |
| Unexpected instruction, core 1 | PASS; nonce 2074322354; task 3, PC `0x1000100c`, retained report after automatic reboot | `target/arm139-fault-core1.log` |
| Ordinary I/O with isolation enabled | PASS; 1,000 UART payloads, 16,000 bytes and 10,000 physical GPIO edges | `target/arm139-io-final.log` |
| Trusted DMA with isolation enabled | PASS; 100 × 512-byte UART loopback transfers and timeout cleanup; fresh UF2 loaded through the matched ROM device, then UART plus SWD result | `target/arm139-dma-final.log` |
| Mutexes with isolation enabled | PASS; 2,000 priority-inheritance cycles across both cores | `target/arm139-mutex-final.log` |
| Interrupt races with isolation enabled | PASS; 10,000 exact physical-ISR queue deliveries and 2,500 nested critical-section exits | `target/arm139-race-final.log` |
| Isolation disabled | PASS; original kernel/I/O image, including 1,000 UART payloads and 10,000 physical GPIO edges | `target/arm139-plain-io-final.log` |
| USB with isolation enabled | PASS; image `13900001`, two native USB update/reconnect cycles (13,503 / 13,390 ms, no SWD fallback), watchdog-ROM recovery and stalled-task SWD recovery; fresh data and descriptor checks | `target/arm139-usb-final.log` |
| Unsupported backend | Classic ESP32 selection plus `task-isolation` fails with the explicit unsupported diagnostic | `target/arm139-unsupported.log` |
| Required commit gates | PASS: 1,216 Rust test executions, 27 isolation harness fixtures, existing harness fixtures, lint, layers and full matrix | `target/arm139-gates-final.log` |

Review of the production fatal path found an additional reporting race: if the first
nonblocking scheduler lookup fails but a second succeeds, the unknown task ID
must not be used to index the task array. The reporter now bounds-checks it.
The task ID and actual interrupted PC remain in the isolated fault record even
when scheduler metadata is unavailable.

The fixture was left running the responsive `13900001` USB image with
`kernel/task-isolation` enabled; its linked supervisor/PSP-validation symbols
were checked. No manual BOOTSEL or power cycle was used in these acceptance
runs. The USB image's ordinary service task remains privileged, as intended.
