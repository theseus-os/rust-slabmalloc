//! A slab allocator implementation for objects less than a page-size (4 KiB or 2MiB).
//!
//! # Overview
//!
//! The organization is as follows:
//!
//!  * A `ZoneAllocator` manages many `SCAllocator` and can
//!    satisfy requests for different allocation sizes.
//!  * A `SCAllocator` allocates objects of exactly one size.
//!    It stores the objects and meta-data in one or multiple `AllocablePage` objects.
//!  * A trait `AllocablePage` that defines the page-type from which we allocate objects.
//!
//! Lastly, it provides two default `AllocablePage` implementations `ObjectPage` and `LargeObjectPage`:
//!  * A `ObjectPage` that is 4 KiB in size and contains allocated objects and associated meta-data.
//!  * A `LargeObjectPage` that is 2 MiB in size and contains allocated objects and associated meta-data.
//!
//!
//! # Implementing GlobalAlloc
//! See the [global alloc](https://github.com/gz/rust-slabmalloc/tree/master/examples/global_alloc.rs) example.
//! 
//! # Theseus 
//! Some changes made for the Theseus OS heap:
//!  * A `ObjectPage8k` that is 8 KiB in size and contains allocated objects and associated meta-data.
//!  * return_page() function which allow the ZoneAllocator to return empty pages on request.
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
use core::fmt;
use core::mem;
use core::ptr::{self, NonNull};
use memory::MappedPages;

use log::{error};

#[cfg(target_arch = "x86_64")]
const CACHE_LINE_SIZE: usize = 64;

// #[cfg(target_arch = "x86_64")]
// const BASE_PAGE_SIZE: usize = 4096;

#[cfg(target_arch = "x86_64")]
#[allow(unused)]
const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

#[cfg(target_arch = "x86_64")]
type VAddr = usize;

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
    // unsafe fn refill_large(
    //     &mut self,
    //     layout: Layout,
    //     new_page: &'a mut LargeObjectPage<'a>,
    // ) -> Result<(), AllocationError>;
    fn refill(
        &mut self,
        layout: Layout,
        mp: MappedPages,
    ) -> Result<(), &'static str>;
}
