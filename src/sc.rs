//! A SCAllocator that can allocate fixed size objects.

use crate::*;

/// A genius(?) const min()
///
/// # What this does
/// * create an array of the two elements you want to choose between
/// * create an arbitrary boolean expression
/// * cast said expresison to a usize
/// * use that value to index into the array created above
///
/// # Source
/// https://stackoverflow.com/questions/53619695/calculating-maximum-value-of-a-set-of-constant-expressions-at-compile-time
#[cfg(feature = "unstable")]
const fn cmin(a: usize, b: usize) -> usize {
    [a, b][(a > b) as usize]
}

/// The boring variant of min (not const).
#[cfg(not(feature = "unstable"))]
fn cmin(a: usize, b: usize) -> usize {
    core::cmp::min(a, b)
}

/// A slab allocator allocates elements of a fixed size.
///
/// It maintains three internal lists of objects that implement `AllocablePage`
/// from which it can allocate memory.
///
///  * `empty_slabs`: Is a list of pages that the SCAllocator maintains, but
///    has 0 allocations in them, these can be given back to a requestor in case
///    of reclamation.
///  * `slabs`: A list of pages partially allocated and still have room for more.
///  * `full_slabs`: A list of pages that are completely allocated.
///
/// On allocation we allocate memory from `slabs`, however if the list is empty
/// we try to reclaim a page from `empty_slabs` before we return with an out-of-memory
/// error. If a page becomes full after the allocation we move it from `slabs` to
/// `full_slabs`.
///
/// Similarly, on dealloaction we might move a page from `full_slabs` to `slabs`
/// or from `slabs` to `empty_slabs` after we deallocated an object.
///
/// If an allocation returns `OutOfMemory` a client using SCAllocator can refill
/// it using the `refill` function.
pub struct SCAllocator {
    /// Maximum possible allocation size for this `SCAllocator`.
    pub(crate) size: usize,
    /// Keeps track of succeeded allocations.
    pub(crate) allocation_count: usize,
    /// max objects per page
    pub(crate) obj_per_page: usize,
    /// List of empty ObjectPages (nothing allocated in these).
    pub(crate) empty_slabs: PageList,
    /// List of partially used ObjectPage (some objects allocated but pages are not full).
    pub(crate) slabs: PageList,
    /// List of full ObjectPages (everything allocated in these don't need to search them).
    pub(crate) full_slabs: PageList,
}

/// Creates an instance of a scallocator, we do this in a macro because we
/// re-use the code in const and non-const functions
macro_rules! new_sc_allocator {
    ($size:expr) => {
        SCAllocator {
            size: $size,
            allocation_count: 0,
            obj_per_page: cmin((MappedPages8k::SIZE - MappedPages8k::METADATA_SIZE) / $size, 8 * 64),
            empty_slabs: PageList::new(),
            slabs: PageList::new(),
            full_slabs: PageList::new(),
        }
    };
}

impl SCAllocator {
    const _REBALANCE_COUNT: usize = 10_000;

    /// Create a new SCAllocator.
    #[cfg(feature = "unstable")]
    pub const fn new(size: usize) -> SCAllocator {
        new_sc_allocator!(size)
    }

    #[cfg(not(feature = "unstable"))]
    pub fn new(size: usize) -> SCAllocator {
        new_sc_allocator!(size)
    }

    /// Returns the maximum supported object size of this allocator.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Add a new ObjectPage.
    fn insert_partial_slab(&mut self, new_head: MappedPages8k) {
        self.slabs.insert_front(new_head);
    }

    /// Add page to empty list.
    fn insert_empty(&mut self, new_head: MappedPages8k) {
        // assert_eq!(
        //     new_head as *const MappedPages8k as usize % MappedPages8k::SIZE,
        //     0,
        //     "Inserted page is not aligned to page-size."
        // );
        self.empty_slabs.insert_front(new_head);
    }

    fn remove_empty(&mut self) -> Option<MappedPages8k> {
        self.empty_slabs.pop()
    }

    fn remove_partial(&mut self) -> Option<MappedPages8k> {
        self.slabs.pop()
    }

    fn remove_full(&mut self) -> Option<MappedPages8k> {
        self.full_slabs.pop()
    }
    
    // /// Since `dealloc` can not reassign pages without requiring a lock
    // /// we check slabs and full slabs periodically as part of `alloc`
    // /// and move them to the empty or partially allocated slab lists.
    // pub(crate) fn check_page_assignments(&mut self) {
    //     for slab_page in self.full_slabs.iter_mut() {
    //         if !slab_page.is_full() {
    //             // We need to move it from self.full_slabs -> self.slabs
    //             // trace!("move {:p} full -> partial", slab_page);
    //             self.move_full_to_partial(slab_page);
    //         }
    //     }

    //     for slab_page in self.slabs.iter_mut() {
    //         if slab_page.is_empty(self.obj_per_page) {
    //             // We need to move it from self.slabs -> self.empty_slabs
    //             // trace!("move {:p} partial -> empty", slab_page);
    //             self.move_to_empty(slab_page);
    //         }
    //     }
    // }

    /// Move a page with the starting address `page_addr` from `slabs` to `empty_slabs`.
    fn move_to_empty(&mut self, page_addr: VirtualAddress) {
        debug_assert!(self.slabs.contains(page_addr));
        debug_assert!(
            !self.empty_slabs.contains(page_addr),
            "Page {:p} already in empty_slabs",
            page_addr
        );
        let page_to_move = self.slabs.remove_from_list(page_addr).unwrap();
        self.empty_slabs.insert_front(page_to_move);

        debug_assert!(!self.slabs.contains(page_addr));
        debug_assert!(self.empty_slabs.contains(page_addr));
    }

    /// Move a page with the starting address `page_addr` from `slab` to `full_slabs`.
    fn move_partial_to_full(&mut self, page_addr: VirtualAddress) { 
        debug_assert!(self.slabs.contains(page_addr));
        debug_assert!(!self.full_slabs.contains(page_addr));

        let page_to_move = self.slabs.remove_from_list(page_addr).unwrap();
        self.full_slabs.insert_front(page_to_move);

        debug_assert!(!self.slabs.contains(page_addr));
        debug_assert!(self.full_slabs.contains(page_addr));
    }

    /// Move a page with the starting address `page_addr` from `full_slabs` to `slab`.
    fn move_full_to_partial(&mut self, page_addr: VirtualAddress) {
        debug_assert!(!self.slabs.contains(page_addr));
        debug_assert!(self.full_slabs.contains(page_addr));

        let page_to_move = self.full_slabs.remove_from_list(page_addr).unwrap();
        self.slabs.insert_front(page_to_move);

        debug_assert!(self.slabs.contains(page_addr));
        debug_assert!(!self.full_slabs.contains(page_addr));
    }

    /// Tries to allocate a block of memory with respect to the `layout`.
    /// Searches within already allocated slab pages, if no suitable spot is found
    /// will try to use a page from the empty page list.
    ///
    /// # Arguments
    ///  * `sc_layout`: This is not the original layout but adjusted for the
    ///     SCAllocator size (>= original).
    fn try_allocate_from_pagelist(&mut self, sc_layout: Layout) -> *mut u8 {
        // TODO: Do we really need to check multiple slab pages (due to alignment)
        // If not we can get away with a singly-linked list and have 8 more bytes
        // for the bitfield in an ObjectPage.

        for slab_page in self.slabs.iter_mut() {
            let ptr = slab_page.allocate(sc_layout);
            if !ptr.is_null() {
                if slab_page.is_full() {
                    // trace!("move {:p} partial -> full", slab_page);
                    self.move_partial_to_full(slab_page.start_address());
                }
                self.allocation_count += 1;
                return ptr;
            } else {
                continue;
            }
        }

        // // Periodically rebalance page-lists (since dealloc can't do it for us)
        // if self.allocation_count % SCAllocator::<P>::REBALANCE_COUNT == 0 {
        //     self.check_page_assignments();
        // }

        ptr::null_mut()
    }


    /// removes all of the pages from the lists of `allocator` and adds them to this allocator.
    pub fn merge(&mut self, allocator: &mut SCAllocator, heap_id: usize) -> Result<(), &'static str> {
        while !allocator.empty_slabs.is_empty() {
            match allocator.remove_empty() {
                Some(mut new_head) =>{
                    new_head.set_heap_id(heap_id);
                    self.empty_slabs.insert_front(new_head)
                }
                None => {
                    break;
                }
            }
        }

        while !allocator.slabs.is_empty() {
            match allocator.remove_partial() {
                Some(mut new_head) =>{
                    new_head.set_heap_id(heap_id);
                    self.slabs.insert_front(new_head)
                }
                None => {
                    break;
                }
            }
        }

        while !allocator.full_slabs.is_empty() {
            match allocator.remove_full() {
                Some(mut new_head) =>{
                    new_head.set_heap_id(heap_id);
                    self.full_slabs.insert_front(new_head)
                }
                None => {
                    break;
                }
            }
        }

        Ok(())

    }

    // /// Creates an allocable page given a MappedPages object and returns a reference to the allocable page.
    // /// The MappedPages object is stored within the metadata of the allocable page.
    // fn create_allocable_page(mp: MappedPages8k, heap_id: usize) -> Result<&'a mut MappedPages8k, &'static str> {
    //     // let vaddr = mp.start_address().value();

    //     // let mut mp_8k = MappedPages8k::new(mp, heap_id)?;
    //     let obj_page = mp.as_ObjectPage8k_mut();
    //     obj_page.clear_metadata();

    //     // // create page and store the MappedPages object
    //     // let page = MappedPages8k::new(mp, heap_id)?;
    //     // let page_ref: &'a mut P = unsafe { core::mem::transmute(vaddr) } ; // not unsafe because the allocable page was only create by a mapped page that fit the criteria
    //     // unsafe { (page_ref as *mut P).write(page); }

    //     // Ok(page_ref) 
    // }

    /// Refill the SCAllocator
    pub fn refill(&mut self,mut mp: MappedPages8k, heap_id: usize) -> Result<(), &'static str> {
        // let page = Self::create_allocable_page(mp, heap_id)?;
        mp.clear_metadata();
        mp.bitfield_mut().initialize(self.size, MappedPages8k::SIZE - MappedPages8k::METADATA_SIZE);
        mp.set_heap_id(heap_id);
        // trace!("adding page to SCAllocator {:p}", page);
        self.insert_empty(mp);

        Ok(())
    }

    /// Returns an empty page from the allocator if available.
    /// It removes the MappedPages object from the heap pages where it is stored.
    pub fn retrieve_empty_page(&mut self) -> Option<MappedPages8k> {
        self.remove_empty()
    }

    /// Allocates a block of memory descriped by `layout`.
    ///
    /// Returns a pointer to a valid region of memory or an
    /// AllocationError.
    ///
    /// The function may also move around pages between lists
    /// (empty -> partial or partial -> full).
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, &'static str> {
        // trace!(
        //     "SCAllocator({}) is trying to allocate {:?}, {}",
        //     self.size,
        //     layout, 
        //     MappedPages8k::SIZE - CACHE_LINE_SIZE
        // );

        assert!(layout.size() <= self.size);
        assert!(self.size <= (MappedPages8k::SIZE - CACHE_LINE_SIZE));
        let new_layout = unsafe { Layout::from_size_align_unchecked(self.size, layout.align()) };
        assert!(new_layout.size() >= layout.size());

        let ptr = {
            // Try to allocate from partial slabs,
            // if we fail check if we have empty pages and allocate from there
            let ptr = self.try_allocate_from_pagelist(new_layout);
            if ptr.is_null() && self.empty_slabs.head.is_some() {
                // Re-try allocation in empty page
                let mut empty_page = self.empty_slabs.pop().expect("We checked head.is_some()");
                debug_assert!(!self.empty_slabs.contains(empty_page.start_address()));

                let ptr = empty_page.allocate(layout);
                debug_assert!(!ptr.is_null(), "Allocation must have succeeded here.");

                // trace!(
                //     "move {:#X} empty -> partial empty count {}",
                //     empty_page.start_address(),
                //     self.empty_slabs.elements
                // );
                // Move empty page to partial pages
                self.insert_partial_slab(empty_page);
                ptr
            } else {
                ptr
            }
        };

        let res = NonNull::new(ptr).ok_or("AllocationError::OutOfMemory");

        // if !ptr.is_null() {
        //     trace!(
        //         "SCAllocator({}) allocated ptr=0x{:x}",
        //         self.size,
        //         ptr as usize
        //     );
        // }

        res
    }

    /// Deallocates a previously allocated `ptr` described by `Layout`.
    ///
    /// May return an error in case an invalid `layout` is provided.
    /// The function may also move internal slab pages between lists partial -> empty
    /// or full -> partial lists.
    pub fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) -> Result<(), &'static str> {
        assert!(layout.size() <= self.size);
        assert!(self.size <= (MappedPages8k::SIZE - CACHE_LINE_SIZE));
        // trace!(
        //     "SCAllocator({}) is trying to deallocate ptr = {:p} layout={:?} P.size= {}",
        //     self.size,
        //     ptr,
        //     layout,
        //     MappedPages8k::SIZE
        // );

        let page = (ptr.as_ptr() as usize) & !(MappedPages8k::SIZE - 1) as usize;
        let page_addr = VirtualAddress::new(page)?;

        // Figure out which page we are on and construct a reference to it
        // TODO: The linked list will have another &mut reference
        let slab_page = self.slabs.iter_mut().find(|mp| mp.start_address() == page_addr).or_else(|| self.full_slabs.iter_mut().find(|mp| mp.start_address() == page_addr));//.expect("The page is not in the full or partial slabs!");
        
        // self.slabs.print();
        // self.empty_slabs.print();
        // self.full_slabs.print();
        // loop{}
        let new_layout = unsafe { Layout::from_size_align_unchecked(self.size, layout.align()) };


        let slab_page = unsafe { mem::transmute::<VAddr, &mut ObjectPage8k>(page) };
        let new_layout = unsafe { Layout::from_size_align_unchecked(self.size, layout.align()) };

        let slab_page_was_full = slab_page.is_full();
        let ret = slab_page.deallocate(ptr, new_layout);
        debug_assert!(ret.is_ok(), "Slab page deallocate won't fail at the moment");

        if slab_page.is_empty(self.obj_per_page) {
            // We need to move it from self.slabs -> self.empty_slabs
            // trace!("move {:p} {:#X} partial -> empty", slab_page, VirtualAddress::new(page)?);
            self.move_to_empty(VirtualAddress::new(page)?);
        } else if slab_page_was_full {
            // We need to move it from self.full_slabs -> self.slabs
            // trace!("move {:p} full -> partial", slab_page);
            self.move_full_to_partial(VirtualAddress::new(page)?);
        }

        ret

        // Ok(())
    }
}
