/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![feature(allocator_api)]

use core::fmt::Debug;
use core::hash::Hash;

/// This trait allows you to create thread-local queue handles, as well as
/// consumer handles.
pub trait MpscqUnsafe<const T: usize, const C: usize, const S: usize, const L: usize> {
    /// Returns the head of queue `qid`. Every thread should know their
    /// own qid.
    fn get_producer_handle(&self, qid: u8) -> TLQ<C, L>;

    /// Returns the consumer handle. This is the main interface from which the
    /// consumer reads data and clears the thread-local queues.
    /// NOTE: Only one single thread is allowed to use the consumer handle!
    /// This is a single-consumer(!) queue.
    fn get_consumer_handle(&self) -> ConsumerHandle<T, C, L>;
}

#[derive(Debug)]
pub struct ConsumerHandle<const T: usize, const C: usize, const L: usize> {}
unsafe impl<const T: usize, const C: usize, const L: usize> Send for ConsumerHandle<T, C, L> {}

#[derive(Debug)]
pub struct TLQ<const C: usize, const L: usize> {
    pub head: ThreadLocalHead<C>,
    pub tail: ThreadLocalTail<C>,
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

/// A tail that refers to the queue of a single, specific thread-local queue.
/// The tail may only be modified safely by the consumer!
#[derive(Debug)]
pub struct ThreadLocalTail<const C: usize>(*const u32);
impl<const C: usize> ThreadLocalTail<C> {}

/// A head that may only be modified by exactly one thread! Also:
/// the head references exactly 2^C element, which means the queue's
/// ring buffer needs to have a matching capacity.
#[derive(Debug)]
pub struct ThreadLocalHead<const C: usize>(*mut u32);
impl<const C: usize> ThreadLocalHead<C> {
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
            tails: [<__Tails $ALIGN>]<T>,
            heads: [[<__Head $ALIGN>]; T],
            buffer: [u8; S],
            dealloc: DeallocFn,
        }

        impl<'a,
            const T: usize, const C: usize, const S: usize, const L: usize>
            MpscqUnsafe<T, C, S, L>
            for [<__MPSCQ $ALIGN>]<T, C, S, L> {
                        /// Returns a TLQ handle. The only threads that are allowed to modify
            /// the data behind this TLQ are the consumer thread and the producer
            /// thread with the given qid. Any other access is unsafe and may
            /// lead to critical failure!
            fn get_producer_handle(&self, qid: u8) -> TLQ<C, L> {
                assert!((qid as usize) < T);
                TLQ::<C, L> {
                    tail: ThreadLocalTail(&self.tails.0[qid as usize] as *const u32),
                    head: ThreadLocalHead(&self.heads[qid as usize].0 as *const u32 as *mut u32),
                    buffer: ThreadLocalBuffer::<L>(
                            //
                            (&self.buffer as *const u8 as usize
                                + {qid as usize * {1 << C}}) as *mut [u8; L]
                    ),
                }
            }
            fn get_consumer_handle(&self) -> ConsumerHandle<T, C, L> {
                ConsumerHandle::<T, C, L> {

                }
            }
        }
        // Run custom deallocator!
        impl<const T: usize, const C: usize,
             const S: usize, const L: usize>
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

// Create structs for the following L1 cache alignments:
create_aligned! {32, 64, 128}

// pub static queue: __MPSCQ64<9, 16, 589824, 65536> = __MPSCQ64::<9, 16, 589824, 65536> {
//     tails: __Tails64([0u32; 9]),
//     heads: [__Head64(0); 9],
//     buffer: &[0u8;589824] as *const [u8] as *mut [u8;589824],
//     dealloc: |_, _, _| {},
// };

unsafe impl<const T: usize, const C: usize, const S: usize, const L: usize> Sync
    for __MPSCQ64<T, C, S, L>
{
}

/// Allocate an mpscq with the default system allocator.
#[macro_export]
macro_rules! mpscq {
    (
        bitsize: $capacity:expr,
        producers: $producers:expr,
        l1_cache: $l1_cache:expr
    ) => {{
        let alloc = std::alloc::System {};
        mpscq_alloc!(
            bitsize: $capacity,
            producers: $producers,
            l1_cache: $l1_cache,
            allocator: alloc
        )
    }};
}

/// This module contains all #[no_std] compatible code.
pub mod nostd {

    use core::{
        alloc::{AllocError, Allocator, Layout},
        ptr::NonNull,
    };

    /// Creates a MPSC queue with a custom allocator. The allocator must implement
    /// the core::alloc::Allocator interface. If you want to use the default system
    /// allocator, use [`mpscq!`].
    #[macro_export]
    macro_rules! mpscq_alloc {
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
            let queue = paste::paste! {
                $alloc.allocate(layout).unwrap().as_ptr() as *mut
                [<__MPSCQ $l1>]::<$p, $b,
                    // S = T * 2^C is the global buffer size
                    {$p * (1 << $b)},
                    // L = 2^C  is the thread-local capacity
                    {1 << $b}>
            };
            unsafe { queue.as_ref().unwrap() }
        }};
    }

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
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_dev() {
        let mpscq = mpscq!(
            bitsize: 16,
            producers: 9,
            l1_cache: 128
        );
        let alloc = nostd::FixedAllocStub::<0x2ffff, 0x20000> {};

        let no_std_queue = mpscq_alloc!(
            bitsize: 16,
            producers: 9,
            l1_cache: 128,
            allocator: alloc
        );

        eprintln!("0x{:x}", no_std_queue as *const _ as usize);
    }
}
