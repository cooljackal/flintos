use crate::scheduler;

/// Update the high-water mark for the current task.
/// Called from the context switch path after restoring a task.
pub fn update_hwm(task_id: u32) {
    let sched = scheduler::global();
    if let Some(tcb) = &mut sched.tasks[task_id as usize] {
        let used = check_hwm(tcb.stack_base, tcb.stack_size);
        if used > tcb.stack_hwm {
            tcb.stack_hwm = used;
        }
        // Warn if over 80%.
        #[allow(unused_variables)]
        let pct = if tcb.stack_size > 0 {
            (used * 100) / tcb.stack_size
        } else {
            0
        };
        // TODO: log warning when stack usage exceeds 80%
    }
}

/// Scan the stack for the first non-0xDEADBEEF word.
fn check_hwm(base: u32, size: u32) -> u32 {
    let words = (size / 4) as usize;
    let ptr = base as *const u32;
    let mut used_words = words;
    for i in 0..words {
        unsafe {
            if ptr.add(i).read() == 0xDEADBEEF {
                used_words = i;
                break;
            }
        }
    }
    (used_words * 4) as u32
}
