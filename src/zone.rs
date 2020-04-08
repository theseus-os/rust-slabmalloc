//! A ZoneAllocator to allocate arbitrary object sizes (up to `ZoneAllocator::MAX_ALLOC_SIZE`)
//!
//! The ZoneAllocator achieves this by having many `SCAllocator`

use crate::*;

/// Creates an instance of a zone, we do this in a macro because we
/// re-use the code in const and non-const functions
///
/// We can get rid of this once the const fn feature is fully stabilized.
macro_rules! new_zone {
    () => {
        ZoneAllocator {
            // TODO(perf): We should probably pick better classes
            // rather than powers-of-two (see SuperMalloc etc.)
            small_slabs: [
                SCAllocator::new(1 << 3),  // 8
                SCAllocator::new(1 << 4),  // 16
                SCAllocator::new(1 << 5),  // 32
                SCAllocator::new(1 << 6),  // 64
                SCAllocator::new(1 << 7),  // 128
                SCAllocator::new(1 << 8),  // 256
                SCAllocator::new(1 << 9),  // 512
                SCAllocator::new(1 << 10), // 1024 (TODO: maybe get rid of this class?)
                SCAllocator::new(1 << 11), // 2048 (TODO: maybe get rid of this class?)
                SCAllocator::new(1 << 12), // 4096 
                SCAllocator::new(ZoneAllocator::MAX_ALLOC_SIZE),    // 8104 (can't do 8192 because of metadata in ObjectPage)
            ]
        }
    };
}

/// A zone allocator for arbitrary sized allocations.
///
/// Has a bunch of `SCAllocator` and through that can serve allocation
/// requests for many different object sizes up to (MAX_SIZE_CLASSES) by selecting
/// the right `SCAllocator` for allocation and deallocation.
///
/// The allocator provides to refill functions `refill` and `refill_large`
/// to provide the underlying `SCAllocator` with more memory in case it runs out.
pub struct ZoneAllocator<'a> {
    small_slabs: [SCAllocator<'a, ObjectPage8k<'a>>; ZoneAllocator::MAX_BASE_SIZE_CLASSES],
    // big_slabs: [SCAllocator<'a, LargeObjectPage<'a>>; ZoneAllocator::MAX_LARGE_SIZE_CLASSES],
}

impl<'a> Default for ZoneAllocator<'a> {
    fn default() -> ZoneAllocator<'a> {
        new_zone!()
    }
}

#[allow(dead_code)]
enum Slab {
    Base(usize),
    Large(usize),
    Unsupported,
}


impl<'a> ZoneAllocator<'a> {
    /// Maximum size that allocated within 2 pages. (8 KiB - 88 bytes)
    /// This is also the maximum object size that this allocator can handle.
    pub const MAX_ALLOC_SIZE: usize = ObjectPage8k::SIZE - ObjectPage8k::METADATA_SIZE;

    /// Maximum size which is allocated with ObjectPages8k (4 KiB pages).
    ///
    /// e.g. this is 8 KiB - 88 bytes of meta-data.
    pub const MAX_BASE_ALLOC_SIZE: usize = ZoneAllocator::MAX_ALLOC_SIZE;

    /// How many allocators of type SCAllocator<ObjectPage8k> we have.
    pub const MAX_BASE_SIZE_CLASSES: usize = 11;

    /// The set of sizes the allocator has lists for.
    pub const BASE_ALLOC_SIZES: [usize; ZoneAllocator::MAX_BASE_SIZE_CLASSES] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, ZoneAllocator::MAX_BASE_ALLOC_SIZE];

    /// A slab must have greater than this number of empty pages to return one.
    const SLAB_EMPTY_PAGES_THRESHOLD: usize = 0;

    #[cfg(feature = "unstable")]
    pub const fn new() -> ZoneAllocator<'a> {
        new_zone!()
    }

    #[cfg(not(feature = "unstable"))]
    pub fn new() -> ZoneAllocator<'a> {
        new_zone!()
    }


    /// Return maximum size an object of size `current_size` can use.
    ///
    /// Used to optimize `realloc`.
    #[allow(dead_code)]
    fn get_max_size(current_size: usize) -> Option<usize> {
        match current_size {
            0..=8 => Some(8),
            9..=16 => Some(16),
            17..=32 => Some(32),
            33..=64 => Some(64),
            65..=128 => Some(128),
            129..=256 => Some(256),
            257..=512 => Some(512),
            513..=1024 => Some(1024),
            1025..=2048 => Some(2048),
            2049..=4096 => Some(4096),
            4097..=ZoneAllocator::MAX_ALLOC_SIZE => Some(ZoneAllocator::MAX_ALLOC_SIZE),
            _ => None,
        }
    }

    /// Figure out index into zone array to get the correct slab allocator for that size.
    fn get_slab(requested_size: usize) -> Slab {
        match requested_size {
            0..=8 => Slab::Base(0),
            9..=16 => Slab::Base(1),
            17..=32 => Slab::Base(2),
            33..=64 => Slab::Base(3),
            65..=128 => Slab::Base(4),
            129..=256 => Slab::Base(5),
            257..=512 => Slab::Base(6),
            513..=1024 => Slab::Base(7),
            1025..=2048 => Slab::Base(8),
            2049..=4096 => Slab::Base(9),
            4097..=ZoneAllocator::MAX_ALLOC_SIZE => Slab::Base(10),
            _ => Slab::Unsupported,
        }
    }

    /// Returns the heap id from the first page of the first slab
    fn heap_id(&self) -> Result<usize, &'static str> {
        self.small_slabs[0].heap_id().ok_or("There were no pages in the heap")
    }

    /// Removes all the pages of `allocator` and adds them to the appropriate lists in this allocator.
    pub fn merge(&mut self, allocator: &mut ZoneAllocator<'a>, heap_id: usize) -> Result<(), &'static str> {
        for size in &ZoneAllocator::BASE_ALLOC_SIZES {
            match ZoneAllocator::get_slab(*size) {
                Slab::Base(idx) => {
                    self.small_slabs[idx].merge(&mut allocator.small_slabs[idx], heap_id)?;
                }
                Slab::Large(_idx) => return Err("AllocationError::InvalidLayout"),
                Slab::Unsupported => return Err("AllocationError::InvalidLayout"),
            }
        }
        Ok(())
    }

    /// Refills the SCAllocator for a given Layout with an ObjectPage.
    ///
    /// # Safety
    /// ObjectPage needs to be emtpy etc.
    pub fn refill(
        &mut self,
        layout: Layout,
        mp: MappedPages,
        heap_id: usize
    ) -> Result<(), &'static str> {
        match ZoneAllocator::get_slab(layout.size()) {
            Slab::Base(idx) => {
                self.small_slabs[idx].refill(mp, heap_id)
            }
            Slab::Large(_idx) => Err("AllocationError::InvalidLayout"),
            Slab::Unsupported => Err("AllocationError::InvalidLayout"),
        }
    }

    /// Returns an ObjectPage from the SCAllocator with the maximum number of empty pages,
    /// if there are more empty pages than the threshold.
    pub fn retrieve_empty_page(
        &mut self
    ) -> Option<MappedPages> {
        let (max_empty_pages, idx) = self.small_slab_with_max_empty_pages();
        if max_empty_pages > ZoneAllocator::SLAB_EMPTY_PAGES_THRESHOLD {
            self.small_slabs[idx].retrieve_empty_page()
        }
        else {
            None
        }
    }

    pub fn exchange_pages_within_heap(&mut self, layout: Layout, heap_id: usize) -> Result<(), &'static str> {
        let mp = self.retrieve_empty_page().ok_or("Couldn't find an empty page to exchange within the heap")?;
        self.refill(layout, mp, heap_id)
    }   

    /// Allocate a pointer to a block of memory described by `layout`.
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, &'static str> {
        match ZoneAllocator::get_slab(layout.size()) {
            Slab::Base(idx) => {
                match self.small_slabs[idx].allocate(layout) {
                    Ok(ptr) => Ok(ptr),
                    Err(_e) => {
                        self.exchange_pages_within_heap(layout, self.heap_id()?)?;
                        self.small_slabs[idx].allocate(layout)
                    }
                }
            }
            Slab::Large(_idx) => Err("AllocationError::InvalidLayout"),
            Slab::Unsupported => Err("AllocationError::InvalidLayout"),
        }
    }

    /// Deallocates a pointer to a block of memory, which was
    /// previously allocated by `allocate`.
    ///
    /// # Arguments
    ///  * `ptr` - Address of the memory location to free.
    ///  * `layout` - Memory layout of the block pointed to by `ptr`.
    pub fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) -> Result<(), &'static str> {
        match ZoneAllocator::get_slab(layout.size()) {
            Slab::Base(idx) => self.small_slabs[idx].deallocate(ptr, layout),
            Slab::Large(_idx) => Err("AllocationError::InvalidLayout"),
            Slab::Unsupported => Err("AllocationError::InvalidLayout"),
        }
    }

    /// The total number of empty pages in this zone allocator
    pub fn empty_pages(&self) -> usize {
        let mut empty_pages = 0;
        for sca in &self.small_slabs {
            empty_pages += sca.empty_slabs.elements;
        }
        empty_pages
    }

    /// Number of empty pages and index of small slab with the maximum number of empty pages
    pub fn small_slab_with_max_empty_pages(&self) -> (usize,usize) {
        let mut max_empty_pages = 0;
        let mut id = 0;
        for i in 0..self.small_slabs.len() {
            let empty_pages = self.small_slabs[i].empty_slabs.elements;
            if empty_pages > max_empty_pages {
                max_empty_pages = empty_pages;
                id = i;
            }
        }
        (max_empty_pages, id)
    }


    // /// Refills the SCAllocator for a given Layout with an ObjectPage.
    // ///
    // /// # Safety
    // /// ObjectPage needs to be emtpy etc.
    // /// 
    // /// Will return an error since we do not use large pages
    // pub unsafe fn refill_large(
    //     &mut self,
    //     layout: Layout,
    //     _new_page: &'a mut LargeObjectPage<'a>,
    // ) -> Result<(), AllocationError> {
    //     match ZoneAllocator::get_slab(layout.size()) {
    //         Slab::Base(_idx) => Err(AllocationError::InvalidLayout),
    //         Slab::Large(_idx) => Err(AllocationError::InvalidLayout),
    //         Slab::Unsupported => Err(AllocationError::InvalidLayout),
    //     }
    // }
}

