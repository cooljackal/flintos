// SPDX-License-Identifier: Apache-2.0

//! Fail-closed vocabulary for hardware-isolated tasks. A validated layout is
//! geometry, not ownership: the kernel must allocate its memory exclusively.

/// Permissions never include writable executable memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
    ReadExecute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Size,
    Alignment,
    Overflow,
    Overlap,
    OutsideWindow,
    Entry,
    Capacity,
}

/// Half-open address window supplied by the trusted linker/allocator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: u32,
    pub end: u32,
}

impl Window {
    pub fn contains(self, address: u32, bytes: u32) -> bool {
        bytes != 0
            && address >= self.start
            && address
                .checked_add(bytes)
                .is_some_and(|end| end <= self.end)
    }
}

/// Exact power-of-two region. Constructors never round a grant outward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    base: u32,
    size: u32,
    access: Access,
    guard: bool,
}

impl Region {
    pub fn new(base: u32, size: u32, access: Access) -> Result<Self, Error> {
        if size < 256 || !size.is_power_of_two() {
            return Err(Error::Size);
        }
        if base & (size - 1) != 0 {
            return Err(Error::Alignment);
        }
        base.checked_add(size).ok_or(Error::Overflow)?;
        Ok(Self {
            base,
            size,
            access,
            guard: false,
        })
    }

    /// Deny the bottom eighth; it is not included in the usable stack window.
    pub fn stack(base: u32, size: u32) -> Result<Self, Error> {
        let mut region = Self::new(base, size, Access::ReadWrite)?;
        if size < 1024 {
            return Err(Error::Size);
        }
        region.guard = true;
        Ok(region)
    }

    pub const fn base(self) -> u32 {
        self.base
    }
    pub const fn size(self) -> u32 {
        self.size
    }
    pub const fn access(self) -> Access {
        self.access
    }
    pub const fn guarded(self) -> bool {
        self.guard
    }

    pub fn usable(self) -> Window {
        Window {
            start: self.base + if self.guard { self.size / 8 } else { 0 },
            end: self.base + self.size,
        }
    }

    pub fn fits(self, window: Window) -> bool {
        window.contains(self.base, self.size)
    }

    fn overlaps(self, other: Self) -> bool {
        self.base < other.base + other.size && other.base < self.base + self.size
    }
}

/// Fixed-budget initial task domain: shared RX image, private guarded stack,
/// and optional private RW data. Five of the eight hardware slots stay denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskMemory {
    code: Region,
    stack: Region,
    data: Option<Region>,
}

impl TaskMemory {
    pub fn new(
        code: Region,
        stack: Region,
        data: Option<Region>,
        entry: u32,
    ) -> Result<Self, Error> {
        if code.access != Access::ReadExecute || code.guard || !stack.guard {
            return Err(Error::Entry);
        }
        if !code.usable().contains(entry & !1, 2) {
            return Err(Error::Entry);
        }
        if code.overlaps(stack) || data.is_some_and(|r| r.overlaps(code) || r.overlaps(stack)) {
            return Err(Error::Overlap);
        }
        if data.is_some_and(|r| r.access != Access::ReadWrite || r.guard) {
            return Err(Error::Entry);
        }
        Ok(Self { code, stack, data })
    }

    pub const fn code(self) -> Region {
        self.code
    }
    pub const fn stack(self) -> Region {
        self.stack
    }
    pub const fn data(self) -> Option<Region> {
        self.data
    }

    /// Validate an exception frame before any privileged read/write through
    /// the user-controlled stack pointer. Reserve the software save area too.
    pub fn exception_frame(self, psp: u32) -> bool {
        psp & 7 == 0
            && psp
                .checked_sub(32)
                .is_some_and(|base| self.stack.usable().contains(base, 64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn code() -> Region {
        Region::new(0x1000_0000, 4096, Access::ReadExecute).unwrap()
    }
    fn stack() -> Region {
        Region::stack(0x2000_0000, 4096).unwrap()
    }
    #[test]
    fn exact_geometry_never_expands_authority() {
        for size in [0, 1, 32, 128, 255, 257, 1000] {
            assert_eq!(
                Region::new(0x2000_0000, size, Access::ReadWrite),
                Err(Error::Size)
            );
        }
        assert_eq!(
            Region::new(0x2000_0100, 4096, Access::ReadWrite),
            Err(Error::Alignment)
        );
        assert_eq!(
            Region::new(0xffff_ff00, 256, Access::ReadWrite),
            Err(Error::Overflow)
        );
    }
    #[test]
    fn boundaries_and_overflow_are_denied() {
        let w = Window {
            start: 0x1000,
            end: 0x2000,
        };
        assert!(w.contains(0x1000, 4096));
        assert!(!w.contains(0x0fff, 1));
        assert!(!w.contains(0x2000, 1));
        assert!(!w.contains(0x1000, 0));
        assert!(!w.contains(u32::MAX, 2));
    }
    #[test]
    fn guarded_stack_and_frame_bounds() {
        let m = TaskMemory::new(code(), stack(), None, 0x1000_0001).unwrap();
        assert_eq!(
            m.stack().usable(),
            Window {
                start: 0x2000_0200,
                end: 0x2000_1000
            }
        );
        assert!(m.exception_frame(0x2000_0fd8));
        assert!(m.exception_frame(0x2000_0fe0));
        assert!(!m.exception_frame(0x2000_0fe8));
        assert!(!m.exception_frame(0x2000_0200)); // software save would hit guard
        assert!(!m.exception_frame(0x2000_0241));
        assert!(!m.exception_frame(0));
    }
    #[test]
    fn overlapping_grants_and_non_code_entry_fail() {
        assert_eq!(
            TaskMemory::new(code(), stack(), Some(stack()), 0x1000_0001),
            Err(Error::Overlap)
        );
        assert_eq!(
            TaskMemory::new(code(), stack(), None, 0x1000_1001),
            Err(Error::Entry)
        );
        assert_eq!(
            TaskMemory::new(code(), stack(), None, 0x2000_0001),
            Err(Error::Entry)
        );
    }
    #[test]
    fn private_data_must_be_non_executable() {
        let d = Region::new(0x2000_1000, 256, Access::ReadExecute).unwrap();
        assert_eq!(
            TaskMemory::new(code(), stack(), Some(d), 0x1000_0001),
            Err(Error::Entry)
        );
    }
    #[test]
    fn stack_cannot_be_an_unguarded_region() {
        let s = Region::new(0x2000_0000, 4096, Access::ReadWrite).unwrap();
        assert_eq!(
            TaskMemory::new(code(), s, None, 0x1000_0001),
            Err(Error::Entry)
        );
    }
    #[test]
    fn allocator_windows_include_the_guard() {
        assert!(!stack().fits(stack().usable()));
        assert!(stack().fits(Window {
            start: 0x2000_0000,
            end: 0x2000_1000
        }));
    }
}
