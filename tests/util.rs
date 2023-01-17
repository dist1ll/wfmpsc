use std::{
    alloc::{AllocError, Allocator, Global, Layout},
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

#[derive(Clone)]
pub struct MockAllocator {
    alloc: Global,
    counter: std::sync::Arc<AtomicU32>,
}

impl MockAllocator {
    pub fn new(counter: std::sync::Arc<AtomicU32>) -> Self {
        MockAllocator {
            alloc: Global {},
            counter,
        }
    }
}

unsafe impl Allocator for MockAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let ptr = self.alloc.allocate(layout);
        eprintln!("steodna");
        self.counter.fetch_add(1, Ordering::Release);
        ptr
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.counter.fetch_sub(1, Ordering::Release);
        self.alloc.deallocate(ptr, layout);
    }
}
