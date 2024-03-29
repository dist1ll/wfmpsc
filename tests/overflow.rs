/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![feature(allocator_api)]

mod util;
use std::{
    hint::black_box,
    sync::{
        atomic::{AtomicU32, AtomicUsize, Ordering},
        Arc,
    },
};
use util::MockAllocator;
use wfmpsc::{queue, ConsumerHandle, ProducerHandle, Section, ThreadSafeAlloc};

/**!
This test checks if indexing is correctly implemented for overlapping concurrent
push and pop operations.
*/

/// Check that we can't pop data from a producer ID higher than the given
/// producer count
#[test]
#[should_panic]
pub fn pid_overflow() {
    let (tx, rx) = wfmpsc::queue!(bitsize: 4, producers: 8);
    tx[0].push("".as_bytes());
    let mut x = [0u8; 12];
    rx.pop_into(8, &mut x);
}

/// Check if custom deallocator is called.
#[test]
pub fn custom_dealloc() {
    // alloc can't outlive mpsc!!!
    let counter = std::sync::Arc::new(AtomicU32::new(0));
    let cc = counter.clone();
    let (tx, rx) = wfmpsc::queue_alloc!(
        bitsize: 4,
        producers: 5,
        alloc: MockAllocator, cc,
    )
    .unwrap();
    assert!(counter.load(Ordering::Acquire) == 1);
    drop(rx);
    drop(tx);
    assert!(counter.load(Ordering::Acquire) == 0);
}

/// Check if partial writes are executed correctly on the buffer.
#[test]
pub fn partial_write() {
    let (tx, rx) = wfmpsc::queue!(bitsize: 4, producers: 1);
    // push more than 15 bytes into the queue
    tx[0].push("Hello World, how are you doing".as_bytes());
    //                           ^
    //                     the 'w' is the 16th letter
    let mut dst = [0u8; 15];
    rx.pop_into(0, &mut dst);
    assert_eq!("Hello World, ho", conv(&dst));
}

#[test]
pub fn concurrent_write() {
    let mut handlers = vec![];
    let total_bytes = 500_000;
    const PROD_COUNT: usize = 8;
    let (tx, rx) = queue!(
        bitsize: 12,
        producers: PROD_COUNT
    );
    let prod_counter = Arc::new(AtomicUsize::new(PROD_COUNT));
    for p in tx.into_iter() {
        let pc = prod_counter.clone();
        let tmp = std::thread::spawn(move || {
            push_wfmpsc(p, total_bytes, 105, pc);
        });
        handlers.push(tmp);
    }
    pop_wfmpsc(rx, prod_counter);
    for h in handlers {
        h.join().expect("Joining thread");
    }
}

#[test]
pub fn no_overlapping_pops() {
    let (tx, rx) = wfmpsc::queue!(bitsize: 4, producers: 1);
    tx[0].push("123456789".as_bytes());
    let mut dst = [0u8; 4];
    rx.pop_into(0, &mut dst);
    assert_eq!("1234", conv(&dst));
    rx.pop_into(0, &mut dst);
    assert_eq!("5678", conv(&dst));
}

#[test]
pub fn raii_slice() {
    // max capacity is 2^3 - 1
    let (tx, mut rx) = wfmpsc::queue!(bitsize: 3, producers: 1);
    tx[0].push("1234".as_bytes());
    {
        let mut section = rx.pop(0);
        let buffer = section.get_buffer();
        assert_eq!(tx[0].tail.read_atomic(Ordering::Relaxed), 0);
        assert_eq!("1234", conv(buffer));
    } // dropping buffer, which should increase tail
    tx[0].push(b"5678901");
    {
        assert_eq!(tx[0].tail.read_atomic(Ordering::Relaxed), 4);
        let mut section = rx.pop(0);
        let buffer = section.get_buffer();
    } // dropping buffer

    // tail = ZERO because pop can't do split reads.
    assert_eq!(tx[0].tail.read_atomic(Ordering::Relaxed), 0);
    let mut section = rx.pop(0);
    let buffer = section.get_buffer();
    assert_eq!("901", conv(buffer));
}

fn push_wfmpsc<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    mut p: ProducerHandle<T, C, S, L, A>,
    bytes: usize,
    chunk_size: usize,
    pc: Arc<AtomicUsize>,
) {
    let mut chunk = vec![0u8; chunk_size];
    let mut written = 0;
    while written < bytes {
        black_box(&mut chunk);
        written += p.push(&chunk);
        black_box(&mut p);
    }
    pc.fetch_sub(1, Ordering::Release);
}

/// Blocking function that empties the MPSCQ until a total number of
/// `elem_count` elements have been popped in total.
fn pop_wfmpsc(c: impl ConsumerHandle, pc: Arc<AtomicUsize>) {
    let mut counter: usize = 0;
    let mut destination_buffer = [0u8; 1 << 8]; // uart dummy
    let p_count = c.get_producer_count();
    while pc.load(Ordering::Acquire) != 0 {
        for i in 0..p_count {
            let written_bytes = c.pop_into(i, &mut destination_buffer);
            counter += written_bytes;
        }
    }
    black_box(counter);
}

/// utility function that converts byte slice into utf8 and unwraps
fn conv(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap()
}
