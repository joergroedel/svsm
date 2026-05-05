// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Copyright (c) Microsoft Corporation
//
// Author: Jon Lange <jlange@microsoft.com>

use bootdefs::kernel_launch::BldrLaunchInfo;
use core::arch::asm;
use core::slice;

#[derive(Debug)]
pub struct TempMappings<'a> {
    ptes: &'a mut [u64],
    mappings_vaddr: u64,
    confidentiality_mask: u64,
    index: usize,
    active: bool,
}

impl<'a> TempMappings<'a> {
    /// # Safety
    /// The caller must ensure that the physical and virtual addresses for the
    /// page table mappings specified in the launch info are correct and can
    /// be used for mapping purposes.
    pub unsafe fn new(launch_info: &BldrLaunchInfo, confidentiality_mask: u64) -> Self {
        // SAFETY: the caller guarantees the correctness of the page table
        // addresses.
        let ptes = unsafe {
            slice::from_raw_parts_mut(launch_info.page_table_start as usize as *mut u64, 0x200)
        };
        Self {
            ptes,
            mappings_vaddr: launch_info.page_table_map_vaddr,
            confidentiality_mask,
            index: 0,
            active: false,
        }
    }

    /// # Safety
    /// The caller must guarantee that the address describes memory that can
    /// be modified safely.
    pub unsafe fn map_page<'b>(&'b mut self, paddr: u64) -> TempMappingRef<'b, 'a> {
        // A second mapping cannot be created while another mapping is active.
        assert!(!self.active);

        // If the next index in sequence is beyond the end of this page, then
        // force a TLB flush so the PTEs can be reused.
        if self.index == self.ptes.len() {
            // SAFETY: the paging root will be reloaded with its current
            // value, which is always safe.
            unsafe {
                asm!("movq %cr3, %rax",
                     "movq %rax, %cr3",
                     out("rax") _,
                     options(att_syntax));
            }

            self.index = 0;
        }

        // Construct a new PTE to map the specified physical address, which
        // must be aligned to a page boundary.  The PTE encodes valid,
        // write, accessed, and dirty.
        assert!((paddr & 0xFFF) == 0);
        let pte_index = self.index;
        self.ptes[self.index] = 0x63 | paddr | self.confidentiality_mask;
        let vaddr = self.mappings_vaddr as usize + (self.index << 12);
        self.index += 1;
        self.active = true;

        // Construct a mapping reference that describes this mapping.  The
        // reference object will release the `active` state once the mapping
        // has been dropped.
        TempMappingRef {
            mappings: self,
            pte_index,
            vaddr,
        }
    }

    fn clear_pte(&mut self, index: usize) {
        self.ptes[index] = 0;
    }
}

#[derive(Debug)]
pub struct TempMappingRef<'b, 'a> {
    mappings: &'b mut TempMappings<'a>,
    pte_index: usize,
    vaddr: usize,
}

impl TempMappingRef<'_, '_> {
    /// # Safety
    /// The caller must ensure that the mapping that was created can be
    /// interpreted as a slice of type `T`.
    pub unsafe fn as_slice<T>(&mut self) -> &mut [T] {
        // SAFETY: the caller guarantees the correct usability of type `T`
        // when generating the slice.
        unsafe { slice::from_raw_parts_mut(self.vaddr as *mut T, 0x1000 / size_of::<T>()) }
    }
}

impl Drop for TempMappingRef<'_, '_> {
    fn drop(&mut self) {
        self.mappings.clear_pte(self.pte_index);
        self.mappings.active = false;
    }
}
