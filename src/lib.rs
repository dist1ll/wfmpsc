/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![feature(allocator_api)]

use core::fmt::Debug;
use core::{
    fmt::Display,
    sync::atomic::{AtomicU32, Ordering},
};

use core::mem::MaybeUninit;
use core::ptr::{addr_of, addr_of_mut, copy_nonoverlapping};
use core::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};

/* Aliases for queue's atomic integer type */
type AtomicUnit = AtomicU32;

/// A safe interface to access the MPSC queue, allowing a single
pub trait ConsumerHandle {
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
    refcount: *const AtomicUnit,
    mpscq_ptr: *const __MPSCQ<T, C, S, L>,
}

impl<const T: usize, const C: usize, const S: usize, const L: usize> ConsumerHandle
    for ConsumerHandleImpl<T, C, S, L>
{
    fn pop_elements_into(&self, pid: usize, dst: &mut [u8]) -> usize {
        // We can use relaxed memory ordering, because a stale head doesn't cause
        // a data race.
        let tail = self.tails.read_atomic(pid, Ordering::Relaxed);
        let head = self.heads[pid].read_atomic(Ordering::Relaxed);
        let queue_element_count = queue_element_count::<C>(head, tail) as usize;

        // TODO: Consider off-by-one error because of requirement to not let
        // queue fill up fully (max. 2^C elements are allowed) u_u

        // actual length of elements to be copied
        let len = core::cmp::min(queue_element_count, dst.len());
        let target_wrap = (tail + len as u32) & fmask_32::<C>();
        let src = (self.buffer.tlq_base_ptr(pid) as usize + tail as usize) as *mut u8;
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
                    split_len,
                );
            }
        }
        self.tails.incr_atomic_rel(pid, len as u32);
        len
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

/// A producer handle. Use this to push data into the MPSC queue that can be
/// read by the consumer.
#[derive(Debug)]
pub struct TLQ<const T: usize, const C: usize, const S: usize, const L: usize> {
    pub head: RWHead<C>,
    pub tail: ReadOnlyTail<C>,
    pub buffer: ThreadLocalBuffer<L>,
    refcount: *const AtomicUnit,
    mpscq_ptr: *const __MPSCQ<T, C, S, L>,
}

impl<const T: usize, const C: usize, const S: usize, const L: usize> TLQ<T, C, S, L> {
    /// Pushes a byte slice to the buffer. Performs a partial write if
    /// the queue is full. No error is returned.
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
    pub fn push(&self, byte: &[u8]) {
        // relaxed ordering is fine, because a stale read from tail
        // does not cause a data race.
        let tail = self.tail.read_atomic(Ordering::Relaxed);
        let head = self.head.read_atomic(Ordering::Relaxed);
        let capacity = queue_leftover_capacity::<C>(head, tail);

        // Limit arg bytes to queue size - 1 (because we can't distinguish)
        // between full and empty queues if its filled entirely.
        let len = core::cmp::min(capacity, byte.len() as u32 + 1) as usize - 1;
        if ((head + 1) & fmask_32::<C>() == tail) || byte.len() == 0 {
            return;
        }

        let target_wrap = (head + len as u32) & fmask_32::<C>();
        let src = byte.as_ptr() as usize;
        let dst = self.buffer.0 as usize + head as usize;
        if target_wrap >= head {
            // only make one copy
            unsafe {
                core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, len as usize);
            }
        }
        // TODO: place this path into a seperate function, as its less likely.
        else {
            // we have two make two copies
            unsafe {
                let split_len = L - head as usize;
                core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, split_len);
                core::ptr::copy_nonoverlapping(
                    (src + C - head as usize) as *const u8,
                    self.buffer.0 as *mut u8,
                    len - split_len,
                );
            }
        }
        // We don't want to increment the head before the memcpy completes!
        self.head
            .store_atomic((head + len as u32) & fmask_32::<C>(), Ordering::Release);
    }
}

/// Computes the remaining capacity of a ring buffer for a given head index, tail index
/// and bit width of the queue (i.e. the total capacity).
#[inline(always)]
pub fn queue_leftover_capacity<const C: usize>(head: u32, tail: u32) -> u32 {
    match head >= tail {
        true => (1 << C) - (head - tail),
        false => tail - head,
    }
}

/// Computes the element count of a ring buffer for a given head index, tail index and
/// bit width of the queue (i.e. the total capacity).
#[inline(always)]
pub fn queue_element_count<const C: usize>(head: u32, tail: u32) -> u32 {
    (1 << C) - queue_leftover_capacity::<C>(head, tail)
}

unsafe impl<const T: usize, const C: usize, const S: usize, const L: usize> Send
    for TLQ<T, C, S, L>
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
pub struct RWTails<const T: usize, const C: usize>(*const [AtomicUnit; T]);
impl<const T: usize, const C: usize> RWTails<T, C> {
    pub fn new(ptr: *const [AtomicUnit; T]) -> Self {
        Self { 0: ptr }
    }
    #[inline(always)]
    pub fn read_atomic(&self, pid: usize, ord: Ordering) -> u32 {
        // TODO: Check that pointer arithmetics don't outperform this
        unsafe {
            let base_addr = self.0 as *mut AtomicUnit as usize;
            let pid_pointer = base_addr + pid * core::mem::size_of::<AtomicUnit>();
            let atomic = &*(pid_pointer as *const AtomicUnit);
            atomic.load(ord)
        }
    }
    /// Increment the tail atomically using release semantics.
    #[inline(always)]
    pub fn incr_atomic_rel(&self, pid: usize, len: u32) {
        // NOTE: We don't need CAS or LL/SC because we are the only thread that's
        // performing STORE operations on this memory address.
        unsafe {
            // Bitmask wrap increment, using bitwidth of queue C
            let atomic = &*((self.0 as usize + pid * 4) as *mut AtomicUnit);
            let curr_val = atomic.load(Ordering::Relaxed);
            let new_val = (curr_val + len) & fmask_32::<C>();
            atomic.store(new_val, Ordering::Release);
        }
    }
}

/// A tail that refers to the queue of a single, specific thread-local queue.
/// This is a read-only view! The tail may only be modified safely by the consumer.
#[derive(Debug)]
pub struct ReadOnlyTail<const C: usize>(*const AtomicUnit);
impl<const C: usize> ReadOnlyTail<C> {
    pub fn new(ptr: *const AtomicUnit) -> Self {
        Self { 0: ptr }
    }
    /// Performs an atomic read on the tail with given memory ordering.
    #[inline(always)]
    pub fn read_atomic(&self, ord: Ordering) -> u32 {
        unsafe {
            let atomic = &*self.0;
            atomic.load(ord)
        }
    }
}

/// A head that refers to the queue of a single, specific thread-local queue.
/// This is a read-only view! The tail may only be modified safely by the producer.
#[derive(Debug)]
pub struct ReadOnlyHead<const C: usize>(*const AtomicUnit);
impl<const C: usize> ReadOnlyHead<C> {
    pub fn new(ptr: *const AtomicUnit) -> Self {
        Self { 0: ptr }
    }
    /// Performs an atomic read on the head with given ordering semantics.
    #[inline(always)]
    pub fn read_atomic(&self, ord: Ordering) -> u32 {
        unsafe {
            let atomic = &*self.0;
            atomic.load(ord)
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

/// Read & Write access to a TLQ head. May only be modified by a
/// single producer.
/// A head that may only be modified by exactly one thread! Also:
/// the head references exactly 2^C element, which means the queue's
/// ring buffer needs to have a matching capacity.
#[derive(Debug)]
pub struct RWHead<const C: usize>(*const AtomicUnit);
impl<const C: usize> RWHead<C> {
    pub fn new(ptr: *const AtomicUnit) -> Self {
        Self { 0: ptr }
    }
    /// Increments the head pointer, thereby committing the written
    /// bytes to the consumer
    #[inline]
    pub fn store_atomic(&self, val: u32, ord: Ordering) {
        unsafe {
            let atomic = &*self.0;
            atomic.store(val, ord);
        }
    }
    /// Performs an atomic read on the head with given ordering.
    #[inline(always)]
    pub fn read_atomic(&self, ord: Ordering) -> u32 {
        unsafe {
            let atomic = &*self.0;
            atomic.load(ord)
        }
    }
}

impl<const T: usize, const C: usize, const S: usize, const L: usize> Display for TLQ<T, C, S, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "Head(val): {}\nHead(addr): 0x{:x}\nBuffer: {}",
            self.head, self.head.0 as usize, self.buffer
        )
    }
}

impl<const L: usize> Display for ThreadLocalBuffer<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
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

/// Array of cache-aligned queue tails
#[cfg_attr(not(cache_line), repr(C, align(64)))]
#[cfg_attr(cache_line = "32", repr(C, align(32)))]
#[cfg_attr(cache_line = "64", repr(C, align(64)))]
#[cfg_attr(cache_line = "128", repr(C, align(128)))]
#[derive(Debug)]
pub struct __Tails<const T: usize>(pub [AtomicUnit; T]);

/// Single queue head
#[cfg_attr(not(cache_line), repr(C, align(64)))]
#[cfg_attr(cache_line = "32", repr(C, align(32)))]
#[cfg_attr(cache_line = "64", repr(C, align(64)))]
#[cfg_attr(cache_line = "128", repr(C, align(128)))]
#[derive(Debug)]
pub struct __Head(pub AtomicUnit);

/// MPSCQ table that stores tail and head as offsets into TLQs. S is the max
/// number of elements in all TLQs combined. So S = T * 2^C
#[cfg_attr(not(cache_line), repr(C, align(64)))]
#[cfg_attr(cache_line = "32", repr(C, align(32)))]
#[cfg_attr(cache_line = "64", repr(C, align(64)))]
#[cfg_attr(cache_line = "128", repr(C, align(128)))]
pub struct __MPSCQ<
    const T: usize, // number of producers
    const C: usize, // bitwidth of queue size
    const S: usize, // size of entire global buffer (T * 2^C)
    const L: usize, // size of the thread-local buffer (2 ^ C)
> {
    buffer: [u8; S],
    tails: __Tails<T>,
    heads: [__Head; T],
    refcount: AtomicUnit,
    dealloc: Option<DeallocFn>,
}

impl<'a, const T: usize, const C: usize, const S: usize, const L: usize> __MPSCQ<T, C, S, L> {
    pub const fn layout() -> Layout {
        let size = core::mem::size_of::<Self>();
        let align = core::mem::align_of::<Self>();
        unsafe { Layout::from_size_align_unchecked(size, align) }
    }
}

/// Returns a producer handle
fn prod_handle<const T: usize, const C: usize, const S: usize, const L: usize>(
    ptr: *mut __MPSCQ<T, C, S, L>,
    pid: u8,
) -> TLQ<T, C, S, L> {
    assert!((pid as usize) < T);
    let ret = TLQ::<T, C, S, L> {
        tail: ReadOnlyTail::new(unsafe { addr_of_mut!((*ptr).tails.0[pid as usize]) }),
        head: RWHead::new(unsafe { addr_of_mut!((*ptr).heads[pid as usize].0) }),
        buffer: ThreadLocalBuffer::<L>::new(
            (unsafe { addr_of_mut!((*ptr).buffer) } as usize + { pid as usize * { 1 << C } })
                as *mut [u8; L],
        ),
        refcount: unsafe { addr_of_mut!((*ptr).refcount) },
        mpscq_ptr: ptr as *const __MPSCQ<T, C, S, L>,
    };
    eprintln!("dude what");
    eprintln!("0x{:x}", ret.refcount as usize);
    ret
}

/// Returns a consumer handle. This allows a single thread to pop data
/// from the other producers in a safe way.
fn cons_handle<const T: usize, const C: usize, const S: usize, const L: usize>(
    ptr: *mut __MPSCQ<T, C, S, L>,
) -> ConsumerHandleImpl<T, C, S, L> {
    let mut heads: [ReadOnlyHead<C>; T] = unsafe { core::mem::zeroed() };
    for i in 0..T {
        heads[i] = ReadOnlyHead::new(unsafe { addr_of_mut!((*ptr).heads[i].0) });
    }
    ConsumerHandleImpl::<T, C, S, L> {
        tails: RWTails::<T, C>::new(unsafe { addr_of_mut!((*ptr).tails.0) }),
        heads,
        buffer: ReadOnlyBuffer::<T, S, L>::new(unsafe { addr_of_mut!((*ptr).buffer) }),
        refcount: unsafe { addr_of_mut!((*ptr).refcount) },
        mpscq_ptr: ptr,
    }
}

/// Splits a correctly allocated __MPSCQ object into a consumer and producer
/// array (totaling T+1 objects). These objects are internally atomically
/// refcounted, so the resulting objects are thread safe.
pub fn split<const T: usize, const C: usize, const S: usize, const L: usize>(
    ptr: *mut __MPSCQ<T, C, S, L>,
) -> (ConsumerHandleImpl<T, C, S, L>, [TLQ<T, C, S, L>; T]) {
    let mut producers: [MaybeUninit<TLQ<T, C, S, L>>; T] =
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
    refcount.store(T as u32 + 1, Ordering::SeqCst);

    // SAFETY: We are only allowed to turn initialized memory into a value type.
    // When using the MPSC queue, the first thing you do is read heads and tails
    // to determine length/capacity and continue with the read/write operation.
    //
    // Because of this, heads and tails MUST be initialized before being
    // released to safe rust-land.
    zero_heads_and_tails(ptr);
    (cons_handle(ptr), producers)
}

fn zero_heads_and_tails<const T: usize, const C: usize, const S: usize, const L: usize>(
    ptr: *mut __MPSCQ<T, C, S, L>,
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

impl<const T: usize, const C: usize, const S: usize, const L: usize> Drop
    for ConsumerHandleImpl<T, C, S, L>
{
    fn drop(&mut self) {
        drop_handle(unsafe { &*self.refcount }, self.mpscq_ptr);
    }
}
impl<const T: usize, const C: usize, const S: usize, const L: usize> Drop for TLQ<T, C, S, L> {
    fn drop(&mut self) {
        // sound because we obtained our refcount from a valid shared reference
        eprintln!("0x{:x}", self.refcount as usize);
        drop_handle(unsafe { &*self.refcount }, self.mpscq_ptr);
    }
}

/// Atomic ref-counting implementation. Atomically drops a consumer or producer
/// handle.
fn drop_handle<const T: usize, const C: usize, const S: usize, const L: usize>(
    refcount: &AtomicUnit,
    mpscq_ptr: *const __MPSCQ<T, C, S, L>,
) {
    // See: std::sync::Arc source code. Release + Acquire
    if refcount.fetch_sub(1, Ordering::Release) != 1 {
        eprintln!("dropped consumer!");
        return;
    }
    refcount.load(Ordering::Acquire);
    #[cfg(feature = "no_std")]
    {
        // TODO: drop it like it's hot
        // TODO: do no_std deallocation
    }
    #[cfg(not(feature = "no_std"))]
    {
        eprintln!("Deallocation complete!");
        let ptr = mpscq_ptr as *const u8 as *mut u8;
        unsafe {
            std::alloc::dealloc(ptr, __MPSCQ::<T, C, S, L>::layout());
        }
    }
}
/*

*/

impl<const T: usize, const C: usize, const S: usize, const L: usize> Display
    for __MPSCQ<T, C, S, L>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
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
macro_rules! queue {
    (
        bitsize: $b:expr,
        producers: $p:expr
    ) => {{
        use core::alloc::Layout;
        let layout = wfmpsc::__MPSCQ::<$p, $b, { (1 << $b) * $p }, { 1 << $b }>::layout();
        let queue = unsafe {
            // SAFETY: Turning any uninitialized memory into a value type is UB.
            // In this queue, we make sure that any read was preceded by a store
            // for every memory location.
            std::alloc::alloc(layout)
                as *mut wfmpsc::__MPSCQ<$p, $b, { (1 << $b) * $p }, { 1 << $b }>
        };
        wfmpsc::split(queue)
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
        allocator: $alloc:expr
    ) => {{
        use core::alloc::{Allocator, Layout};
        let layout = wfmpsc::__MPSCQ<$p, $b, { (1 << $b) * $p }, { 1 << $b }>::layout();
        let queue = unsafe {
            $alloc.allocate(layout).unwrap().as_ptr()
                as *mut wfmpsc::__MPSCQ<$p, $b, { (1 << $b) * $p }, { 1 << $b }>
        };
        let q_ref = unsafe { queue.as_mut().unwrap() };
        q_ref.zero_heads_and_tails();
        q_ref.split()
    }};
}

/// Returns 0xfff..., where `B` is the number of `f`s.
#[inline(always)]
const fn fmask_32<const B: usize>() -> u32 {
    (1 << B) - 1
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
