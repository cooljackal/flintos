#!/usr/bin/env python3
"""Break the kernel on purpose, once per race test, and check the right test notices.

A test that has never failed is not known to work. Each mutation below is the
specific bug its test's doc comment claims to catch, so a test that stays green
under it is testing nothing.

Restores every file from an in-memory copy of the original bytes, in a finally
block, so a crash or a Ctrl-C cannot leave the tree modified.
"""
import subprocess, sys, pathlib, re

ROOT = pathlib.Path(r"D:\work\repos\flintos")

ALL = [
    (
        "isr_queue_delivers_exactly_once",
        "api/src/queue.rs",
        "            let slot = tail % N;",
        "            if tail % 8 == 7 { return Ok(()); } // MUTATION: report a send that never happened\n            let slot = tail % N;",
        "a send is reported successful but never enqueued",
    ),
    (
        "nested_critical_sections_stay_masked",
        "arch/xtensa/src/critical_section.rs",
        'core::arch::asm!("wsr.ps {0}", "rsync", in(reg) self.saved_ps);',
        'core::arch::asm!("wsr.ps {0}", "rsync", in(reg) self.saved_ps & !0xF); // MUTATION: unmask instead of restore',
        "leaving a critical section unmasks rather than restoring",
    ),
    (
        "interrupt_depth_returns_to_zero",
        "kernel/src/interrupt.rs",
        "        IN_INTERRUPT_DEPTH.fetch_sub(1, Ordering::Relaxed);",
        "        // MUTATION: never leave interrupt context",
        "interrupt depth is incremented but never decremented",
    ),
    (
        "ready_mask_agrees_with_task_states",
        "kernel/src/scheduler.rs",
        "        let mut need_switch = false;\n        let cur_prio = self.current_priority();",
        "        let mut need_switch = false;\n        self.ready_mask |= 1u64 << 47; // MUTATION: a ready bit with no task behind it\n        let cur_prio = self.current_priority();",
        "ready_mask carries a bit no task justifies",
    ),
    (
        "pending_switch_is_taken_once",
        "kernel/src/scheduler.rs",
        "    PENDING_SWITCH.swap(false, Ordering::Relaxed)",
        "    PENDING_SWITCH.load(Ordering::Relaxed) // MUTATION: read without consuming",
        "a pending switch is never consumed, so it fires twice",
    ),
    (
        "mutex_cycle_under_ticks_leaves_no_residue",
        "kernel/src/mutex.rs",
        "        recompute_owner_priority(prev_owner);",
        "        // MUTATION: leave the boosted priority in place",
        "an inherited priority is never given back",
    ),
]

# Run every mutation. Narrow this while iterating on one test.
MUTATIONS = ALL

# Invoke the harness directly. Through `make` the capture is intermittently
# garbled and the judge refuses it -- correctly, but it makes a mutation run
# unreadable.
# Never bare "bash": on Windows that resolves to the WSL launcher, which
# cannot see the toolchain or open a COM port. The Makefile pins it for the
# same reason.
import shutil
# Never bare "bash": on Windows that resolves to the WSL launcher, which
# cannot see the toolchain or open a COM port -- the run then reports
# "not caught" for a mutation that never reached the board. The Makefile
# pins it for the same reason.
BASH = shutil.which("bash")
for _c in (r"C:/Program Files/Git/usr/bin/bash.exe", "/usr/bin/bash", "/bin/bash"):
    if pathlib.Path(_c).exists():
        BASH = _c
        break
CMD = [BASH, "tools/target-test.sh"]
ENV = {"APP": "demo", "BOARD": "board-m5-atom-matrix", "DEBUG": "debug-level-1",
       "PORT": "COM5", "FLASH_BAUD": "115200", "MONITOR_BAUD": "115200",
       "ESPFLASH_CHIP": "esp32", "FLASH_MODE": "dio"}


def run_suite():
    """Return (harness_verdict, [names of tests that failed])."""
    import os
    env = dict(os.environ); env.update(ENV)
    p = subprocess.run(CMD, cwd=ROOT, capture_output=True, timeout=900, env=env)
    out = (p.stdout or b"").decode("utf-8", "replace") + (p.stderr or b"").decode("utf-8", "replace")
    # "[FLINT] TEST <name> FAIL <reason>" -- see tools/target-test.sh MARK_TEST.
    failed = re.findall(r"\[FLINT\] TEST (\S+) FAIL", out)
    passed = re.findall(r"\[FLINT\] TEST (\S+) PASS", out)
    verdict = "PASS" if "PASS:" in out and p.returncode == 0 else "FAIL"
    if passed and not failed:
        verdict += " (every test still passed)"
    return verdict, failed, out


def main():
    results = []
    for name, relpath, find, replace, why in MUTATIONS:
        path = ROOT / relpath
        original = path.read_bytes()
        text = original.decode("utf-8")
        if find not in text:
            results.append((name, "SKIPPED", "anchor not found in " + relpath, []))
            print(f"!! {name}: anchor not found in {relpath}", flush=True)
            continue
        try:
            path.write_bytes(text.replace(find, replace, 1).encode("utf-8"))
            print(f"\n== {name}\n   bug: {why}", flush=True)
            verdict, failed, out = run_suite()
            caught = name in failed
            print(f"   suite: {verdict}; failed tests: {failed or 'none'}", flush=True)
            results.append((name, verdict, why, failed))
            (ROOT / "target" / f"mutant-{name}.log").write_text(out, encoding="utf-8")
            if verdict == "PASS":
                print("   !! the suite stayed green under a real bug", flush=True)
        finally:
            path.write_bytes(original)

    print("\n\n==================== SUMMARY ====================")
    for name, verdict, why, failed in results:
        caught = "CAUGHT by itself" if name in failed else (
            "caught by: " + ", ".join(failed) if failed else "NOT CAUGHT")
        print(f"{verdict:8} {name}\n         {caught}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
