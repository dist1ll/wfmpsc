/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![no_std]
#![feature(allocator_api)]

use core::fmt::Debug;
use core::{
    fmt::Display,
    sync::atomic::{AtomicU16, AtomicU32, Ordering},
};

use core::mem::{size_of, MaybeUninit};
use core::ptr::{addr_of, addr_of_mut, copy_nonoverlapping};
use core::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};

#[cfg(feature = "alloc")]
extern crate alloc;

/* Aliases for queue's atomic integer type */
/// Compressed tail index
type utail = u16;
/// Canocial size of indices (i.e. precise addressing of memory)
type udefault = u16;
type AtomicTail = AtomicU16;
type AtomicHead = AtomicU16;

#[inline(always)]
pub const fn compress(tail: udefault, queue_bitwidth: usize) -> utail {
    (tail >> chunk_width(queue_bitwidth)) as utail
}
#[inline(always)]
pub const fn decompress(tail: utail, queue_bitwidth: usize) -> udefault {
    (tail as udefault) << chunk_width(queue_bitwidth)
}
/// Width of chunks, given a queue's bitwidth. Any queue smaller than 16-bits
/// can be precisely addressed by a 16-bit utail. If the queue is larger, then
/// any remaining bits determine the size of chunks needed to compress the ptr.
/// E.g. a 20-bit queue would divide the queue into 16-byte chunks
#[inline(always)]
pub const fn chunk_width(queue_bitwidth: usize) -> usize {
    if queue_bitwidth <= 16 {
        return 0;
    }
    queue_bitwidth - 16
}

/// A safe interface to access the MPSC queue, allowing a single
pub trait ConsumerHandle {
    /// Returns the number of producers
    fn get_producer_count(&self) -> usize;

    /// From the producer given by `pid`, read at most `dst.len()` elements
    /// from its local queue, copies them into destination buffer `dst` and
    /// update the queue tail.Returns the number of elements that were copied
    fn pop_into(&self, pid: usize, dst: &mut [u8]) -> usize;
}

pub struct ConsumerHandleImpl<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
> {
    tails: RWTails<T, C>,
    heads: [ReadOnlyHead<C>; T],
    buffer: ReadOnlyBuffer<T, S, L>,
    refcount: *const AtomicU32,
    mpscq_ptr: *const __MPSCQ<T, C, S, L, A>,
}

impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > ConsumerHandle for ConsumerHandleImpl<T, C, S, L, A>
{
    fn pop_into(&self, pid: usize, dst: &mut [u8]) -> usize {
        let tail = self.tails.read_atomic(pid, Ordering::Relaxed);
        // FIXME: Understand why relaxed causes a data race in miri.
        // Isn't there a data dependence between this head and the
        // first memcpy below? Why do we need to synchronize here?
        // A stale head should not cause data race issues.
        let head = self.heads[pid].read_atomic(Ordering::Acquire);
        let queue_element_count = queue_element_count::<C>(head, tail) as usize;

        // actual length of elements to be copied
        let len = core::cmp::min(queue_element_count, dst.len());
        let target_wrap = (tail + len as udefault) & fmask_udefault::<C>();
        let src =
            (self.buffer.tlq_base_ptr(pid) as usize + tail as usize) as *mut u8;

        if target_wrap >= tail {
            unsafe {
                copy_nonoverlapping(src, dst.as_mut_ptr(), len);
            }
        } else {
            let split_len = L - tail as usize;
            unsafe {
                copy_nonoverlapping(src, dst.as_mut_ptr(), split_len);
                copy_nonoverlapping(
                    self.buffer.tlq_base_ptr(pid),
                    // this is guaranteed to work because we made sure that
                    // the total number of items copied is min(elems, dst.len)
                    (dst.as_mut_ptr() as usize + split_len) as *mut _,
                    len - split_len,
                );
            }
        }
        self.tails.store_atomic(pid, target_wrap, Ordering::Release);
        len
    }

    #[inline(always)]
    fn get_producer_count(&self) -> usize {
        return T;
    }
}
unsafe impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > Send for ConsumerHandleImpl<T, C, S, L, A>
{
}

/// A producer handle. Use this to push data into the MPSC queue that can be
/// read by the consumer.
#[derive(Debug)]
pub struct TLQ<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
> {
    pub head: RWHead<C>,
    pub tail: ReadOnlyTail<C>,
    pub buffer: ThreadLocalBuffer<L>,
    refcount: *const AtomicU32,
    mpscq_ptr: *const __MPSCQ<T, C, S, L, A>,
}

impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > TLQ<T, C, S, L, A>
{
    /// Pushes a byte slice to the buffer. Performs a partial write if
    /// the queue is full. Returns the number of bytes written to the queue.
    ///
    /// This operation is sound as long as the TLQ's backing array is a
    /// contiguous block of 2^C bytes.
    ///
    /// TODO: Two functions: push and try_push. Both of them take an argument
    /// for failure behaviour. Something like:
    /// ```
    /// pub enum FailureBehavior {
    ///     Noop,
    ///     PartialWrite,
    /// }
    /// ```
    pub fn push(&self, byte: &[u8]) -> usize {
        // relaxed ordering is fine, because a stale read from tail
        // does not cause a data race.
        let head = self.head.read_atomic(Ordering::Relaxed);
        // FIXME: Same question as with pop_into()
        let tail = self.tail.read_atomic(Ordering::Acquire);
        let capacity = queue_leftover_capacity::<C>(head, tail);

        // Limit arg bytes to queue size - 1 (because we can't distinguish)
        // between full and empty queues if its filled entirely.
        // OVERFLOW: capacity can never be zero, because head and tail are only
        // allowed to be equal if the queue is empty.
        let len =
            core::cmp::min(capacity, byte.len() as udefault + 1) as usize - 1;
        assert!(capacity != 0);
        if ((head + 1) & fmask_udefault::<C>() == tail as udefault) || len == 0
        {
            return 0;
        }

        let target_wrap = (head + len as udefault) & fmask_udefault::<C>();
        let src = byte.as_ptr() as usize;
        let dst = self.buffer.0 as usize + head as usize;
        if target_wrap >= head {
            // only make one copy
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src as *const u8,
                    dst as *mut u8,
                    len as usize,
                );
            }
        }
        // TODO: place this path into a seperate function, as its less likely.
        else {
            // we have two make two copies
            unsafe {
                let split_len = L - head as usize;
                core::ptr::copy_nonoverlapping(
                    src as *const u8,
                    dst as *mut u8,
                    split_len,
                );
                core::ptr::copy_nonoverlapping(
                    (src + split_len) as *const u8,
                    self.buffer.0 as *mut u8,
                    len - split_len,
                );
            }
        }
        // We don't want to increment the head before the memcpy completes!
        self.head.store_atomic(
            (head + len as udefault) & fmask_udefault::<C>(),
            Ordering::Release,
        );
        len
    }
}

/// Computes the remaining capacity of a ring buffer for a given head index,
/// tail index and bit width of the queue (i.e. the total capacity).
#[inline(always)]
pub fn queue_leftover_capacity<const C: usize>(
    h: udefault,
    t: udefault,
) -> udefault {
    match h >= t {
        true => (1 << C) - (h - t),
        false => t - h,
    }
}

/// Computes the element count of a ring buffer for a given head index, tail
/// index and bit width of the queue (i.e. the total capacity).
#[inline(always)]
pub fn queue_element_count<const C: usize>(
    h: udefault,
    t: udefault,
) -> udefault {
    (1 << C) - queue_leftover_capacity::<C>(h, t)
}

unsafe impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > Send for TLQ<T, C, S, L, A>
{
}

#[derive(Debug)] // TODO: Add custom debug implementation!
pub struct ThreadLocalBuffer<const L: usize>(*mut [u8; L]);
impl<const L: usize> ThreadLocalBuffer<L> {
    pub fn new(ptr: *mut [u8; L]) -> Self {
        Self { 0: ptr }
    }
}

/// A read & write acces to all tails of the MPSCQ. This may only be accessed
/// and modified by a single consumer.
pub struct RWTails<const T: usize, const C: usize>(*const [AtomicTail; T]);
impl<const T: usize, const C: usize> RWTails<T, C> {
    pub fn new(ptr: *const [AtomicTail; T]) -> Self {
        Self { 0: ptr }
    }
    #[inline(always)]
    pub fn read_atomic(&self, pid: usize, ord: Ordering) -> udefault {
        // TODO: Check that pointer arithmetics don't outperform this
        unsafe {
            let base_addr = self.0 as *mut AtomicTail as usize;
            let pid_pointer =
                base_addr + pid * core::mem::size_of::<AtomicTail>();
            let atomic = &*(pid_pointer as *const AtomicTail);
            let l = atomic.load(ord);
            decompress(l, C)
        }
    }
    /// Increment the tail atomically using release semantics.
    /// Automatically compresses the tail into the right size.
    #[inline(always)]
    pub fn store_atomic(&self, pid: usize, val: udefault, ord: Ordering) {
        let val = compress(val, C);
        // NOTE: We don't need CAS or LL/SC because we are the only thread
        // that's performing STORE operations on this memory address.
        unsafe {
            // Bitmask wrap increment, using bitwidth of queue C
            let atomic = &*((self.0 as usize + pid * size_of::<AtomicTail>())
                as *mut AtomicTail);
            atomic.store(val, ord);
        }
    }
}

/// A tail that refers to the queue of a single, specific thread-local queue.
/// This is a read-only view! The tail may only be modified by the consumer.
#[derive(Debug)]
pub struct ReadOnlyTail<const C: usize>(*const AtomicTail);
impl<const C: usize> ReadOnlyTail<C> {
    pub fn new(ptr: *const AtomicTail) -> Self {
        Self { 0: ptr }
    }
    /// Performs an atomic read on the tail with given memory ordering.
    /// Automatically decompresses the index
    #[inline(always)]
    pub fn read_atomic(&self, ord: Ordering) -> udefault {
        unsafe {
            let atomic = &*self.0;
            let l = atomic.load(ord);
            decompress(l, C)
        }
    }
}

/// A head that refers to the queue of a single, specific thread-local queue.
/// This is a read-only view! The tail may only be modified by the producer.
#[derive(Debug)]
pub struct ReadOnlyHead<const C: usize>(*const AtomicHead);
impl<const C: usize> ReadOnlyHead<C> {
    pub fn new(ptr: *const AtomicHead) -> Self {
        Self { 0: ptr }
    }
    /// Performs an atomic read on the head with given ordering semantics.
    #[inline(always)]
    pub fn read_atomic(&self, ord: Ordering) -> udefault {
        unsafe {
            let atomic = &*self.0;
            atomic.load(ord)
        }
    }
}

/// A read-only view on the entire MPSC queue buffer.
#[derive(Debug)]
pub struct ReadOnlyBuffer<const T: usize, const S: usize, const L: usize>(
    *mut [u8; S],
);
impl<const T: usize, const S: usize, const L: usize> ReadOnlyBuffer<T, S, L> {
    pub fn new(ptr: *mut [u8; S]) -> Self {
        Self { 0: ptr }
    }
    /// Returns pointer to byte arr that points to the TLQ backing array with
    /// the specified producer id.
    #[inline(always)]
    pub fn tlq_base_ptr(&self, pid: usize) -> *mut u8 {
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

/// Read & Write access to a TLQ head. May only be modified by a single
/// producer. A head that may only be modified by exactly one thread!
/// Also: the head references exactly 2^C element, which means the queue's
/// ring buffer needs to have a matching capacity.
#[derive(Debug)]
pub struct RWHead<const C: usize>(*const AtomicHead);
impl<const C: usize> RWHead<C> {
    pub fn new(ptr: *const AtomicHead) -> Self {
        Self { 0: ptr }
    }
    /// Increments the head pointer, thereby committing the written
    /// bytes to the consumer
    #[inline]
    pub fn store_atomic(&self, val: udefault, ord: Ordering) {
        unsafe {
            let atomic = &*self.0;
            atomic.store(val, ord);
        }
    }
    /// Performs an atomic read on the head with given ordering.
    #[inline(always)]
    pub fn read_atomic(&self, ord: Ordering) -> udefault {
        unsafe {
            let atomic = &*self.0;
            atomic.load(ord)
        }
    }
}

impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > Display for TLQ<T, C, S, L, A>
{
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> Result<(), core::fmt::Error> {
        write!(
            f,
            "Head(val): {}\nHead(addr): 0x{:x}\nBuffer: {}",
            self.head, self.head.0 as usize, self.buffer
        )
    }
}

impl<const L: usize> Display for ThreadLocalBuffer<L> {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> Result<(), core::fmt::Error> {
        unsafe {
            let arr = *self.0;
            write!(f, "[ ")?;
            for i in 0..(L - 1) {
                write!(f, "{:?} ", arr[i])?;
            }
            write!(f, "]")?;
        }
        Ok(())
    }
}
impl<const C: usize> Display for RWHead<C> {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> Result<(), core::fmt::Error> {
        unsafe { write!(f, "{:?}", *self.0) }
    }
}

/// Deallocation function, specifically for MPSC queue.
/// 1st param: pointer to full contiguous byte buffer
/// 2nd param: size of the full buffer (type parameter S)
/// 3rd param: alignment of the buffer, which is the size of the TLQ
///            (type parameter L)
pub type DeallocFn = fn(*mut u8, usize, usize);
fn __dummy(ptr: *mut u8, size: usize, align: usize) {}

pub trait ThreadSafeAlloc: Allocator + Clone + Send {}
impl<T: Allocator + Clone + Send> ThreadSafeAlloc for T {}

/// Array of cache-aligned queue tails
#[cfg_attr(cache_line = "32", repr(C, align(32)))]
#[cfg_attr(cache_line = "64", repr(C, align(64)))]
#[cfg_attr(cache_line = "128", repr(C, align(128)))]
#[derive(Debug)]
pub struct __Tails<const T: usize>(pub [AtomicTail; T]);

/// Single queue head
#[cfg_attr(cache_line = "32", repr(C, align(32)))]
#[cfg_attr(cache_line = "64", repr(C, align(64)))]
#[cfg_attr(cache_line = "128", repr(C, align(128)))]
#[derive(Debug)]
pub struct __Head(pub AtomicHead);

/// MPSCQ table that stores tail and head as offsets into TLQs. S is the max
/// number of elements in all TLQs combined. So S = T * 2^C
#[repr(C, align(128))]
pub struct __MPSCQ<
    const T: usize, // number of producers
    const C: usize, // bitwidth of queue size
    const S: usize, // size of entire global buffer (T * 2^C)
    const L: usize, // size of the thread-local buffer (2 ^ C)
    A: ThreadSafeAlloc,
> {
    buffer: [u8; S],
    tails: __Tails<T>,
    heads: [__Head; T],
    refcount: AtomicU32,
    /// SAFETY: `__MPSCQ::alloc` is only allowed to be `None` if the queue was
    /// allocated with [`std::alloc::allocate`] or the GlobalAllocator. Use the
    /// [`wfmpsc::queue!`] macro for creating a valid __MPSCQ object.
    alloc: Option<A>,
}

impl<
        'a,
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > __MPSCQ<T, C, S, L, A>
{
    pub const fn layout() -> Layout {
        let size = core::mem::size_of::<Self>();
        let align = core::mem::align_of::<Self>();
        unsafe { Layout::from_size_align_unchecked(size, align) }
    }
}

/// Returns a producer handle
fn prod_handle<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    ptr: *mut __MPSCQ<T, C, S, L, A>,
    pid: u8,
) -> TLQ<T, C, S, L, A> {
    assert!((pid as usize) < T);
    let ret = TLQ::<T, C, S, L, A> {
        tail: ReadOnlyTail::new(unsafe {
            addr_of_mut!((*ptr).tails.0[pid as usize])
        }),
        head: RWHead::new(unsafe {
            addr_of_mut!((*ptr).heads[pid as usize].0)
        }),
        buffer: ThreadLocalBuffer::<L>::new(
            (unsafe { addr_of_mut!((*ptr).buffer) } as usize + {
                pid as usize * { 1 << C }
            }) as *mut [u8; L],
        ),
        refcount: unsafe { addr_of_mut!((*ptr).refcount) },
        mpscq_ptr: ptr as *const __MPSCQ<T, C, S, L, A>,
    };
    ret
}

/// Returns a consumer handle. This allows a single thread to pop data
/// from the other producers in a safe way.
fn cons_handle<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    ptr: *mut __MPSCQ<T, C, S, L, A>,
) -> ConsumerHandleImpl<T, C, S, L, A> {
    let mut heads: [ReadOnlyHead<C>; T] = unsafe { core::mem::zeroed() };
    for i in 0..T {
        heads[i] =
            ReadOnlyHead::new(unsafe { addr_of_mut!((*ptr).heads[i].0) });
    }
    ConsumerHandleImpl::<T, C, S, L, A> {
        tails: RWTails::<T, C>::new(unsafe { addr_of_mut!((*ptr).tails.0) }),
        heads,
        buffer: ReadOnlyBuffer::<T, S, L>::new(unsafe {
            addr_of_mut!((*ptr).buffer)
        }),
        refcount: unsafe { addr_of_mut!((*ptr).refcount) },
        mpscq_ptr: ptr,
    }
}

/// Splits a correctly allocated __MPSCQ object into a consumer and producer
/// array (totaling T+1 objects). These objects are internally atomically
/// refcounted, so the resulting objects are thread safe.
pub fn split<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    ptr: *mut __MPSCQ<T, C, S, L, A>,
    alloc: Option<A>,
) -> (ConsumerHandleImpl<T, C, S, L, A>, [TLQ<T, C, S, L, A>; T]) {
    // FIXME: Check if this doesn't invoke undefined behavior
    let alloc_ptr = unsafe { addr_of_mut!((*ptr).alloc) };
    unsafe {
        alloc_ptr.write(alloc);
    }

    let mut producers: [MaybeUninit<TLQ<T, C, S, L, A>>; T] =
        unsafe { MaybeUninit::uninit().assume_init() };
    for (i, p) in producers.iter_mut().enumerate() {
        p.write(prod_handle(ptr, i as u8));
    }
    // FIXME: Cannot do mem::transmute from MaybeUninit to a const generic
    // array. See https://github.com/rust-lang/rust/issues/61956
    let prod_ptr = addr_of!(producers) as *const _;
    let producers = unsafe { core::ptr::read(prod_ptr) };

    // SAFETY: refcount is atomic, so creating a shared reference is safe.
    let refcount = unsafe { &(*ptr).refcount };
    refcount.store(T as u32 + 1, Ordering::Release);

    // SAFETY: We are only allowed to turn initialized memory into a value type.
    // When using the MPSC queue, the first thing you do is read heads and tails
    // to determine length/capacity and continue with the read/write operation.
    //
    // Because of this, heads and tails MUST be initialized before being
    // released to safe rust-land.
    zero_heads_and_tails(ptr);
    (cons_handle(ptr), producers)
}

fn zero_heads_and_tails<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    ptr: *mut __MPSCQ<T, C, S, L, A>,
) {
    for i in 0..T {
        // because tails are atomic, we are allowed to create a shared reference
        // with multiple aliases. Same reasoning applies for head elements.
        let tail = unsafe { &(*ptr).tails.0[i] };
        tail.store(0, Ordering::Release);
        let head = unsafe { &(*ptr).heads[i].0 };
        head.store(0, Ordering::Release);
    }
}
/* create_aligned! end */

impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > Drop for ConsumerHandleImpl<T, C, S, L, A>
{
    fn drop(&mut self) {
        drop_handle(unsafe { &*self.refcount }, self.mpscq_ptr);
    }
}
impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > Drop for TLQ<T, C, S, L, A>
{
    fn drop(&mut self) {
        // sound because we obtained our refcount from a valid shared reference
        drop_handle(unsafe { &*self.refcount }, self.mpscq_ptr);
    }
}

/// Atomic ref-counting implementation. Atomically drops a consumer or producer
/// handle.
fn drop_handle<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    refcount: &AtomicU32,
    mpscq_ptr: *const __MPSCQ<T, C, S, L, A>,
) {
    // See: std::sync::Arc source code. Release + Acquire
    if refcount.fetch_sub(1, Ordering::Release) != 1 {
        return;
    }
    refcount.load(Ordering::Acquire);

    // information for deallocation
    let ptr = mpscq_ptr as *const u8 as *mut u8;
    let layout = __MPSCQ::<T, C, S, L, A>::layout();

    // SAFETY: Since the queue is refcounted, we know that no other thread
    // has a mutable reference of it. Any operation from this point may be
    // considered single-threaded.
    let mpscq_ref = unsafe { &mut *(mpscq_ptr as *mut __MPSCQ<T, C, S, L, A>) };
    let custom_dealloc = mpscq_ref.alloc.take();
    match custom_dealloc {
        None => {
            #[cfg(not(feature = "alloc"))]
            {
                panic!("no custom allocator found!");
            }
            #[cfg(feature = "alloc")]
            {
                // SAFETY: The object was created with alloc, and with the same
                // alignment. Also, this object is a POD with the exception of
                // the custom allocator. But since that allocator is None, we
                // have no managed resources or fields with custom destructors.
                // Hence, we can safely deallocate this object.
                unsafe {
                    alloc::alloc::dealloc(ptr, layout);
                }
            }
        }
        Some(f) => {
            drop(mpscq_ref);
            // We are allowed to dealloc with the cloned allocator.
            // From [`Allocator`] docs:
            //   "A cloned allocator must behave like the same allocator."
            let cloned_alloc = f.clone();
            drop(f);
            // SAFETY: We already dropped the custom allocator, making the
            // rest of the queue a POD that can be freed.
            unsafe {
                cloned_alloc.deallocate(NonNull::new_unchecked(ptr), layout);
            }
        }
    };
}

impl<
        const T: usize,
        const C: usize,
        const S: usize,
        const L: usize,
        A: ThreadSafeAlloc,
    > Display for __MPSCQ<T, C, S, L, A>
{
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> Result<(), core::fmt::Error> {
        write!(f, "heads: ")?;
        for (idx, h) in self.heads.iter().enumerate() {
            write!(f, "[#{}: {}] ", idx, h.0.load(Ordering::Relaxed))?;
        }
        Ok(())
    }
}

/// Creates an MPSC queue using the standard allocator.
///
/// Parameters:
///  - bitsize(usize): number of bits of queue capacity (1 <= b <= 32)
///  - producers(usize): number of concurrent producers, usize
///  - l1_cache(usize): byte size of L1 cache
///
/// If you want to use a custom allocator, use [`queue_alloc!`].
#[macro_export]
#[cfg(feature = "alloc")]
macro_rules! queue {
    (
        bitsize: $b:expr,
        producers: $p:expr
    ) => {{
        extern crate alloc;
        let layout = wfmpsc::__MPSCQ::<
            $p,
            $b,
            { (1 << $b) * $p },
            { 1 << $b },
            alloc::alloc::Global,
        >::layout();
        let queue = unsafe {
            // SAFETY: Turning any uninitialized memory into a value type is UB.
            // In this queue, we make sure that any read was preceded by a store
            // for every memory location.
            alloc::alloc::alloc(layout)
                as *mut wfmpsc::__MPSCQ<
                    $p,
                    $b,
                    { (1 << $b) * $p },
                    { 1 << $b },
                    std::alloc::Global,
                >
        };
        wfmpsc::split(queue, None)
    }};
}

/// Creates a MPSC queue with a custom allocator.
///
/// Parameters:
///  - bitsize(usize): number of bits of queue capacity (1 <= b <= 32)
///  - producers(usize): number of concurrent producers, usize
///  - allocator(Allocator): an allocator object, using Rust alloc API.
///
/// The allocator must implement the core::alloc::Allocator interface. If
/// you want to use the default system allocator, use [`queue!`].
#[macro_export]
macro_rules! queue_alloc {
    (
        bitsize: $b:expr,
        producers: $p:expr,
        alloc: $alloc:ty, $($args:ident),* $(,)?
    ) => {{
        use core::alloc::{Allocator, Layout};
        let alloc = <$alloc>::new($($args)*);
        let layout = wfmpsc::__MPSCQ::<
            $p,
            $b,
            { (1 << $b) * $p },
            { 1 << $b },
            $alloc,
        >::layout();
        let queue = unsafe {
            alloc.allocate(layout).unwrap().as_ptr()
                as *mut wfmpsc::__MPSCQ<
                    $p,
                    $b,
                    { (1 << $b) * $p },
                    { 1 << $b },
                    $alloc,
                >
        };
        wfmpsc::split(queue, Some(alloc))
    }};
}

/// Returns 0xfff..., where `B` is the number of `f`s.
#[inline(always)]
const fn fmask_udefault<const B: usize>() -> udefault {
    (1 << B) - 1
}

/// A stub allocator that always returns them same given memory region.
/// The region is given by START and SIZE parameter (address and length
/// of the memory region)
pub struct FixedAllocStub<const START: usize, const SIZE: usize>;

unsafe impl<const START: usize, const SIZE: usize> Allocator
    for FixedAllocStub<START, SIZE>
{
    fn allocate(&self, _: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match NonNull::new(core::ptr::slice_from_raw_parts_mut(
            START as *mut u8,
            SIZE,
        )) {
            None => Err(AllocError {}),
            Some(ptr) => Ok(ptr),
        }
    }
    unsafe fn deallocate(&self, _: NonNull<u8>, _: Layout) {}
}
