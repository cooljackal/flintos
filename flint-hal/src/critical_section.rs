//! Critical section trait.
//!
//! A token-based critical section masks interrupts up to a configurable
//! priority threshold.  Not all interrupts are masked — real-time
//! properties are preserved for high-priority events.
//! The token restores the previous interrupt state when dropped.

/// A critical section implementation.
pub trait CriticalSection {
    /// The token type that holds the saved interrupt state.
    type Token: CriticalSectionToken;

    /// Enter a critical section, returning a token that will restore
    /// interrupt state when dropped.
    fn enter() -> Self::Token;
}

/// Opaque token whose drop restores the previous interrupt state.
pub trait CriticalSectionToken {
    /// Explicitly release (re-enable interrupts).
    fn release(self);
}