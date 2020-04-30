//! A slab allocator implementation for objects less than 8 KiB.
//!
//! # Overview
//!
//! The organization is as follows:
//!
//!  * A `ZoneAllocator` manages many `SCAllocator` and can
//!    satisfy requests for different allocation sizes.
//!  * A `SCAllocator` allocates objects of exactly one size.
//!    It stores the objects and meta-data in the pages represented by 'MappedPages8k' objects.
//!
//! Lastly, it provides an `ObjectPage8k` type that represents the layout of the 8 KiB pages, and the MappedPages8k type
//! which represent memory granted to the heap by the OS. 
//!
//! # Implementing GlobalAlloc
//! See the [global alloc](https://github.com/gz/rust-slabmalloc/tree/master/examples/global_alloc.rs) example.
#![allow(unused_features)]
#![cfg_attr(feature = "unstable", feature(const_fn))]
#![cfg_attr(
    test,
    feature(
        prelude_import,
        test,
        raw,
        c_void_variant,
        core_intrinsics,
        vec_remove_item
    )
)]
#![no_std]
#![crate_name = "slabmalloc"]
#![crate_type = "lib"]

extern crate memory;

mod pages;
mod sc;
mod zone;

pub use pages::*;
pub use sc::*;
pub use zone::*;

#[cfg(test)]
#[macro_use]
extern crate std;
#[cfg(test)]
extern crate test;

#[cfg(test)]
mod tests;

use core::alloc::Layout;
use core::mem;
use core::ptr::{self, NonNull};
use memory::{VirtualAddress, MappedPages, create_mapping, EntryFlags};

use log::{error,trace};

#[cfg(target_arch = "x86_64")]
const CACHE_LINE_SIZE: usize = 64;

// #[cfg(target_arch = "x86_64")]
// const BASE_PAGE_SIZE: usize = 4096;

#[cfg(target_arch = "x86_64")]
#[allow(unused)]
const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;


/// Error that can be returned for `allocation` and `deallocation` requests.
#[derive(Debug)]
pub enum AllocationError {
    /// Can't satisfy the allocation request for Layout because the allocator
    /// does not have enough memory (you may be able to `refill` it).
    OutOfMemory,
    /// Allocator can't deal with the provided size of the Layout.
    InvalidLayout,
}

pub unsafe trait Allocator<'a> {
    fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, &'static str>;
    fn deallocate(&mut self, ptr: NonNull<u8>, layout: Layout) -> Result<(), &'static str>;
    fn refill(
        &mut self,
        layout: Layout,
        mp: MappedPages8k,
    ) -> Result<(), &'static str>;
}

