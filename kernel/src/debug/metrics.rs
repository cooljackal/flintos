/// The metrics table is populated at compile time by the
/// `flint::Counter`/`flint::Gauge` static definitions in user code.

/// Snapshot of all metrics for the shell `metrics` command.
pub struct MetricSnapshot {
    pub name: &'static str,
    pub value: u32,
    pub kind: MetricKind,
}

pub enum MetricKind {
    Counter,
    Gauge,
}

/// Placeholder — Phase 1 stores metrics inline in user statics.
/// A future phase will register them into a kernel table.
pub fn dump() {
    // Walk the static metric table (Phase 2+).
}
