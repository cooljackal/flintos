// SPDX-License-Identifier: Apache-2.0

//! Machine-judged RP2040 tests of the real FlintOS scheduler and kernel APIs.

#![no_std]
#![no_main]
#![cfg_attr(
    any(
        feature = "watchdog-reset",
        feature = "reset-recovery-smoke",
        feature = "diagnostics-smoke",
        feature = "dma-smoke",
        feature = "mutex-smoke",
        feature = "race-smoke",
        feature = "pwm-smoke",
        feature = "adc-entropy-smoke",
        feature = "bus-smoke",
        feature = "clock-smoke",
        feature = "pio-smoke",
        feature = "flash-smoke"
    ),
    allow(dead_code)
)]

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
use api::mutex::{lock, Mutex};
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
use api::queue::{Queue, RecvError};
use api::task;
#[cfg(not(feature = "expected-hardfault"))]
use api::timer;
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
use hal::stream::ByteStream;
use hal::types::Priority;
#[cfg(not(feature = "expected-hardfault"))]
use portable_atomic::{AtomicU32, Ordering};

#[cfg(feature = "diagnostics-smoke")]
static DIAGNOSTIC_COUNTER: api::Counter = api::Counter::new("arm_concurrent_updates");
#[cfg(feature = "diagnostics-smoke")]
static DIAGNOSTIC_GAUGE: api::Gauge = api::Gauge::new("arm_recovery_stage");
#[cfg(feature = "diagnostics-smoke")]
static DIAGNOSTIC_PEER_DONE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "diagnostics-smoke")]
#[no_mangle]
static FLINT_ARM_DIAGNOSTIC_STAGE: AtomicU32 = AtomicU32::new(0);

kernel::flint_app!(main, abi = 2);

#[cfg(feature = "flash-smoke")]
mod flash_test;

#[cfg(all(feature = "clock-smoke", target_arch = "arm"))]
mod clock_test;

#[cfg(all(feature = "pio-smoke", target_arch = "arm"))]
mod pio_test;

#[cfg(not(feature = "expected-hardfault"))]
static PEER_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static CORE1_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static WRONG_CORE_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static ISR_QUEUE: Queue<u32, 4> = Queue::new();
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static ISR_NEXT: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static ISR_PRODUCING: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static ISR_SENT: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_MUTEX: Mutex<u32> = Mutex::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[no_mangle]
static MUTEX_SOAK_PROGRESS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "mutex-smoke")]
static MUTEX_SOAK_CORES_DONE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "mutex-smoke")]
static MUTEX_SOAK_ERRORS: AtomicU32 = AtomicU32::new(0);

#[cfg(feature = "mutex-smoke")]
struct PiStress {
    mutex: Mutex<u32>,
    cycle: AtomicU32,
    low_id: AtomicU32,
    medium_id: AtomicU32,
    high_id: AtomicU32,
    medium_ready: AtomicU32,
    medium_finished: AtomicU32,
    high_finished: AtomicU32,
    medium_seen: AtomicU32,
    high_seen: AtomicU32,
}

#[cfg(feature = "mutex-smoke")]
impl PiStress {
    const fn new() -> Self {
        Self {
            mutex: Mutex::new(0),
            cycle: AtomicU32::new(0),
            low_id: AtomicU32::new(u32::MAX),
            medium_id: AtomicU32::new(u32::MAX),
            high_id: AtomicU32::new(u32::MAX),
            medium_ready: AtomicU32::new(0),
            medium_finished: AtomicU32::new(0),
            high_finished: AtomicU32::new(0),
            medium_seen: AtomicU32::new(0),
            high_seen: AtomicU32::new(0),
        }
    }
}

#[cfg(feature = "mutex-smoke")]
static PI_STRESS_CORE0: PiStress = PiStress::new();
#[cfg(feature = "mutex-smoke")]
static PI_STRESS_CORE1: PiStress = PiStress::new();
#[cfg(feature = "race-smoke")]
static RACE_QUEUE: Queue<u32, 8> = Queue::new();
#[cfg(feature = "race-smoke")]
static RACE_INPUT: api::Once<rp2040_gpio::Rp2040Pin> = api::Once::new();
#[cfg(feature = "race-smoke")]
#[no_mangle]
static RACE_ISR_HANDLED: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "race-smoke")]
#[no_mangle]
static RACE_ISR_SENT: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "race-smoke")]
#[no_mangle]
static RACE_TASK_RECEIVED: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "race-smoke")]
#[no_mangle]
static RACE_NESTED_MASKED: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "race-smoke")]
static RACE_ERRORS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "race-smoke")]
static RACE_ACTIVE_EDGE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "race-smoke")]
static RACE_PREEMPTIONS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
static PWM_INPUT: api::Once<rp2040_gpio::Rp2040Pin> = api::Once::new();
#[cfg(feature = "pwm-smoke")]
#[no_mangle]
static PWM_EDGE_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
#[no_mangle]
static PWM_PERIOD_US: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
#[no_mangle]
static PWM_HIGH_US: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
static PWM_RISES: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
static PWM_FALLS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
static PWM_FIRST_RISE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
static PWM_LAST_RISE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pwm-smoke")]
static PWM_ERRORS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ADC_SAMPLE_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ADC_MIN_RAW: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ADC_MAX_RAW: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ADC_AVG_RAW: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ADC_TEMP_MILLI_C: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ENTROPY_RAW_BITS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ENTROPY_RAW_ONES: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ENTROPY_TRANSITIONS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "adc-entropy-smoke")]
#[no_mangle]
static ENTROPY_CHECKSUM: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
static BUS_I2C_SLAVE: api::Once<rp2040_i2c::Rp2040I2c> = api::Once::new();
#[cfg(feature = "bus-smoke")]
static BUS_SLAVE_READY: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_MASTER_STAGE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_SLAVE_STAGE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
static BUS_FAULTS_DONE: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_SPI_TIMEOUT_US: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_I2C_TIMEOUT_US: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_SPI_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_SPI_CHECKSUM: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_I2C_TRANSACTIONS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_I2C_BYTES: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "bus-smoke")]
#[no_mangle]
static BUS_I2C_NACK_RECOVERED: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_PHASE: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_BOOST_SEEN: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_OWNER_ID: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_MEDIUM_ID: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_HIGH_ID: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_HIGH_PARKED: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_MEDIUM_PARKED: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_RESTORE_SEEN: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_MEDIUM_READY: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_MEDIUM_RAN_DURING_INVERSION: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static PI_MEDIUM_FINISHED: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static SMP_LOCK: kernel::smp::Spinlock<u32> = kernel::smp::Spinlock::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static CORE0_ACTIVE: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static CORE1_ACTIVE: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static DUPLICATE_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static REMOTE_REQUEST: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
static GPIO_LOOPBACK_INPUT: api::Once<rp2040_gpio::Rp2040Pin> = api::Once::new();
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
static GPIO_EDGE_COUNT: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
static GPIO_EDGE_ERRORS: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    r#"
    .syntax unified
    .cpu cortex-m0plus
    .thumb
    .global arm_sentinel_yield
    .type arm_sentinel_yield,%function
    .thumb_func
arm_sentinel_yield:
    push {r3-r7,lr}
    mov r4,r8
    mov r5,r9
    mov r6,r10
    mov r7,r11
    push {r4-r7}
    movs r4,#0x44
    movs r5,#0x45
    movs r6,#0x46
    movs r7,#0x47
    movs r0,#0x48
    mov r8,r0
    movs r0,#0x49
    mov r9,r0
    movs r0,#0x4a
    mov r10,r0
    movs r0,#0x4b
    mov r11,r0
    bl _flint_sys_yield
    movs r0,#0
    cmp r4,#0x44
    bne 1f
    cmp r5,#0x45
    bne 1f
    cmp r6,#0x46
    bne 1f
    cmp r7,#0x47
    bne 1f
    mov r1,r8
    cmp r1,#0x48
    bne 1f
    mov r1,r9
    cmp r1,#0x49
    bne 1f
    mov r1,r10
    cmp r1,#0x4a
    bne 1f
    mov r1,r11
    cmp r1,#0x4b
    bne 1f
    movs r0,#1
1:  pop {r4-r7}
    mov r8,r4
    mov r9,r5
    mov r10,r6
    mov r11,r7
    pop {r3-r7,pc}

    .global arm_sentinel_preempt
    .type arm_sentinel_preempt,%function
    .thumb_func
arm_sentinel_preempt:
    push {r3-r7,lr}
    mov r4,r8
    mov r5,r9
    mov r6,r10
    mov r7,r11
    push {r4-r7}
    movs r4,#0x54
    movs r5,#0x55
    movs r6,#0x56
    movs r7,#0x57
    movs r0,#0x58
    mov r8,r0
    movs r0,#0x59
    mov r9,r0
    movs r0,#0x5a
    mov r10,r0
    movs r0,#0x5b
    mov r11,r0
    ldr r0,=120000
2:  subs r0,#1
    bne 2b
    movs r0,#0
    cmp r4,#0x54
    bne 3f
    cmp r5,#0x55
    bne 3f
    cmp r6,#0x56
    bne 3f
    cmp r7,#0x57
    bne 3f
    mov r1,r8
    cmp r1,#0x58
    bne 3f
    mov r1,r9
    cmp r1,#0x59
    bne 3f
    mov r1,r10
    cmp r1,#0x5a
    bne 3f
    mov r1,r11
    cmp r1,#0x5b
    bne 3f
    movs r0,#1
3:  pop {r4-r7}
    mov r8,r4
    mov r9,r5
    mov r10,r6
    mov r11,r7
    pop {r3-r7,pc}

    .global arm_inject_expected_fault
    .type arm_inject_expected_fault,%function
    .thumb_func
arm_inject_expected_fault:
    .global _flint_expected_fault_start
_flint_expected_fault_start:
    udf #0
    .global _flint_expected_fault_end
_flint_expected_fault_end:
    b _flint_expected_fault_end
"#,
    options(raw)
);

#[cfg(not(feature = "expected-hardfault"))]
#[allow(dead_code)]
unsafe extern "C" {
    fn arm_sentinel_yield() -> u32;
    fn arm_sentinel_preempt() -> u32;
}

#[cfg(feature = "expected-hardfault")]
unsafe extern "C" {
    fn arm_inject_expected_fault() -> !;
}

fn main() {
    #[cfg(all(feature = "pio-smoke", target_arch = "arm"))]
    task::spawn_on(0, "pio", pio_test::run, Priority::Normal(1), 8192)
        .expect("PIO test task");
    #[cfg(all(feature = "clock-smoke", target_arch = "arm"))]
    {
        task::spawn_on(0, "clock", clock_test::run, Priority::Normal(1), 4096)
            .expect("clock test task");
        task::spawn_on(1, "clock-peer", clock_test::peer, Priority::Normal(1), 2048)
            .expect("clock peer task");
    }
    #[cfg(feature = "flash-smoke")]
    task::spawn_on(0, "flash-smoke", flash_test::run, Priority::Normal(1), 8192)
        .expect("flash target test");
    #[cfg(feature = "dma-smoke")]
    task::spawn("dma", dma_test, Priority::Normal(0), 4096).expect("DMA test task");
    #[cfg(feature = "diagnostics-smoke")]
    {
        if kernel::debug::panic::previous_was_reported() {
            task::spawn(
                "diagnostics-report",
                diagnostics_reported,
                Priority::Normal(0),
                2048,
            )
            .expect("diagnostics report task");
        } else {
            task::spawn_on(
                1,
                "diagnostics-peer",
                diagnostics_peer,
                Priority::Normal(1),
                2048,
            )
            .expect("diagnostics peer task");
            task::spawn_on(
                0,
                "diagnostics",
                diagnostics_test,
                Priority::Normal(1),
                2048,
            )
            .expect("diagnostics task");
        }
    }
    #[cfg(feature = "mutex-smoke")]
    {
        spawn_pi_stress_low(0, pi_low_core0);
        spawn_pi_stress_low(1, pi_low_core1);
    }
    #[cfg(feature = "race-smoke")]
    task::spawn_on(0, "race-producer", race_producer, Priority::Normal(2), 4096)
        .expect("physical interrupt race producer");
    #[cfg(feature = "pwm-smoke")]
    task::spawn_on(0, "pwm-smoke", pwm_smoke, Priority::Normal(1), 4096).expect("PWM target test");
    #[cfg(feature = "adc-entropy-smoke")]
    task::spawn_on(
        0,
        "adc-entropy",
        adc_entropy_smoke,
        Priority::Normal(1),
        4096,
    )
    .expect("ADC and entropy target test");
    #[cfg(feature = "bus-smoke")]
    {
        let slave = rp2040_i2c::Rp2040I2c::open_slave(
            &kernel::board::active::I2C_SELFTEST_SLAVE,
            0x42,
        )
        .expect("I2C1 slave");
        BUS_I2C_SLAVE.init(slave);
        board::expansion_i2c().expect("I2C0 master bus");
        board::expansion_spi().expect("SPI0 bus");
        task::spawn_on(0, "bus-master", bus_master, Priority::Normal(1), 4096)
            .expect("SPI and I2C master task");
    }
    #[cfg(feature = "watchdog-reset")]
    task::spawn("watchdog", watchdog_reset_test, Priority::Normal(0), 2048).expect("watchdog task");
    #[cfg(feature = "expected-hardfault")]
    task::spawn("fault", inject_hardfault, Priority::Normal(1), 2048).expect("fault task");
    #[cfg(all(
        not(feature = "expected-hardfault"),
        not(feature = "watchdog-reset"),
        not(feature = "reset-recovery-smoke"),
        not(feature = "diagnostics-smoke"),
        not(feature = "dma-smoke"),
        not(feature = "mutex-smoke"),
        not(feature = "race-smoke"),
        not(feature = "pwm-smoke"),
        not(feature = "adc-entropy-smoke"),
        not(feature = "bus-smoke"),
        not(feature = "clock-smoke"),
        not(feature = "pio-smoke"),
        not(feature = "flash-smoke")
    ))]
    {
        task::spawn_on(0, "peer", peer, Priority::Normal(2), 2048).expect("peer task");
        task::spawn_on(1, "core1", core1_peer, Priority::Normal(2), 2048).expect("core-1 task");
        #[cfg(not(feature = "minimal"))]
        {
            let medium = task::spawn_on(0, "pi-medium", pi_medium, Priority::Normal(1), 2048)
                .expect("priority-inheritance medium task");
            let high = task::spawn_on(0, "pi-high", pi_high, Priority::Critical(0), 2048)
                .expect("priority-inheritance high task");
            PI_MEDIUM_ID.store(medium.0 + 1, Ordering::Release);
            PI_HIGH_ID.store(high.0 + 1, Ordering::Release);
        }
        task::spawn_on(0, "tests", tests, Priority::Normal(2), 4096).expect("test task");
    }
}

#[cfg(feature = "bus-smoke")]
fn bus_i2c_slave() {
    const CYCLES: u32 = 1_001;
    BUS_SLAVE_STAGE.store(1, Ordering::Release);
    // SPI setup and loopback run first. Do not spend the slave's bounded
    // transaction timeout waiting for unrelated work on the other core.
    if !task::wait_until(|| BUS_SPI_BYTES.load(Ordering::Acquire) == 4_096, 5_000) {
        fail(41);
    }
    BUS_SLAVE_READY.store(1, Ordering::Release);
    BUS_SLAVE_STAGE.store(2, Ordering::Release);
    for cycle in 0..CYCLES {
        if cycle == 1_000 {
            // The master deliberately stalls SCL for a full timeout. This
            // responder is not serving a transaction during fault injection.
            if !task::wait_until(|| BUS_FAULTS_DONE.load(Ordering::Acquire) == 1, 1_000) {
                fail(41);
            }
            BUS_SLAVE_STAGE.store(3, Ordering::Release);
        }
        let mut request = [0u8; 4];
        let response = [cycle as u8, (cycle >> 8) as u8, 0x5a, 0xa5];
        let Ok(count) = BUS_I2C_SLAVE
            .get()
            .expect("I2C slave")
            .serve_once(&mut request, &response)
        else {
            fail(41);
        };
        let expected = [cycle as u8, (cycle >> 8) as u8, 0x33, 0xcc];
        if count != expected.len() || request != expected {
            fail(42);
        }
    }
    BUS_SLAVE_STAGE.store(4, Ordering::Release);
    loop { task::sleep_ms(1_000); }
}

#[cfg(feature = "bus-smoke")]
fn bus_master() {
    use hal::bus::{Bus as _, BusError, Op};

    BUS_MASTER_STAGE.store(1, Ordering::Release);
    let Ok(spi) = board::expansion_spi() else {
        fail(38);
    };
    if !core::ptr::eq(spi, board::expansion_spi().expect("cached SPI")) { fail(38); }
    if rp2040_spi::Rp2040Spi::open(&kernel::board::active::SPI_SELFTEST).is_ok() {
        fail(38);
    }
    // Disable the shifter while leaving queued FIFO data. The physical
    // driver must time out, reset the FIFOs, and restore the configuration.
    let cr1 = (kernel::board::active::SPI_SELFTEST.ctrl.base() + 4) as *mut u32;
    unsafe { cr1.write_volatile((cr1.read_volatile() | 1) & !2); }
    let start = soc_rp2040::timer_us();
    let timeout = spi.transfer(&mut [Op::exchange(&[0xee; 16], &mut [0u8; 16])]);
    let elapsed = soc_rp2040::timer_us().wrapping_sub(start);
    if timeout != Err(BusError::Timeout) || !(50_000..100_000).contains(&elapsed) { fail(46); }
    BUS_SPI_TIMEOUT_US.store(elapsed, Ordering::Release);
    let mut checksum = 0u32;
    for round in 0..64u32 {
        let mut tx = [0u8; 64];
        let mut rx = [0u8; 64];
        for (index, byte) in tx.iter_mut().enumerate() {
            *byte = (round as u8).wrapping_mul(17).wrapping_add(index as u8);
        }
        if spi.transfer(&mut [Op::exchange(&tx, &mut rx)]).is_err() || rx != tx {
            fail(39);
        }
        for &byte in &rx { checksum = checksum.wrapping_add(u32::from(byte)); }
        BUS_SPI_BYTES.fetch_add(64, Ordering::Relaxed);
    }
    BUS_SPI_CHECKSUM.store(checksum, Ordering::Release);
    BUS_MASTER_STAGE.store(2, Ordering::Release);
    if BUS_SPI_BYTES.load(Ordering::Acquire) != 4_096 || checksum == 0 { fail(40); }

    task::spawn_on(1, "bus-slave", bus_i2c_slave, Priority::Normal(1), 3072)
        .expect("I2C slave task");
    if !task::wait_until(|| BUS_SLAVE_READY.load(Ordering::Acquire) == 1, 1_000) {
        fail(41);
    }
    let controller = board::expansion_i2c().expect("I2C master bus");
    if !core::ptr::eq(controller, board::expansion_i2c().expect("cached I2C")) { fail(38); }
    if rp2040_i2c::Rp2040I2c::open(&kernel::board::active::I2C_SELFTEST_MASTER).is_ok() {
        fail(38);
    }
    BUS_MASTER_STAGE.store(3, Ordering::Release);
    let device = controller.device(0x42);
    for cycle in 0..1_000u32 {
        let request = [cycle as u8, (cycle >> 8) as u8, 0x33, 0xcc];
        let expected = [cycle as u8, (cycle >> 8) as u8, 0x5a, 0xa5];
        let mut response = [0u8; 4];
        if device.transfer(&mut [Op::exchange(&request, &mut response)]).is_err()
            || response != expected
        {
            fail(43);
        }
        BUS_I2C_TRANSACTIONS.fetch_add(1, Ordering::Relaxed);
        BUS_I2C_BYTES.fetch_add(8, Ordering::Relaxed);
    }
    let absent = controller.device(0x43);
    // Force the receiver's SCL pad low through the documented GPIO overrides.
    // Always restore it before checking the result, even if the driver fails.
    let scl = kernel::board::active::I2C_SELFTEST_SLAVE.cfg.scl;
    let control = (soc_rp2040::IO_BANK0_BASE + 4 + u32::from(scl) * 8) as *mut u32;
    let saved = unsafe { control.read_volatile() };
    unsafe { control.write_volatile((saved & !0x3300) | (2 << 8) | (3 << 12)); }
    let start = soc_rp2040::timer_us();
    let timeout = absent.transfer(&mut [Op::write(&[0x11])]);
    let elapsed = soc_rp2040::timer_us().wrapping_sub(start);
    unsafe { control.write_volatile(saved); }
    if timeout != Err(BusError::Timeout) || !(50_000..100_000).contains(&elapsed) { fail(47); }
    BUS_I2C_TIMEOUT_US.store(elapsed, Ordering::Release);
    if absent.transfer(&mut [Op::write(&[0x11])]) != Err(BusError::DeviceNotResponding) {
        fail(44);
    }
    BUS_FAULTS_DONE.store(1, Ordering::Release);
    if !task::wait_until(|| BUS_SLAVE_STAGE.load(Ordering::Acquire) == 3, 1_000) {
        fail(41);
    }
    let request = [0xe8, 0x03, 0x33, 0xcc];
    let mut response = [0u8; 4];
    if device.transfer(&mut [Op::exchange(&request, &mut response)]).is_err()
        || response != [0xe8, 0x03, 0x5a, 0xa5]
    {
        fail(45);
    }
    BUS_I2C_TRANSACTIONS.fetch_add(1, Ordering::Relaxed);
    BUS_I2C_BYTES.fetch_add(8, Ordering::Relaxed);
    BUS_I2C_NACK_RECOVERED.store(1, Ordering::Release);
    if !task::wait_until(|| BUS_SLAVE_STAGE.load(Ordering::Acquire) == 4, 1_000) {
        fail(42);
    }
    BUS_MASTER_STAGE.store(4, Ordering::Release);
    api::log_info!(
        "[FLINT] ARM BUS PASS spi_bytes=4096 i2c_transactions=1001 i2c_bytes=8008 nack_recovered=1"
    );
    task::sleep_ms(100);
    unsafe { soc_rp2040::test_status::pass_live() }
}

#[cfg(feature = "adc-entropy-smoke")]
fn adc_entropy_smoke() {
    const ADC_SAMPLES: u32 = 1_024;
    const ENTROPY_SEEDS: u32 = 64;

    let Ok(mut adc) = rp2040_adc::Rp2040Adc::open() else {
        fail(32);
    };
    if rp2040_adc::Rp2040Adc::open().is_ok() {
        fail(32);
    }
    let mut minimum = u16::MAX;
    let mut maximum = 0u16;
    let mut total = 0u64;
    for _ in 0..ADC_SAMPLES {
        let Ok(raw) = adc.read(rp2040_adc::Channel::Temperature) else {
            fail(33);
        };
        minimum = minimum.min(raw);
        maximum = maximum.max(raw);
        total += u64::from(raw);
        ADC_SAMPLE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    let average = (total / u64::from(ADC_SAMPLES)) as u16;
    let temperature = rp2040_adc::temperature_milli_celsius(average);
    ADC_MIN_RAW.store(u32::from(minimum), Ordering::Release);
    ADC_MAX_RAW.store(u32::from(maximum), Ordering::Release);
    ADC_AVG_RAW.store(u32::from(average), Ordering::Release);
    ADC_TEMP_MILLI_C.store(temperature as u32, Ordering::Release);
    if minimum == 0
        || maximum >= 4_095
        || minimum >= maximum
        || !(-40_000..=125_000).contains(&temperature)
    {
        fail(34);
    }

    let mut raw_bits = 0u32;
    let mut raw_ones = 0u32;
    let mut transitions = 0u32;
    let mut checksum = 0u32;
    let mut previous = [0u64; 2];
    for index in 0..ENTROPY_SEEDS {
        let Ok(seed) = rp2040_entropy::sample_seed() else {
            fail(35);
        };
        raw_bits += u32::from(seed.health.bits);
        raw_ones += u32::from(seed.health.ones);
        transitions += u32::from(seed.health.transitions);
        checksum ^= seed.words[0] as u32
            ^ (seed.words[0] >> 32) as u32
            ^ seed.words[1] as u32
            ^ (seed.words[1] >> 32) as u32;
        if index != 0 && seed.words == previous {
            fail(36);
        }
        previous = seed.words;
    }
    ENTROPY_RAW_BITS.store(raw_bits, Ordering::Release);
    ENTROPY_RAW_ONES.store(raw_ones, Ordering::Release);
    ENTROPY_TRANSITIONS.store(transitions, Ordering::Release);
    ENTROPY_CHECKSUM.store(checksum, Ordering::Release);
    if raw_bits != ENTROPY_SEEDS * 64
        || !(raw_bits / 4..=raw_bits * 3 / 4).contains(&raw_ones)
        || !(raw_bits / 5..=raw_bits * 4 / 5).contains(&transitions)
        || checksum == 0
    {
        fail(37);
    }
    api::log_info!(
        "[FLINT] ARM ADC+ENTROPY PASS adc_samples={} raw={}..{} temp_mc={} entropy_bits={} ones={} transitions={}",
        ADC_SAMPLES,
        minimum,
        maximum,
        temperature,
        raw_bits,
        raw_ones,
        transitions
    );
    task::sleep_ms(100);
    unsafe { soc_rp2040::test_status::pass_live() }
}

#[cfg(feature = "mutex-smoke")]
fn spawn_pi_stress_low(core: u8, low: fn()) {
    task::spawn_on(core, "pi-low", low, Priority::Normal(2), 3072)
        .expect("priority-inheritance low task");
}

#[cfg(feature = "mutex-smoke")]
fn pi_stress_fail(code: u8) -> ! {
    MUTEX_SOAK_ERRORS.fetch_add(1, Ordering::Relaxed);
    fail(code)
}

#[cfg(feature = "mutex-smoke")]
fn pi_tasks_parked(state: &'static PiStress) -> bool {
    let medium = state.medium_id.load(Ordering::Acquire);
    let high = state.high_id.load(Ordering::Acquire);
    kernel::scheduler::with(|sched| {
        [medium, high].into_iter().all(|id| {
            sched.tasks[id as usize]
                .as_ref()
                .is_some_and(|task| task.state == kernel::scheduler::TaskState::BlockedSleep)
        })
    })
}

#[cfg(feature = "mutex-smoke")]
fn run_pi_low(state: &'static PiStress, expected_core: u8, medium: fn(), high: fn()) {
    const CYCLES: u32 = 1_000;
    if kernel::smp::current_core().0 != expected_core {
        pi_stress_fail(28);
    }
    let low_id = task::current_id().0;
    state.low_id.store(low_id, Ordering::Release);

    for cycle in 1..=CYCLES {
        if cycle > 1 && !task::wait_until(|| pi_tasks_parked(state), 100) {
            pi_stress_fail(29);
        }
        let mut guard = lock(&state.mutex);
        if *guard != cycle - 1 {
            pi_stress_fail(28);
        }
        state.cycle.store(cycle, Ordering::Release);
        if cycle == 1 {
            let high_id =
                task::spawn_on(expected_core, "pi-high", high, Priority::Critical(0), 2048)
                    .expect("priority-inheritance high task");
            state.high_id.store(high_id.0, Ordering::Release);
            let medium_id = task::spawn_on(
                expected_core,
                "pi-medium",
                medium,
                Priority::Normal(1),
                2048,
            )
            .expect("priority-inheritance medium task");
            state.medium_id.store(medium_id.0, Ordering::Release);
        } else {
            let medium_id = state.medium_id.load(Ordering::Acquire);
            let high_id = state.high_id.load(Ordering::Acquire);
            kernel::scheduler::with(|sched| {
                sched.unblock(medium_id);
                sched.unblock(high_id);
            });
        }

        // A yield is advisory and may return before a newly spawned task has
        // taken its first exception. Blocking for one tick makes the handoff
        // part of the test: high must queue on the mutex, boost this sleeping
        // owner, and let it preempt medium when the timer wakes it.
        task::sleep_ms(1);
        let high_id = state.high_id.load(Ordering::Acquire);
        let boost_wait_start = timer::now_ms();
        loop {
            let high_is_blocked = kernel::scheduler::with(|sched| {
                sched.tasks[high_id as usize]
                    .as_ref()
                    .is_some_and(|task| task.state == kernel::scheduler::TaskState::BlockedMutex)
            });
            if high_is_blocked {
                break;
            }
            if timer::now_ms().wrapping_sub(boost_wait_start) >= 100 {
                pi_stress_fail(29);
            }
            task::sleep_ms(5);
        }
        let boosted = kernel::scheduler::with(|sched| {
            sched.tasks[low_id as usize]
                .as_ref()
                .is_some_and(|task| task.priority == Priority::Critical(0).numeric())
        });
        if !boosted {
            pi_stress_fail(30);
        }

        *guard = cycle;
        drop(guard);
        if !task::wait_until(
            || {
                state.high_finished.load(Ordering::Acquire) == cycle
                    && state.medium_finished.load(Ordering::Acquire) == cycle
            },
            100,
        ) {
            pi_stress_fail(29);
        }
        let restored = kernel::scheduler::with(|sched| {
            sched.tasks[low_id as usize]
                .as_ref()
                .is_some_and(|task| task.priority == Priority::Normal(2).numeric())
        });
        if !restored {
            pi_stress_fail(30);
        }
        MUTEX_SOAK_PROGRESS.fetch_add(1, Ordering::Relaxed);
    }

    let done = MUTEX_SOAK_CORES_DONE.fetch_add(1, Ordering::AcqRel) + 1;
    if done == 2 {
        if MUTEX_SOAK_PROGRESS.load(Ordering::Acquire) != CYCLES * 2
            || MUTEX_SOAK_ERRORS.load(Ordering::Acquire) != 0
        {
            pi_stress_fail(31);
        }
        api::log_info!(
            "[FLINT] ARM MUTEX PI PASS cores=2 cycles_per_core=1000 total=2000 errors=0"
        );
        task::sleep_ms(100);
        unsafe { soc_rp2040::test_status::pass_live() }
    }
    task::exit()
}

#[cfg(feature = "mutex-smoke")]
fn run_pi_medium(state: &'static PiStress, expected_core: u8) {
    while state.medium_id.load(Ordering::Acquire) == u32::MAX
        || state.high_id.load(Ordering::Acquire) == u32::MAX
    {
        task::sleep_ms(10);
    }
    loop {
        if kernel::smp::current_core().0 != expected_core {
            pi_stress_fail(28);
        }
        let cycle = state.cycle.load(Ordering::Acquire);
        if cycle == 0 || state.medium_seen.swap(cycle, Ordering::AcqRel) >= cycle {
            pi_stress_fail(31);
        }
        state.medium_ready.store(cycle, Ordering::Release);
        while state.high_finished.load(Ordering::Acquire) != cycle {
            core::hint::spin_loop();
        }
        state.medium_finished.store(cycle, Ordering::Release);
        task::sleep_ms(u32::MAX);
    }
}

#[cfg(feature = "mutex-smoke")]
fn run_pi_high(state: &'static PiStress, expected_core: u8) {
    while state.medium_id.load(Ordering::Acquire) == u32::MAX
        || state.high_id.load(Ordering::Acquire) == u32::MAX
    {
        task::sleep_ms(10);
    }
    loop {
        if kernel::smp::current_core().0 != expected_core {
            pi_stress_fail(28);
        }
        let cycle = state.cycle.load(Ordering::Acquire);
        if cycle == 0 || state.high_seen.swap(cycle, Ordering::AcqRel) >= cycle {
            pi_stress_fail(31);
        }
        let wait_start = timer::now_ms();
        while state.medium_ready.load(Ordering::Acquire) != cycle {
            if timer::now_ms().wrapping_sub(wait_start) >= 100 {
                pi_stress_fail(29);
            }
            // A one-tick sleep can expire in SysTick before its pending
            // PendSV runs, immediately selecting this critical task again.
            // Five ticks leave a real interval for medium to be dispatched.
            task::sleep_ms(5);
        }
        let guard = lock(&state.mutex);
        if *guard != cycle {
            pi_stress_fail(31);
        }
        let low = state.low_id.load(Ordering::Acquire);
        let low_restored = kernel::scheduler::with(|sched| {
            sched.tasks[low as usize]
                .as_ref()
                .is_some_and(|task| task.priority == Priority::Normal(2).numeric())
        });
        if !low_restored {
            pi_stress_fail(30);
        }
        drop(guard);
        state.high_finished.store(cycle, Ordering::Release);
        task::sleep_ms(u32::MAX);
    }
}

#[cfg(feature = "mutex-smoke")]
fn pi_low_core0() {
    run_pi_low(&PI_STRESS_CORE0, 0, pi_medium_core0, pi_high_core0);
}

#[cfg(feature = "mutex-smoke")]
fn pi_medium_core0() {
    run_pi_medium(&PI_STRESS_CORE0, 0);
}

#[cfg(feature = "mutex-smoke")]
fn pi_high_core0() {
    run_pi_high(&PI_STRESS_CORE0, 0);
}

#[cfg(feature = "mutex-smoke")]
fn pi_low_core1() {
    run_pi_low(&PI_STRESS_CORE1, 1, pi_medium_core1, pi_high_core1);
}

#[cfg(feature = "mutex-smoke")]
fn pi_medium_core1() {
    run_pi_medium(&PI_STRESS_CORE1, 1);
}

#[cfg(feature = "mutex-smoke")]
fn pi_high_core1() {
    run_pi_high(&PI_STRESS_CORE1, 1);
}

#[cfg(feature = "race-smoke")]
fn race_fail(code: u8) -> ! {
    RACE_ERRORS.fetch_add(1, Ordering::Relaxed);
    fail(code)
}

#[cfg(feature = "race-smoke")]
fn race_gpio_isr() {
    let Some(input) = RACE_INPUT.get() else {
        RACE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let Ok(events) = input.take_edge_events() else {
        RACE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let edge_count = u32::from(events.falling) + u32::from(events.rising);
    if edge_count != 1 {
        RACE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let sequence = RACE_ISR_HANDLED.fetch_add(1, Ordering::AcqRel) + 1;
    if RACE_QUEUE.send_isr(sequence).is_ok() {
        RACE_ISR_SENT.fetch_add(1, Ordering::Relaxed);
    } else {
        RACE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "race-smoke")]
fn race_consumer() {
    if kernel::smp::current_core().0 != 0 {
        race_fail(28);
    }
    for expected in 1..=10_000u32 {
        let Ok(value) = api::queue::recv(&RACE_QUEUE, 100) else {
            race_fail(29);
        };
        if value != expected {
            race_fail(30);
        }
        if RACE_ACTIVE_EDGE.load(Ordering::Acquire) == expected {
            RACE_PREEMPTIONS.fetch_add(1, Ordering::Relaxed);
        }
        RACE_TASK_RECEIVED.store(expected, Ordering::Release);
    }
    task::sleep_ms(u32::MAX);
    race_fail(31)
}

#[cfg(feature = "race-smoke")]
fn race_producer() {
    const EDGES: u32 = 10_000;
    if kernel::smp::current_core().0 != 0 {
        race_fail(28);
    }
    let Ok(output) = rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_OUT) else {
        race_fail(28);
    };
    let Ok(input) = rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_IN) else {
        race_fail(28);
    };
    if output.set_mode(rp2040_gpio::PinMode::Output).is_err()
        || output.write(rp2040_gpio::PinLevel::Low).is_err()
        || input.set_mode(rp2040_gpio::PinMode::InputPullDown).is_err()
    {
        race_fail(28);
    }
    RACE_INPUT.init(input);
    if unsafe {
        kernel::interrupt::connect_at(
            soc_rp2040::IRQ_IO_BANK0,
            soc_rp2040::IRQ_IO_BANK0,
            race_gpio_isr,
        )
    }
    .is_err()
        || RACE_INPUT
            .get()
            .expect("race loopback input")
            .enable_edge_interrupt(rp2040_gpio::Edge::Both)
            .is_err()
    {
        race_fail(28);
    }

    task::spawn_on(
        0,
        "race-consumer",
        race_consumer,
        Priority::Critical(0),
        3072,
    )
    .expect("physical interrupt race consumer");
    task::sleep_ms(5);

    for expected in 1..=EDGES {
        let level = if expected & 1 == 1 {
            rp2040_gpio::PinLevel::High
        } else {
            rp2040_gpio::PinLevel::Low
        };
        RACE_ACTIVE_EDGE.store(expected, Ordering::Release);
        if expected % 4 == 0 {
            let before = RACE_ISR_HANDLED.load(Ordering::Acquire);
            let outer = unsafe { kernel::arch::cs_enter() };
            let inner = unsafe { kernel::arch::cs_enter() };
            if output.write(level).is_err() {
                race_fail(28);
            }
            let masked_start = hardware_timer_us();
            while hardware_timer_us().wrapping_sub(masked_start) < 20 {
                core::hint::spin_loop();
            }
            if RACE_ISR_HANDLED.load(Ordering::Acquire) != before {
                race_fail(30);
            }
            unsafe { kernel::arch::cs_exit(inner) };
            if RACE_ISR_HANDLED.load(Ordering::Acquire) != before {
                race_fail(30);
            }
            unsafe { kernel::arch::cs_exit(outer) };
            RACE_NESTED_MASKED.fetch_add(1, Ordering::Relaxed);
        } else if output.write(level).is_err() {
            race_fail(28);
        }

        if !task::wait_until(
            || RACE_TASK_RECEIVED.load(Ordering::Acquire) == expected,
            100,
        ) {
            race_fail(29);
        }
        RACE_ACTIVE_EDGE.store(0, Ordering::Release);
        if RACE_ISR_HANDLED.load(Ordering::Acquire) != expected
            || RACE_ISR_SENT.load(Ordering::Acquire) != expected
            || RACE_ERRORS.load(Ordering::Acquire) != 0
        {
            race_fail(30);
        }
    }

    if RACE_INPUT
        .get()
        .expect("race loopback input")
        .disable_edge_interrupt()
        .is_err()
        || RACE_ISR_HANDLED.load(Ordering::Acquire) != EDGES
        || RACE_ISR_SENT.load(Ordering::Acquire) != EDGES
        || RACE_TASK_RECEIVED.load(Ordering::Acquire) != EDGES
        || RACE_NESTED_MASKED.load(Ordering::Acquire) != EDGES / 4
        || RACE_PREEMPTIONS.load(Ordering::Acquire) == 0
        || RACE_ERRORS.load(Ordering::Acquire) != 0
    {
        race_fail(31);
    }
    api::log_info!("[FLINT] ARM ISR RACE PASS handled=10000 sent=10000 received=10000 nested=2500");
    task::sleep_ms(100);
    unsafe { soc_rp2040::test_status::pass_live() }
}

#[cfg(feature = "pwm-smoke")]
fn pwm_gpio_isr() {
    let Some(input) = PWM_INPUT.get() else {
        PWM_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if PWM_EDGE_COUNT.load(Ordering::Acquire) >= 2_000 {
        let _ = input.disable_edge_interrupt();
        return;
    }
    let Ok(events) = input.take_edge_events() else {
        PWM_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let now = hardware_timer_us();
    let count = u32::from(events.rising) + u32::from(events.falling);
    if count != 1 {
        PWM_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if events.rising {
        let rises = PWM_RISES.fetch_add(1, Ordering::Relaxed);
        if rises == 0 {
            PWM_FIRST_RISE.store(now, Ordering::Relaxed);
        }
        PWM_LAST_RISE.store(now, Ordering::Relaxed);
    }
    if events.falling {
        PWM_FALLS.fetch_add(1, Ordering::Relaxed);
    }
    let total = PWM_EDGE_COUNT.fetch_add(1, Ordering::AcqRel) + 1;
    if total == 2_000 && input.disable_edge_interrupt().is_err() {
        PWM_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "pwm-smoke")]
fn wait_pwm_level(input: &rp2040_gpio::Rp2040Pin, level: rp2040_gpio::PinLevel) -> u32 {
    let start = hardware_timer_us();
    loop {
        if input.read() == Ok(level) {
            return hardware_timer_us();
        }
        if hardware_timer_us().wrapping_sub(start) >= 2_000 {
            fail(29);
        }
        core::hint::spin_loop();
    }
}

#[cfg(feature = "pwm-smoke")]
fn measure_pwm_level(input: &rp2040_gpio::Rp2040Pin) -> (u32, u32) {
    let mut period_total = 0u32;
    let mut high_total = 0u32;
    for _ in 0..100 {
        let _ = wait_pwm_level(input, rp2040_gpio::PinLevel::Low);
        let rise = wait_pwm_level(input, rp2040_gpio::PinLevel::High);
        let fall = wait_pwm_level(input, rp2040_gpio::PinLevel::Low);
        let next_rise = wait_pwm_level(input, rp2040_gpio::PinLevel::High);
        high_total = high_total.wrapping_add(fall.wrapping_sub(rise));
        period_total = period_total.wrapping_add(next_rise.wrapping_sub(rise));
    }
    (period_total / 100, high_total / 100)
}

#[cfg(feature = "pwm-smoke")]
fn pwm_smoke() {
    let Ok(pwm) = rp2040_pwm::Rp2040Pwm::open(&kernel::board::active::PWM_LOOPBACK_OUT) else {
        fail(28);
    };
    if rp2040_pwm::Rp2040Pwm::open(&kernel::board::active::PWM_LOOPBACK_OUT).is_ok() {
        fail(28);
    }
    let Ok(input) = rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_IN) else {
        fail(28);
    };
    if input.set_mode(rp2040_gpio::PinMode::InputPullDown).is_err() {
        fail(28);
    }
    PWM_INPUT.init(input);
    if unsafe {
        kernel::interrupt::connect_at(
            soc_rp2040::IRQ_IO_BANK0,
            soc_rp2040::IRQ_IO_BANK0,
            pwm_gpio_isr,
        )
    }
    .is_err()
        || PWM_INPUT
            .get()
            .expect("PWM loopback input")
            .enable_edge_interrupt(rp2040_gpio::Edge::Both)
            .is_err()
        || pwm.start(1_000, 500).is_err()
    {
        fail(28);
    }

    if !task::wait_until(|| PWM_EDGE_COUNT.load(Ordering::Acquire) >= 2_000, 3_000) {
        fail(29);
    }
    if PWM_INPUT
        .get()
        .expect("PWM loopback input")
        .disable_edge_interrupt()
        .is_err()
    {
        fail(28);
    }
    let rises = PWM_RISES.load(Ordering::Acquire);
    let falls = PWM_FALLS.load(Ordering::Acquire);
    if rises != 1_000 || falls != 1_000 || PWM_ERRORS.load(Ordering::Acquire) != 0 {
        fail(30);
    }
    let (period, high) = measure_pwm_level(PWM_INPUT.get().expect("PWM loopback input"));
    pwm.stop();
    PWM_PERIOD_US.store(period, Ordering::Release);
    PWM_HIGH_US.store(high, Ordering::Release);
    if !(950..=1_050).contains(&period) || !(400..=600).contains(&high) {
        fail(31);
    }
    api::log_info!(
        "[FLINT] ARM PWM PASS edges=2000 period_us={} high_us={}",
        period,
        high
    );
    task::sleep_ms(100);
    unsafe { soc_rp2040::test_status::pass_live() }
}

#[cfg(feature = "dma-smoke")]
fn dma_irq() {
    if let Some(id) = rp2040_uart::Rp2040Uart::take_pending_dma() {
        kernel::dma_broker::signal_complete(id);
    }
}

#[cfg(feature = "dma-smoke")]
fn dma_test() {
    const BYTES: usize = 512;
    const ROUNDS: u32 = 100;

    if unsafe {
        kernel::interrupt::connect_at(soc_rp2040::IRQ_DMA_0, soc_rp2040::IRQ_DMA_0, dma_irq)
    }
    .is_err()
    {
        fail(25);
    }

    let Ok(uart) = rp2040_uart::Rp2040Uart::open(&kernel::board::active::SELFTEST_UART) else {
        fail(25);
    };
    let Ok(tx_handle) = api::dma::alloc(BYTES as u32) else {
        fail(25);
    };
    let Ok(rx_handle) = api::dma::alloc(BYTES as u32) else {
        fail(25);
    };
    let tx = unsafe { core::slice::from_raw_parts_mut(tx_handle.addr() as *mut u8, BYTES) };
    let rx = unsafe { core::slice::from_raw_parts_mut(rx_handle.addr() as *mut u8, BYTES) };

    uart.set_loopback(false);
    tx[..16].fill(0xa5);
    rx[..16].fill(0);
    let Ok(timeout_transfer) = uart.exchange_dma(&tx_handle, &rx_handle, 16) else {
        fail(26);
    };
    if !matches!(
        timeout_transfer.await_done(),
        Err(hal::Error::Dma(hal::DmaError::Timeout))
    ) {
        fail(26);
    }

    uart.set_loopback(true);
    for round in 0..ROUNDS {
        for (index, byte) in tx.iter_mut().enumerate() {
            *byte = (index as u8)
                .wrapping_mul(37)
                .wrapping_add(round as u8)
                .wrapping_add(11);
        }
        rx.fill(0);
        let Ok(transfer) = uart.exchange_dma(&tx_handle, &rx_handle, BYTES) else {
            fail(27);
        };
        if transfer.await_done().is_err() || rx != tx {
            fail(28);
        }
    }
    api::log_info!(
        "[FLINT] ARM DMA PASS rounds={} bytes={} timeout=ok",
        ROUNDS,
        BYTES
    );
    task::sleep_ms(250);
    unsafe { soc_rp2040::test_status::pass_live() }
}

#[cfg(feature = "diagnostics-smoke")]
fn diagnostics_peer() {
    for _ in 0..10_000 {
        DIAGNOSTIC_COUNTER.increment();
    }
    DIAGNOSTIC_GAUGE.set(143);
    DIAGNOSTIC_PEER_DONE.store(1, Ordering::Release);
    loop {
        task::sleep_ms(1_000);
    }
}

#[cfg(feature = "diagnostics-smoke")]
fn diagnostics_test() {
    for _ in 0..10_000 {
        DIAGNOSTIC_COUNTER.increment();
    }
    let deadline = timer::now_ms().wrapping_add(2_000);
    while DIAGNOSTIC_PEER_DONE.load(Ordering::Acquire) == 0 {
        if timer::now_ms().wrapping_sub(deadline) < u64::MAX / 2 {
            panic!("ARM diagnostics peer timed out");
        }
        task::sleep_ms(1);
    }
    if DIAGNOSTIC_COUNTER.read() != 20_000 || DIAGNOSTIC_GAUGE.read() != 143 {
        panic!("ARM diagnostics metrics mismatch");
    }
    FLINT_ARM_DIAGNOSTIC_STAGE.store(1, Ordering::Release);
    api::log_info!(
        "[FLINT] ARM DIAGNOSTICS counter={} gauge={}",
        DIAGNOSTIC_COUNTER.read(),
        DIAGNOSTIC_GAUGE.read()
    );
    FLINT_ARM_DIAGNOSTIC_STAGE.store(2, Ordering::Release);
    task::sleep_ms(250);
    FLINT_ARM_DIAGNOSTIC_STAGE.store(3, Ordering::Release);
    panic!("ARM diagnostics deliberate panic");
}

#[cfg(feature = "diagnostics-smoke")]
fn diagnostics_reported() {
    api::log_info!("[FLINT] ARM DIAGNOSTICS RECOVERED");
    task::sleep_ms(250);
    unsafe { soc_rp2040::test_status::pass_live() }
}

#[cfg(feature = "watchdog-reset")]
fn watchdog_reset_test() {
    task::sleep_ms(500);
    if unsafe { soc_rp2040::watchdog::flint_watchdog_caused_reset() } {
        if unsafe { soc_rp2040::watchdog::reset_reason() } != 1 {
            fail(19);
        }
        unsafe { soc_rp2040::watchdog::clear_flint_watchdog_marker() };
        unsafe { soc_rp2040::test_status::pass_to_bootsel() }
    }
    unsafe { kernel::watchdog::arm() };
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(feature = "expected-hardfault"))]
fn fail(code: u8) -> ! {
    soc_rp2040::test_status::fail(code)
}

#[cfg(not(feature = "expected-hardfault"))]
fn peer() {
    loop {
        if CORE0_ACTIVE.swap(1, Ordering::AcqRel) != 0 {
            DUPLICATE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        if kernel::smp::current_core().0 != 0 {
            WRONG_CORE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        SMP_LOCK.with(|count| *count = count.wrapping_add(1));
        PEER_RUNS.fetch_add(1, Ordering::Relaxed);
        CORE0_ACTIVE.store(0, Ordering::Release);
        task::sleep_ms(100);
    }
}

#[cfg(not(feature = "expected-hardfault"))]
fn core1_peer() {
    #[cfg(not(feature = "minimal"))]
    loop {
        if CORE1_ACTIVE.swap(1, Ordering::AcqRel) != 0 {
            DUPLICATE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        if kernel::smp::current_core().0 != 1 {
            WRONG_CORE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        SMP_LOCK.with(|count| *count = count.wrapping_add(1));
        CORE1_RUNS.fetch_add(1, Ordering::Relaxed);
        #[cfg(not(feature = "minimal"))]
        {
            let request = REMOTE_REQUEST.load(Ordering::Acquire);
            if request != 0 {
                kernel::scheduler::with(|sched| {
                    let id = request - 1;
                    let blocked = sched.tasks[id as usize].as_ref().is_some_and(|task| {
                        task.state == kernel::scheduler::TaskState::BlockedSleep
                    });
                    if blocked {
                        REMOTE_REQUEST.store(0, Ordering::Release);
                        sched.unblock(id);
                    }
                });
            }
        }
        CORE1_ACTIVE.store(0, Ordering::Release);
        let poll_start = hardware_timer_us();
        while hardware_timer_us().wrapping_sub(poll_start) < 1_000 {
            core::hint::spin_loop();
        }
        task::yield_now();
    }
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
fn pi_medium() {
    PI_MEDIUM_PARKED.store(1, Ordering::Release);
    task::sleep_ms(u32::MAX);
    PI_MEDIUM_READY.store(1, Ordering::Release);
    while PI_PHASE.load(Ordering::Acquire) == 2 {
        core::hint::spin_loop();
    }
    if PI_PHASE.load(Ordering::Acquire) == 3 {
        PI_MEDIUM_RAN_DURING_INVERSION.store(1, Ordering::Release);
    }
    PI_MEDIUM_FINISHED.store(1, Ordering::Release);
    loop {
        task::sleep_ms(1_000);
    }
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
fn pi_high() {
    PI_HIGH_PARKED.store(1, Ordering::Release);
    task::sleep_ms(u32::MAX);
    while PI_MEDIUM_READY.load(Ordering::Acquire) == 0 {
        task::sleep_ms(10);
    }
    PI_PHASE.store(3, Ordering::Release);
    let mut guard = lock(&PI_MUTEX);
    if PI_BOOST_SEEN.load(Ordering::Acquire) == 1 && *guard == 1 {
        let owner = PI_OWNER_ID.load(Ordering::Acquire).wrapping_sub(1);
        let restored = kernel::scheduler::with(|sched| {
            sched.tasks[owner as usize]
                .as_ref()
                .is_some_and(|task| task.priority == Priority::Normal(2).numeric())
        });
        if restored {
            PI_RESTORE_SEEN.store(1, Ordering::Release);
        }
        *guard = guard.wrapping_add(1);
        drop(guard);
        PI_PHASE.store(4, Ordering::Release);
    } else {
        drop(guard);
        PI_PHASE.store(5, Ordering::Release);
    }
    loop {
        task::sleep_ms(1_000);
    }
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
fn isr_queue_producer() {
    if ISR_PRODUCING
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let value = ISR_NEXT.fetch_add(1, Ordering::Relaxed);
    if ISR_QUEUE.send_isr(value).is_ok() {
        ISR_SENT.fetch_add(1, Ordering::Relaxed);
    }
    ISR_PRODUCING.store(0, Ordering::Release);
}

// Read the monotonic clock through the kernel, naming no chip: on the RP2040
// this is the same free-running timer as before (`now_us`'s low 32 bits are the
// hardware counter), so the wrap-based interval checks below are unchanged, but
// the self-test no longer reaches into `soc_rp2040` for it.
#[cfg(all(
    target_arch = "arm",
    not(feature = "expected-hardfault"),
    not(feature = "minimal")
))]
fn hardware_timer_us() -> u32 {
    kernel::clock::now_us() as u32
}

#[cfg(all(
    not(target_arch = "arm"),
    not(feature = "expected-hardfault"),
    not(feature = "minimal")
))]
fn hardware_timer_us() -> u32 {
    kernel::clock::now_us() as u32
}

#[cfg(not(feature = "expected-hardfault"))]
fn tests() {
    // Leave enough time after UF2 boot for the host to observe BOOTSEL vanish
    // and arm its fresh-return judge; this delay is not part of any timing assertion.
    task::sleep_ms(2_000);
    if kernel::smp::current_core().0 != 0 || CORE1_RUNS.load(Ordering::Relaxed) == 0 {
        fail(13);
    }
    // Register-frame sentinels are covered by the single-core acceptance
    // suite. This image keeps both cores live and tests SMP-owned behavior.
    if WRONG_CORE_RUNS.load(Ordering::Relaxed) != 0 || CORE1_RUNS.load(Ordering::Relaxed) == 0 {
        fail(13);
    }
    let sleep_start = timer::now_ms();
    task::sleep_ms(15);
    let slept = timer::now_ms().wrapping_sub(sleep_start);
    if !(15..=500).contains(&slept) {
        fail(2);
    }

    #[cfg(feature = "minimal")]
    unsafe {
        soc_rp2040::test_status::pass_to_bootsel()
    }

    #[cfg(not(feature = "minimal"))]
    run_extended_tests();
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
fn run_extended_tests() -> ! {
    uart_loopback_test();
    gpio_edge_loopback_test();
    let timeout_start = timer::now_ms();
    let timeout_result = api::queue::recv(&ISR_QUEUE, 5);
    let timeout_elapsed = timer::now_ms().wrapping_sub(timeout_start);
    if timeout_result != Err(RecvError::Timeout) || !(5..=500).contains(&timeout_elapsed) {
        fail(3);
    }

    let outer = unsafe { kernel::arch::cs_enter() };
    let inner = unsafe { kernel::arch::cs_enter() };
    let masked_tick = timer::now_ms();
    let hardware_start = hardware_timer_us();
    while hardware_timer_us().wrapping_sub(hardware_start) < 5_000 {
        core::hint::spin_loop();
    }
    if timer::now_ms() != masked_tick {
        fail(4);
    }
    unsafe { kernel::arch::cs_exit(inner) };
    if timer::now_ms() != masked_tick {
        fail(5);
    }
    unsafe { kernel::arch::cs_exit(outer) };
    task::sleep_ms(2);
    if timer::now_ms() <= masked_tick {
        fail(6);
    }

    let (stack_base, stack_size) = kernel::scheduler::with(|sched| {
        let current = sched.current();
        let tcb = sched.tasks[current as usize].as_ref().expect("current TCB");
        (tcb.stack_base, tcb.stack_size)
    });
    if !kernel::spawn::stack_guard_intact(stack_base, stack_size) {
        fail(7);
    }

    let before = kernel::heap::free_bytes(kernel::heap::Caps::Internal);
    let ptr = unsafe { kernel::heap::alloc(257, 16) };
    if ptr.is_null() || ptr as usize & 15 != 0 {
        fail(8);
    }
    unsafe {
        ptr.write_volatile(0xa5);
        ptr.add(256).write_volatile(0x5a);
        if ptr.read_volatile() != 0xa5 || ptr.add(256).read_volatile() != 0x5a {
            fail(8);
        }
        kernel::heap::free(ptr, kernel::heap::Caps::Internal);
    }
    if kernel::heap::free_bytes(kernel::heap::Caps::Internal) != before {
        fail(8);
    }

    if PI_MEDIUM_PARKED.load(Ordering::Acquire) != 1 || PI_HIGH_PARKED.load(Ordering::Acquire) != 1
    {
        fail(20);
    }
    let mut pi_guard = lock(&PI_MUTEX);
    let owner = task::current_id().0;
    PI_OWNER_ID.store(owner + 1, Ordering::Release);
    PI_PHASE.store(2, Ordering::Release);
    let medium = PI_MEDIUM_ID.load(Ordering::Acquire).wrapping_sub(1);
    let high = PI_HIGH_ID.load(Ordering::Acquire).wrapping_sub(1);
    kernel::scheduler::with(|sched| {
        sched.unblock(medium);
        sched.unblock(high);
    });
    task::yield_now();
    while PI_PHASE.load(Ordering::Acquire) != 3 {
        core::hint::spin_loop();
    }
    let effective = kernel::scheduler::with(|sched| {
        sched.tasks[owner as usize]
            .as_ref()
            .map_or(u8::MAX, |task| task.priority)
    });
    if effective == Priority::Critical(0).numeric() {
        PI_BOOST_SEEN.store(1, Ordering::Release);
    }
    *pi_guard = pi_guard.wrapping_add(1);
    drop(pi_guard);
    let pi_deadline = hardware_timer_us().wrapping_add(2_000_000);
    while PI_PHASE.load(Ordering::Acquire) < 4 {
        if hardware_timer_us().wrapping_sub(pi_deadline) < u32::MAX / 2 {
            fail(20);
        }
        task::sleep_ms(10);
    }
    if PI_PHASE.load(Ordering::Acquire) != 4
        || PI_BOOST_SEEN.load(Ordering::Acquire) != 1
        || PI_RESTORE_SEEN.load(Ordering::Acquire) != 1
        || PI_MEDIUM_RAN_DURING_INVERSION.load(Ordering::Acquire) != 0
    {
        fail(21);
    }
    let medium_deadline = hardware_timer_us().wrapping_add(500_000);
    while PI_MEDIUM_FINISHED.load(Ordering::Acquire) == 0 {
        if hardware_timer_us().wrapping_sub(medium_deadline) < u32::MAX / 2 {
            fail(22);
        }
        task::sleep_ms(10);
    }

    ISR_NEXT.store(0, Ordering::Relaxed);
    ISR_PRODUCING.store(0, Ordering::Relaxed);
    ISR_SENT.store(0, Ordering::Relaxed);
    while ISR_QUEUE.try_recv().is_ok() {}
    let producer = timer::every_ms(1, isr_queue_producer);
    const ISR_ATTEMPTS: u32 = 256;
    let mut received = 0u32;
    let mut last = None;
    while ISR_NEXT.load(Ordering::Relaxed) < ISR_ATTEMPTS {
        if let Ok(value) = ISR_QUEUE.try_recv() {
            if last.is_some_and(|previous| value <= previous) {
                timer::cancel(producer);
                fail(23);
            }
            last = Some(value);
            received += 1;
        }
    }
    timer::cancel(producer);
    while let Ok(value) = ISR_QUEUE.try_recv() {
        if last.is_some_and(|previous| value <= previous) {
            fail(23);
        }
        last = Some(value);
        received += 1;
    }
    if received == 0 || received != ISR_SENT.load(Ordering::Relaxed) {
        fail(24);
    }
    for iteration in 0..1_000 {
        MUTEX_SOAK_PROGRESS.store(iteration * 2 + 1, Ordering::Release);
        let mut guard = lock(&PI_MUTEX);
        *guard = guard.wrapping_add(1);
        drop(guard);
        MUTEX_SOAK_PROGRESS.store(iteration * 2 + 2, Ordering::Release);
    }

    // Repeatedly block core 0 and require a task pinned to core 1 to wake it.
    // The raw peripheral timer remains independent of the scheduler tick, so
    // the upper bound also catches a stopped/doubled shared timebase.
    const SOAK_ITERATIONS: u32 = 256;
    let lock_before = SMP_LOCK.with(|count| *count);
    let core0_before = PEER_RUNS.load(Ordering::Relaxed);
    let core1_before = CORE1_RUNS.load(Ordering::Relaxed);
    let soak_start = hardware_timer_us();
    let mut worst_wake_us = 0;
    let test_task = kernel::scheduler::with(|sched| sched.current());
    for _ in 1..=SOAK_ITERATIONS {
        let wake_start = hardware_timer_us();
        REMOTE_REQUEST.store(test_task + 1, Ordering::Release);
        task::sleep_ms(500);
        if REMOTE_REQUEST.load(Ordering::Acquire) != 0 {
            fail(14);
        }
        worst_wake_us = worst_wake_us.max(hardware_timer_us().wrapping_sub(wake_start));
        SMP_LOCK.with(|count| {
            // Keep the protected interval non-empty so the two pinned tasks
            // exercise real SIO-lock contention, not merely serialized calls.
            let next = count.wrapping_add(1);
            core::hint::black_box(next);
            *count = next;
        });
        // Give the lower-priority core-0 peer a deterministic window. A
        // yield need not select a lower-priority task, while sleep must.
        task::sleep_ms(1);
    }
    let soak_us = hardware_timer_us().wrapping_sub(soak_start);
    if worst_wake_us > 500_000 || soak_us > 30_000_000 {
        fail(15);
    }
    if DUPLICATE_RUNS.load(Ordering::Relaxed) != 0 || WRONG_CORE_RUNS.load(Ordering::Relaxed) != 0 {
        fail(16);
    }
    if PEER_RUNS.load(Ordering::Relaxed) == core0_before
        || CORE1_RUNS.load(Ordering::Relaxed) < core1_before.wrapping_add(SOAK_ITERATIONS)
    {
        fail(17);
    }
    let lock_after = SMP_LOCK.with(|count| *count);
    if lock_after.wrapping_sub(lock_before) < SOAK_ITERATIONS * 2 {
        fail(18);
    }
    unsafe { soc_rp2040::test_status::pass_to_bootsel() }
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
fn uart_loopback_test() {
    let Ok(led) = rp2040_gpio::Rp2040Pin::open(&kernel::board::active::USER_LED) else {
        fail(19);
    };
    if rp2040_gpio::Rp2040Pin::open(&kernel::board::active::USER_LED).is_ok() {
        fail(19);
    }
    if led.set_mode(rp2040_gpio::PinMode::Output).is_err()
        || led.write(rp2040_gpio::PinLevel::High).is_err()
        || led.read() != Ok(rp2040_gpio::PinLevel::High)
        || led.write(rp2040_gpio::PinLevel::Low).is_err()
    {
        fail(19);
    }
    let Ok(uart) = rp2040_uart::Rp2040Uart::open(&kernel::board::active::SELFTEST_UART) else {
        fail(19);
    };
    if rp2040_uart::Rp2040Uart::open(&kernel::board::active::SELFTEST_UART).is_ok() {
        fail(19);
    }
    uart.set_loopback(true);
    for iteration in 0..1_000u32 {
        let mut tx = [0u8; 16];
        for (index, byte) in tx.iter_mut().enumerate() {
            *byte = (iteration as u8)
                .wrapping_mul(29)
                .wrapping_add((index as u8).wrapping_mul(17));
        }
        if uart.write(&tx) != tx.len() {
            fail(19);
        }
        let start = hardware_timer_us();
        let mut rx = [0; 16];
        let mut received = 0;
        while received < rx.len() && hardware_timer_us().wrapping_sub(start) < 100_000 {
            received += uart.read(&mut rx[received..]);
        }
        if received != rx.len() || rx != tx || uart.errors().any() {
            fail(19);
        }
    }
    api::log_info!("[FLINT] ARM UART LOOPBACK payloads=1000 bytes=16000");
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
fn gpio_edge_isr() {
    let Some(input) = GPIO_LOOPBACK_INPUT.get() else {
        GPIO_EDGE_ERRORS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    match input.take_edge_events() {
        Ok(events) => {
            let count = u32::from(events.falling) + u32::from(events.rising);
            if count == 0 {
                GPIO_EDGE_ERRORS.fetch_add(1, Ordering::Relaxed);
            } else {
                GPIO_EDGE_COUNT.fetch_add(count, Ordering::Relaxed);
            }
        }
        Err(_) => {
            GPIO_EDGE_ERRORS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
fn gpio_edge_loopback_test() {
    let Ok(output) = rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_OUT) else {
        fail(25);
    };
    let Ok(input) = rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_IN) else {
        fail(25);
    };
    if rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_OUT).is_ok()
        || rp2040_gpio::Rp2040Pin::open(&kernel::board::active::GPIO_LOOPBACK_IN).is_ok()
    {
        fail(25);
    }
    if output.set_mode(rp2040_gpio::PinMode::Output).is_err()
        || output.write(rp2040_gpio::PinLevel::Low).is_err()
        || input.set_mode(rp2040_gpio::PinMode::InputPullDown).is_err()
    {
        fail(25);
    }
    GPIO_LOOPBACK_INPUT.init(input);
    if unsafe {
        kernel::interrupt::connect_at(
            soc_rp2040::IRQ_IO_BANK0,
            soc_rp2040::IRQ_IO_BANK0,
            gpio_edge_isr,
        )
    }
    .is_err()
        || GPIO_LOOPBACK_INPUT
            .get()
            .expect("GPIO loopback input")
            .enable_edge_interrupt(rp2040_gpio::Edge::Both)
            .is_err()
    {
        fail(25);
    }
    GPIO_EDGE_COUNT.store(0, Ordering::Relaxed);
    GPIO_EDGE_ERRORS.store(0, Ordering::Relaxed);
    const EDGES: u32 = 10_000;
    for expected in 1..=EDGES {
        let level = if expected & 1 == 1 {
            rp2040_gpio::PinLevel::High
        } else {
            rp2040_gpio::PinLevel::Low
        };
        if output.write(level).is_err() {
            fail(26);
        }
        let deadline = hardware_timer_us().wrapping_add(10_000);
        while GPIO_EDGE_COUNT.load(Ordering::Acquire) < expected {
            if hardware_timer_us().wrapping_sub(deadline) < u32::MAX / 2 {
                fail(26);
            }
            core::hint::spin_loop();
        }
        if GPIO_EDGE_COUNT.load(Ordering::Acquire) != expected
            || GPIO_LOOPBACK_INPUT
                .get()
                .expect("GPIO loopback input")
                .read()
                != Ok(level)
        {
            fail(26);
        }
    }
    if GPIO_LOOPBACK_INPUT
        .get()
        .expect("GPIO loopback input")
        .disable_edge_interrupt()
        .is_err()
        || GPIO_EDGE_COUNT.load(Ordering::Acquire) != EDGES
        || GPIO_EDGE_ERRORS.load(Ordering::Acquire) != 0
    {
        fail(27);
    }
    api::log_info!("[FLINT] ARM GPIO LOOPBACK edges={}", EDGES);
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(not(target_arch = "arm"))]
fn gpio_edge_loopback_test() {}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(not(target_arch = "arm"))]
fn uart_loopback_test() {}

#[cfg(feature = "expected-hardfault")]
fn inject_hardfault() {
    task::sleep_ms(750);
    unsafe {
        soc_rp2040::test_status::arm_expected_fault();
        arm_inject_expected_fault()
    }
}
