# Multicore

Flint runs one scheduler across every core. A task is a task; the core it lands
on is a scheduling decision, not a property of the code.

That is the whole design, and it only works for **symmetric** cores — identical
instruction set, identical memory view, identical clock. Asymmetric packages (a
Cortex-M7 next to an M0) are deliberately out of scope. See
[Asymmetric cores](#asymmetric-cores) for why.

On the ESP32 that means the PRO CPU and the APP CPU, both Xtensa LX6.

---

## Starting the second core

It does not start itself. Something on the first core has to release it:

```rust
unsafe {
    arch_xtensa::appcpu::prepare(second_core);
    soc_esp32::appcpu::start(arch_xtensa::appcpu::_flint_appcpu_entry);
}
```

Then the second core's entry hands itself to the scheduler and never returns:

```rust
#[link_section = ".iram1.second_core"]
extern "C" fn second_core() -> ! {
    unsafe { kernel::boot::join_scheduler() }
}
```

From that point the two cores are peers. Both take timer ticks, both service
interrupts, both run tasks out of the same ready queue.

### `.iram1` is not optional

The second core has **no instruction cache** when it starts. Its entry runs
from a reset vector, and until the first core calls `Cache_Flush(1)` and
`Cache_Read_Enable(1)` on its behalf, every byte of flash reads as garbage.

A function in `.text` lives in flash. Putting the second core's entry there
gets an *IllegalInstruction* exception, immediately, every time — which reads
like a miscompile and is not one.

`appcpu::start` enables the cache, so anything the entry *calls* can live in
flash. The entry itself cannot.

### The three holds

The APP CPU is held by three separate mechanisms, and releasing two of them
does nothing observable. All three, in this order:

| Hold | Register | Bits |
|---|---|---|
| RTC stall | `RTC_CNTL_OPTIONS0` | `[1:0]`, plus `SW_CPU_STALL[25:20]` |
| Clock gate | `DPORT_APPCPU_CTRL_B` | bit 0 |
| Reset | `DPORT_APPCPU_CTRL_A` | bit 0 |

`soc_esp32::appcpu::start` does all of it. The table is here because a core
that stays dark after a partial release gives no diagnostic at all.

---

## Where a task runs

By default, anywhere:

```rust
task::spawn("worker", worker, Priority::Normal(2), 4096);
```

Pinned to a core, when it matters:

```rust
task::spawn_on(1, "sensor", sensor, Priority::Normal(2), 4096);
```

Pinning is a constraint on the scheduler, not a hint. A task pinned to core 1
will sit Ready while core 0 idles rather than run in the wrong place.

### When to pin

Pin when the task's correctness depends on the core, not when you are guessing
about performance:

- it touches a peripheral whose interrupt is routed to that core
- it must not compete with an interrupt-heavy core for cycles
- you are measuring one core's behaviour and need the other one quiet

Otherwise leave it unpinned. The scheduler balances better than a guess does,
and a pinned task that did not need pinning is one that cannot use an idle
core.

### Priority still wins

Affinity narrows the choice; it does not override priority. Each core
independently picks the highest-priority task that is Ready, allowed on that
core, and not already running elsewhere.

Two consequences worth knowing:

- A high-priority pinned task blocks lower-priority work **on its core only**.
- The same task never runs on two cores at once, even unpinned — the scheduler
  checks `running_elsewhere` before it hands a task out.

---

## What is shared, and what is not

Shared, one copy, protected by a lock:

- the scheduler — ready mask, every TCB, the timer list
- the mutex table
- the log buffer and metrics

Per-core, one copy each:

- the current task (`current_per_core`)
- the pending-switch flag
- the interrupt-nesting depth
- the tick re-arm (each core arms its own comparator)

The tick **count** advances on the boot core only. Both cores take the
interrupt; only one owns the clock. Two cores each incrementing it would make
time run at double speed.

---

## The lock

One spinlock type, `kernel::smp::Spinlock<T>`, and it wraps the data rather
than sitting beside it:

```rust
static SEEN: Spinlock<[[u32; 2]; 3]> = Spinlock::new([[0; 2]; 3]);

SEEN.with(|seen| seen[which][core] += 1);
```

There is no `lock()` returning a guard you can hold across arbitrary code. The
closure form makes the critical section a lexical region, which is what makes
"is this held on the interrupt path" answerable by reading.

### Interrupts are masked first

`with` masks interrupts **before** it takes the lock, and releases in the
opposite order. The other ordering deadlocks: take the lock, then take an
interrupt on the same core, then have the handler want the same lock. It never
comes back, and it happens under load rather than in testing.

### Re-entering panics

Taking a lock you already hold is a bug, not a wait. `with` panics rather than
spinning, because spinning here is a hang with no message — and a hang on core
1 looks exactly like a core that never started.

This is why the panic handler and the log path do **not** go through the
scheduler lock. A panic that blocks on a lock is a panic you never see.

### `try_with`

For the paths that genuinely cannot block:

```rust
if let Some(v) = LOCK.try_with(|state| state.snapshot()) {
    // got it
}
```

Returns `None` rather than waiting. Used where dropping the update beats
stalling — diagnostics, mostly.

---

## Asymmetric cores

Out of scope, on purpose.

Two cores of different architectures usually means two *operating systems* —
the small core running something else entirely, or nothing. Programming against
that means picking a memory model, a boot protocol and an IPC mechanism that
vary per package, with no way to test the combinations.

Symmetric SMP is a well-defined problem with one answer. Asymmetric is a
different design, and starting it before symmetric is solid would produce a
worse version of both.

---

## Verifying it on hardware

```bash
make flash APP=smp BOARD=board-m5-atom-matrix
```

Three tasks — one pinned to core 0, one to core 1, one unpinned — each
recording which core it observed itself on, every iteration. All three sleep,
which forces a switch: a pinned task that is never preempted proves nothing.

What a correct run shows:

```
pin0   core0=N  core1=0
pin1   core0=0  core1=N
float  core0=N  core1=M
```

Both zeros matter. A pinned task with a nonzero count on the wrong core is a
scheduler bug; `float` with a zero is a second core that never actually ran.

---

## Sources

- ESP32 TRM §31 (DPORT), §29 (RTC_CNTL)
- `soc/esp32/src/appcpu.rs` — the three holds and the cache enable
- `kernel/src/smp.rs` — the lock
- `kernel/src/scheduler.rs` — affinity and per-core current
- `apps/smp/` — the hardware test
