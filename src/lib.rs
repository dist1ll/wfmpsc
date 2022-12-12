/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![feature(allocator_api)]

use core::fmt::Debug;
pub use paste::paste;
use std::{
    fmt::Display,
    sync::atomic::{compiler_fence, AtomicU32, Ordering},
};

/// A safe interface to access the MPSC queue, allowing a single
pub trait ConsumerHandle {
    /// Remove a single byte from a producer with the given producer id.
    fn pop_single(&self, pid: usize);

    /// Returns the number of producers
    fn get_producer_count(&self) -> usize;

    /// From the producer given by `pid`, read at most `dst.len()` elements from its local
    /// queue, copies them into destination buffer `dst` and update the queue tail.
    /// Returns the number of elements that could be copied
    fn pop_elements_into(&self, pid: usize, dst: &mut [u8]) -> usize;
}

pub struct ConsumerHandleImpl<const T: usize, const C: usize, const S: usize, const L: usize> {
    tails: RWTails<T, C>,
    heads: [ReadOnlyHead<C>; T],
    buffer: ReadOnlyBuffer<T, S, L>,
}

impl<const T: usize, const C: usize, const S: usize, const L: usize> ConsumerHandle
    for ConsumerHandleImpl<T, C, S, L>
{
    #[inline]
    fn pop_single(&self, pid: usize) {
        eprintln!("popping element from queue: {}", pid);
    }

    fn pop_elements_into(&self, pid: usize, dst: &mut [u8]) -> usize {
        let tail = self.tails.get(pid);
        let data_len = (self.heads[pid].read_atomic_acq() - tail) as usize;
        let write_len = core::cmp::min(data_len, dst.len());
        let tlq_base_ptr = self.buffer.get_tlq_slice(pid);
        let src = ((tlq_base_ptr as usize) + tail as usize) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), write_len);
        }
        self.tails.increment(pid, write_len as u32);
        write_len
    }

    #[inline(always)]
    fn get_producer_count(&self) -> usize {
        return T;
    }
}
unsafe impl<const T: usize, const C: usize, const S: usize, const L: usize> Send
    for ConsumerHandleImpl<T, C, S, L>
{
}

#[derive(Debug)]
pub struct TLQ<const C: usize, const L: usize> {
    pub head: ThreadLocalHead<C>,
    pub tail: ReadOnlyTail<C>,
    pub buffer: ThreadLocalBuffer<L>,
}

impl<const C: usize, const L: usize> TLQ<C, L> {
    /// Pushes a single byte to the buffer. This operation is safe
    /// as long as the buffer is a contiguous block of 2^C bytes.
    #[inline]
    pub fn push_single(&self, byte: u8) {
        unsafe {
            let h = *self.head.0 as usize;
            let offset = (h + 1) & ((1usize << C) - 1);
            (*self.buffer.0)[offset] = byte;
        }
        self.head.increment_by(1);
    }
}
unsafe impl<const C: usize, const S: usize> Send for TLQ<C, S> {}

#[derive(Debug)] // TODO: Add custom debug implementation!
pub struct ThreadLocalBuffer<const L: usize>(*mut [u8; L]);
impl<const L: usize> ThreadLocalBuffer<L> {
    pub fn new(ptr: *mut [u8; L]) -> Self {
        Self { 0: ptr }
    }
}

/// A read & write acces to all tails of the MPSCQ. This may only be accessed
/// and modified by a single consumer.
pub struct RWTails<const T: usize, const C: usize>(*mut [u32; T]);
impl<const T: usize, const C: usize> RWTails<T, C> {
    pub fn new(ptr: *mut [u32; T]) -> Self {
        Self { 0: ptr }
    }
    #[inline(always)]
    pub fn get(&self, pid: usize) -> u32 {
        // TODO: Check that pointer arithmetics don't outperform this
        unsafe { (*self.0)[pid] }
    }
    #[inline(always)]
    pub fn increment(&self, pid: usize, len: u32) {
        // NOTE: We don't need CAS or LL/SC because we are the only thread that's
        // performing STORE operations on this memory address.
        // TODO: Implement the atomic store
        unsafe {
            // Bitmask wrap increment, using bitwidth of queue C
            (*self.0)[pid] = ((*self.0)[pid] + len) & (((1 << C) - 1) as u32);
        }
    }
}

/// A tail that refers to the queue of a single, specific thread-local queue.
/// This is a read-only view! The tail may only be modified safely by the consumer.
#[derive(Debug)]
pub struct ReadOnlyTail<const C: usize>(*mut u32);
impl<const C: usize> ReadOnlyTail<C> {
    pub fn new(ptr: *mut u32) -> Self {
        Self { 0: ptr }
    }
    /// Performs an atomic read on the tail with acquire semantics, as the value 
    /// is expected to be written to from the consumer. 
    #[inline(always)]
    pub fn read_atomic_acq(&self) -> u32 {
        unsafe {
            let atomic = &*(self.0 as *const AtomicU32);
            atomic.load(Ordering::Acquire)
        }
    }
}

/// A head that refers to the queue of a single, specific thread-local queue.
/// This is a read-only view! The tail may only be modified safely by the producer.
#[derive(Debug)]
pub struct ReadOnlyHead<const C: usize>(*mut u32);
impl<const C: usize> ReadOnlyHead<C> {
    pub fn new(ptr: *mut u32) -> Self {
        Self { 0: ptr }
    }
    /// Performs an atomic read on the head with acquire semantics, as the value 
    /// is expected to be written to from the producer.
    #[inline(always)]
    pub fn read_atomic_acq(&self) -> u32 {
        unsafe {
            let atomic = &*(self.0 as *const AtomicU32);
            atomic.load(Ordering::Acquire)
        }
    }
}

/// A read-only view on the entire MPSC queue buffer.
#[derive(Debug)]
pub struct ReadOnlyBuffer<const T: usize, const S: usize, const L: usize>(*mut [u8; S]);
impl<const T: usize, const S: usize, const L: usize> ReadOnlyBuffer<T, S, L> {
    pub fn new(ptr: *mut [u8; S]) -> Self {
        Self { 0: ptr }
    }
    /// Returns raw byte slice that points to the TLQ backing array with the
    /// specified producer id.
    #[inline(always)]
    pub fn get_tlq_slice(&self, pid: usize) -> *mut u8 {
        debug_assert!(
            pid < T,
            "this queue has {} producers, but you selected a too large pid={}",
            T,
            pid
        );
        // TODO: Check for assembly optimizations here
        (self.0 as usize + pid * L) as *mut u8
    }
}

/// A head that may only be modified by exactly one thread! Also:
/// the head references exactly 2^C element, which means the queue's
/// ring buffer needs to have a matching capacity.
#[derive(Debug)]
pub struct ThreadLocalHead<const C: usize>(*mut u32);
impl<const C: usize> ThreadLocalHead<C> {
    pub fn new(ptr: *mut u32) -> Self {
        Self { 0: ptr }
    }
    /// Increments the head pointer, thereby committing the written
    /// bytes to the consumer
    #[inline]
    pub fn increment_by(&self, amount: u32) {
        unsafe {
            // We mask by the capacity of the Queue. We always
            // need to make sure that the queue this offset
            // is referencing always has 2^C capacity
            *self.0 = (*self.0 + amount) & ((1 << C) - 1) as u32;
        }
    }
}

impl<const C: usize> Display for ThreadLocalHead<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        unsafe { write!(f, "{:?}", *self.0) }
    }
}

/// Deallocation function, specifically for MPSC queue.
/// 1st param: pointer to full contiguous byte buffer
/// 2nd param: size of the full buffer (type parameter S)
/// 3rd param: alignment of the buffer, which is the size of the TLQ
///            (type parameter L)
pub type DeallocFn = fn(*mut u8, usize, usize);

/// This macro creates one set of types (Tails, Head, MPSCQ) for every cache
/// alignment. The resulting structs have a naming scheme like __Tails64,
/// which is a 64-byte aligned struct that stores queue tails.
macro_rules! create_aligned {
    ($($ALIGN:literal),+$(,)*) => ($(
    paste::paste!{
        /// Array of cache-aligned queue tails
        #[repr(C, align($ALIGN))]
        #[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct [<__Tails $ALIGN>]<const T: usize>(pub [u32; T]);
        /// Single queue head
        #[repr(C, align($ALIGN))]
        #[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
        pub struct [<__Head $ALIGN>](pub u32);
        /// MPSCQ table that stores tail and head as offsets into TLQs. S is the max
        /// number of elements in all TLQs combined. So S = T * 2^C
        pub struct [<__MPSCQ $ALIGN>]<
            const T: usize, // number of producers
            const C: usize, // bitwidth of queue size
            const S: usize, // size of entire global buffer (T * 2^C)
            const L: usize> // size of thread-local buffer (2^C)
        {
            buffer: [u8; S],
            tails: [<__Tails $ALIGN>]<T>,
            heads: [[<__Head $ALIGN>]; T],
            dealloc: DeallocFn,
        }

        impl<'a,
            const T: usize, const C: usize, const S: usize, const L: usize>
            [<__MPSCQ $ALIGN>]<T, C, S, L> {
            /// Returns a TLQ handle. The only threads that are allowed to modify
            /// the data behind this TLQ are the consumer thread and the producer
            /// thread with the given pid. Any other access is unsafe and may
            /// lead to critical failure!
            pub fn get_producer_handle(&mut self, pid: u8) -> TLQ<C, L> {
                assert!((pid as usize) < T);
                TLQ::<C, L> {
                    tail: ReadOnlyTail::new(&mut self.tails.0[pid as usize] as *mut u32),
                    head: ThreadLocalHead::new(&mut self.heads[pid as usize].0 as *mut u32),
                    buffer: ThreadLocalBuffer::<L>::new(
                            (&mut self.buffer as *mut u8 as usize
                                + {pid as usize * {1 << C}}) as *mut [u8; L]
                    ),
                }
            }
            /// Returns a consumer handle. This allows a single thread to pop data
            /// from the other producers in a safe way.
            pub fn get_consumer_handle(&mut self) -> ConsumerHandleImpl<T, C, S, L> {
                let mut heads: [ReadOnlyHead<C>; T] = unsafe { core::mem::zeroed() };
                for i in 0..T {
                    heads[i] = ReadOnlyHead::new(&mut self.heads[i].0 as *mut u32);
                }
                ConsumerHandleImpl::<T, C, S, L> {
                        tails: RWTails::<T, C>::new(&mut self.tails.0 as *mut [u32; T]),
                        heads,
                        buffer: ReadOnlyBuffer::<T, S, L>::new(&mut self.buffer as *mut [u8; S])
                }
            }
        }
        // Run custom deallocator!
        impl<const T: usize, const C: usize, const S: usize, const L: usize>
            Drop for [<__MPSCQ $ALIGN>]<T, C, S, L>
        {
            fn drop(&mut self) {
                let f = self.dealloc;
                f(&mut self.buffer as *mut u8, S, L);
            }
        }
    }
    )*)
}

impl<const T: usize, const C: usize, const S: usize, const L: usize> Display
    for __MPSCQ128<T, C, S, L>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "heads: ")?;
        for (idx, h) in self.heads.iter().enumerate() {
            write!(f, "[#{}: {}] ", idx, h.0)?;
        }
        Ok(())
    }
}

// Create types for common cache alignments
create_aligned! {32, 64, 128}

/// Creates an MPSC queue using the standard allocator.
///
/// Parameters:
///  - bitsize(usize): number of bits of queue capacity (1 <= b <= 32)
///  - producers(usize): number of concurrent producers, usize
///  - l1_cache(usize): byte size of L1 cache
///
/// If you want to use a custom allocator, use [`queue_alloc!`].
#[macro_export]
macro_rules! queue {
    (
        bitsize: $b:expr,
        producers: $p:expr,
        l1_cache: $l1:expr
    ) => {{
        if $p * 4 > $l1 {
            panic!("Too many producers! Maximum is L1_CACHE / 4. TODO");
        }
        use core::alloc::Layout;
        let size = $l1 + ($p * $l1) + $p * (1 << $b);
        let align = 1 << $b;
        let layout = Layout::from_size_align(size, align).unwrap();
        let queue = unsafe {
            wfmpsc::paste! {
                std::alloc::alloc(layout) as *mut
                wfmpsc::[<__MPSCQ $l1>]::<$p, $b,
                    // S = T * 2^C is the global buffer size
                    {$p * (1 << $b)},
                    // L = 2^C  is the thread-local capacity
                    {1 << $b}>
            }
        };
        unsafe { queue.as_mut().unwrap() }
    }};
}

/// Creates a MPSC queue with a custom allocator.
///
/// Parameters:
///  - bitsize(usize): number of bits of queue capacity (1 <= b <= 32)
///  - producers(usize): number of concurrent producers, usize
///  - l1_cache(usize): byte size of L1 cache
///  - allocator(Allocator): an allocator object, using Rust alloc API.
///
/// The allocator must implement the core::alloc::Allocator interface. If
/// you want to use the default system allocator, use [`queue!`].
#[macro_export]
macro_rules! queue_alloc {
    (
        bitsize: $b:expr,
        producers: $p:expr,
        l1_cache: $l1:expr,
        allocator: $alloc:expr
    ) => {{
        if $p * 4 > $l1 {
            panic!("Too many producers! Maximum is L1_CACHE / 4. TODO");
        }
        use core::alloc::{Allocator, Layout};
        let size = $l1 + ($p * $l1) + $p * (1 << $b);
        let align = 1 << $b;
        let layout = Layout::from_size_align(size, align).unwrap();
        let queue = wfmpsc::paste! {
            $alloc.allocate(layout).unwrap().as_ptr() as *mut
            wfmpsc::[<__MPSCQ $l1>]::<$p, $b,
                // S = T * 2^C is the global buffer size
                {$p * (1 << $b)},
                // L = 2^C  is the thread-local capacity
                {1 << $b}>
        };
        unsafe { queue.as_mut().unwrap() }
    }};
}

use core::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};

/// A stub allocator that always returns them same given memory region.
/// The region is given by START and SIZE parameter (address and length
/// of the memory region)
pub struct FixedAllocStub<const START: usize, const SIZE: usize>;

unsafe impl<const START: usize, const SIZE: usize> Allocator for FixedAllocStub<START, SIZE> {
    fn allocate(&self, _: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match NonNull::new(core::ptr::slice_from_raw_parts_mut(START as *mut u8, SIZE)) {
            None => Err(AllocError {}),
            Some(ptr) => Ok(ptr),
        }
    }
    unsafe fn deallocate(&self, _: NonNull<u8>, _: Layout) {}
}
