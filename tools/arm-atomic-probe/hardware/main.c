// SPDX-License-Identifier: Apache-2.0

#include <inttypes.h>
#include <stdio.h>

#include "hardware/sync.h"
#include "pico/multicore.h"
#include "pico/stdlib.h"
#include "pico/time.h"

enum {
    ATOMIC_SPINLOCK = PICO_SPINLOCK_ID_OS1,
    ITERATIONS_PER_CORE = 100000,
    IRQ_TARGET = 1000,
};

typedef struct {
    uint32_t depth;
    uint32_t interrupt_state;
} local_lock_state_t;

static spin_lock_t *atomic_lock;
static local_lock_state_t local_state[NUM_CORES];
static volatile uint8_t byte_value;
static volatile uint32_t word_value;
static volatile uintptr_t pointer_value;
static volatile uint32_t irq_count;
static volatile bool core1_done;

static void atomic_enter(void) {
    local_lock_state_t *state = &local_state[get_core_num()];
    if (state->depth++ == 0) {
        state->interrupt_state = spin_lock_blocking(atomic_lock);
    }
}

static void atomic_exit(void) {
    local_lock_state_t *state = &local_state[get_core_num()];
    if (--state->depth == 0) {
        spin_unlock(atomic_lock, state->interrupt_state);
    }
}

static uint32_t fetch_add_word(uint32_t increment) {
    atomic_enter();
    uint32_t previous = word_value;
    word_value = previous + increment;
    atomic_exit();
    return previous;
}

static uintptr_t fetch_add_pointer(uintptr_t increment) {
    atomic_enter();
    uintptr_t previous = pointer_value;
    pointer_value = previous + increment;
    atomic_exit();
    return previous;
}

static bool compare_exchange_byte(uint8_t current, uint8_t replacement) {
    bool exchanged = false;
    atomic_enter();
    if (byte_value == current) {
        byte_value = replacement;
        exchanged = true;
    }
    atomic_exit();
    return exchanged;
}

static bool timer_irq(struct repeating_timer *timer) {
    (void)timer;
    atomic_enter();
    irq_count++;
    atomic_exit();
    return irq_count < IRQ_TARGET;
}

static void core1_main(void) {
    for (uint32_t i = 0; i < ITERATIONS_PER_CORE; ++i) {
        fetch_add_word(1);
        fetch_add_pointer(1);
    }
    core1_done = true;
    __sev();
}

int main(void) {
    stdio_init_all();
    sleep_ms(2000);

    atomic_lock = spin_lock_instance(ATOMIC_SPINLOCK);
    spin_lock_claim(ATOMIC_SPINLOCK);

    atomic_enter();
    bool nested_ok = compare_exchange_byte(0, 1);
    atomic_exit();

    struct repeating_timer timer;
    bool timer_started = add_repeating_timer_us(-100, timer_irq, NULL, &timer);
    multicore_launch_core1(core1_main);

    for (uint32_t i = 0; i < ITERATIONS_PER_CORE; ++i) {
        fetch_add_word(1);
        fetch_add_pointer(1);
    }
    while (!core1_done || irq_count < IRQ_TARGET) {
        __wfe();
    }
    cancel_repeating_timer(&timer);

    uint32_t expected = 2 * ITERATIONS_PER_CORE;
    bool pass = timer_started && nested_ok && byte_value == 1 &&
                word_value == expected && pointer_value == expected &&
                irq_count == IRQ_TARGET && local_state[0].depth == 0 &&
                local_state[1].depth == 0;

    for (;;) {
        printf("FLINTOS-ARM-ATOMIC %s word=%" PRIu32 " pointer=%" PRIuPTR
               " irq=%" PRIu32 " nested=%u depth=%" PRIu32 ",%" PRIu32 "\r\n",
               pass ? "PASS" : "FAIL", word_value, pointer_value, irq_count,
               nested_ok, local_state[0].depth, local_state[1].depth);
        sleep_ms(1000);
    }
}
