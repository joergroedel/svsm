// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) 2026 Advanced Micro Devices, Inc.
//
// Author: Joerg Roedel <joerg.roedel@amd.com>

//! Generic address-range allocation.

use crate::types::{PAGE_SHIFT, PAGE_SIZE};

use core::cmp::max;

use intrusive_collections::rbtree::{Link, RBTree};
use intrusive_collections::{KeyAdapter, intrusive_adapter};

extern crate alloc;
use alloc::boxed::Box;

#[derive(Debug)]
struct Allocation<T> {
    link: Link,
    start_pfn: usize,
    end_pfn: usize,
    data: T,
}

impl<T> Allocation<T> {
    const fn new(start_pfn: usize, end_pfn: usize, data: T) -> Self {
        Self {
            link: Link::new(),
            start_pfn,
            end_pfn,
            data,
        }
    }
}

intrusive_adapter!(AllocationAdapter<T> = Box<Allocation<T>>: Allocation<T> { link => Link });

impl<'a, T> KeyAdapter<'a> for AllocationAdapter<T> {
    type Key = usize;

    fn get_key(&self, allocation: &'a Allocation<T>) -> Self::Key {
        allocation.start_pfn
    }
}

/// A page-granular address allocator carrying metadata of type `T`.
///
/// Allocations are kept in an intrusive red-black tree ordered by their base
/// address. Allocation uses a first-fit policy, starting at the bottom of the
/// managed address range.
#[derive(Debug)]
pub struct UniqueVaAllocator<T> {
    tree: RBTree<AllocationAdapter<T>>,
    start_pfn: usize,
    end_pfn: usize,
}

impl<T> UniqueVaAllocator<T> {
    /// Creates an allocator for the address range `[start, end)`.
    ///
    /// # Arguments
    ///
    /// * `start` - Page-aligned start of the managed address range.
    /// * `end` - Page-aligned, exclusive end of the managed address range.
    ///
    /// # Panics
    ///
    /// Panics if either bound is not page-aligned or `start` is greater than
    /// `end`.
    pub fn new(start: usize, end: usize) -> Self {
        assert!(start <= end);
        assert!(start.is_multiple_of(PAGE_SIZE));
        assert!(end.is_multiple_of(PAGE_SIZE));

        Self {
            tree: RBTree::new(AllocationAdapter::new()),
            start_pfn: start >> PAGE_SHIFT,
            end_pfn: end >> PAGE_SHIFT,
        }
    }

    /// Allocates a range with a specified alignment and associates `data`
    /// with it.
    ///
    /// The allocation size is rounded up to its next power of two. The first
    /// suitable address in the managed range is returned.
    ///
    /// # Arguments
    ///
    /// * `size` - Number of bytes requested.
    /// * `align` - Power-of-two alignment of at least one page.
    /// * `data` - Metadata associated with the allocation.
    ///
    /// # Returns
    ///
    /// The allocation base address, or [`None`] if no suitable range exists or
    /// the rounded size overflows.
    ///
    /// # Panics
    ///
    /// Panics if `align` is not a power of two or is smaller than one page.
    pub fn alloc_aligned(&mut self, size: usize, align: usize, data: T) -> Option<usize> {
        assert!(align.is_power_of_two());
        assert!(align >= PAGE_SIZE);

        let size_pfn = size.checked_next_power_of_two()? >> PAGE_SHIFT;
        if size_pfn == 0 {
            return None;
        }

        let align_pfn = align >> PAGE_SHIFT;
        let align_mask = align_pfn - 1;
        let mut start_pfn = self.start_pfn.checked_add(align_mask)? & !align_mask;
        let mut cursor = self.tree.front_mut();

        while let Some(allocation) = cursor.get() {
            if allocation.start_pfn.saturating_sub(start_pfn) >= size_pfn {
                break;
            }

            let next_pfn = max(start_pfn, allocation.end_pfn);
            start_pfn = next_pfn.checked_add(align_mask)? & !align_mask;
            cursor.move_next();
        }

        if self.end_pfn.saturating_sub(start_pfn) < size_pfn {
            return None;
        }

        let end_pfn = start_pfn.checked_add(size_pfn)?;
        cursor.insert_before(Box::new(Allocation::new(start_pfn, end_pfn, data)));

        Some(start_pfn << PAGE_SHIFT)
    }

    /// Allocates a naturally aligned range and associates `data` with it.
    ///
    /// The allocation size is rounded up to its next power of two, which is
    /// also used as its alignment.
    ///
    /// # Returns
    ///
    /// The allocation base address, or [`None`] if no suitable range exists or
    /// the rounded size overflows.
    pub fn alloc(&mut self, size: usize, data: T) -> Option<usize> {
        let align = size.checked_next_power_of_two()?;
        if align < PAGE_SIZE {
            return None;
        }
        self.alloc_aligned(size, align, data)
    }

    /// Returns the metadata associated with an allocation base address.
    ///
    /// Addresses within an allocation but not equal to its base do not match.
    pub fn get(&self, start: usize) -> Option<&T> {
        if !start.is_multiple_of(PAGE_SIZE) {
            return None;
        }

        self.tree
            .find(&(start >> PAGE_SHIFT))
            .get()
            .map(|allocation| &allocation.data)
    }

    /// Releases the allocation at `start`.
    ///
    /// The address must exactly match an allocation base. Invalid, unaligned,
    /// or already free addresses are ignored.
    pub fn free(&mut self, start: usize) {
        if !start.is_multiple_of(PAGE_SIZE) {
            return;
        }

        self.tree.find_mut(&(start >> PAGE_SHIFT)).remove();
    }
}

#[cfg(test)]
mod tests {
    use super::UniqueVaAllocator;

    const MIB: usize = 1024 * 1024;
    const GIB: usize = 1024 * MIB;
    const RANGE_START: usize = GIB;
    const RANGE_END: usize = 2 * GIB;

    fn alloc_size<T>(allocator: &mut UniqueVaAllocator<T>, size: usize, data: T) -> usize {
        let allocated_size = size.next_power_of_two();
        let addr = allocator.alloc(size, data).unwrap();

        assert!(addr >= RANGE_START);
        assert!(addr <= RANGE_END - allocated_size);
        assert!(addr.is_multiple_of(allocated_size));

        addr
    }

    #[test]
    fn allocates_naturally_aligned_ranges() {
        let mut allocator = UniqueVaAllocator::new(RANGE_START, RANGE_END);

        assert_eq!(alloc_size(&mut allocator, 23 * MIB, 1), GIB);
        assert_eq!(alloc_size(&mut allocator, 512 * MIB, 2), 1536 * MIB);
        assert!(allocator.alloc(512 * MIB, 3).is_none());
    }

    #[test]
    fn allocates_with_explicit_alignment() {
        let mut allocator = UniqueVaAllocator::new(RANGE_START, RANGE_END);

        assert_eq!(allocator.alloc_aligned(16 * MIB, 256 * MIB, 1), Some(GIB));
        assert_eq!(
            allocator.alloc_aligned(16 * MIB, 256 * MIB, 2),
            Some(1280 * MIB)
        );
    }

    #[test]
    fn returns_allocation_metadata() {
        let mut allocator = UniqueVaAllocator::new(RANGE_START, RANGE_END);
        let addr = alloc_size(&mut allocator, 23 * MIB, 0xf00b05_u64);

        assert_eq!(allocator.get(addr), Some(&0xf00b05));
        assert_eq!(allocator.get(addr + 1), None);
        assert_eq!(allocator.get(addr + 4096), None);
    }

    #[test]
    fn rejects_invalid_sizes() {
        let mut allocator = UniqueVaAllocator::new(RANGE_START, RANGE_END);

        assert!(allocator.alloc(0, ()).is_none());
        assert!(allocator.alloc(RANGE_END - RANGE_START + 1, ()).is_none());
        assert!(allocator.alloc(usize::MAX, ()).is_none());
    }

    #[test]
    fn reuses_freed_ranges() {
        let mut allocator = UniqueVaAllocator::new(RANGE_START, RANGE_END);
        let first = allocator.alloc(256 * MIB, 1).unwrap();
        let second = allocator.alloc(256 * MIB, 2).unwrap();

        allocator.free(first);

        assert_eq!(allocator.get(first), None);
        assert_eq!(allocator.get(second), Some(&2));
        assert_eq!(allocator.alloc(256 * MIB, 3), Some(first));
        assert_eq!(allocator.get(first), Some(&3));
    }

    #[test]
    fn ignores_addresses_that_are_not_allocation_bases() {
        let mut allocator = UniqueVaAllocator::new(RANGE_START, RANGE_END);
        let addr = allocator.alloc(256 * MIB, 1).unwrap();

        allocator.free(addr + 1);
        allocator.free(addr + 4096);

        assert_eq!(allocator.get(addr), Some(&1));
    }
}
