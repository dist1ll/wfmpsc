/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
#![feature(test)]
#![feature(allocator_api)]

extern crate test;
use std::{arch::asm, time::Duration};
use test::{black_box, Bencher};
use wfmpsc::{queue, ConsumerHandle, TLQ};

/// Our wfmpsc requires certain configuration parameters to be known at compile-
/// time. This is why we define a `const` config object,
///
/// To make runs with different configurations, we pass the correct env variables
/// at build time.
const CFG: BenchCfg = BenchCfg {
    queue_size: 4,
    producer_count: 4,
    load: LoadFactor::Maximum,
    chunk_size: 3,
};

pub struct BenchCfg {
    queue_size: usize,
    /// Number of producer threads to be spawned
    producer_count: usize,
    /// Type of CPU load with which data is pushed into the MPSC queue
    load: LoadFactor,
    /// Size of chunks inserted into the queue
    chunk_size: usize,
    // /// Burstiness of traffic. 0 means constant, homogeneous load and
    // /// 1 means data is added in very short intensive bursts.
    // burstiness: f64,
}

#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub enum LoadFactor {
    /// Hammering the queue constantly
    #[default]
    Maximum,
    /// Padding with a few dozen instructions
    Medium,
    /// Blocking wait for short time
    Low,
}

#[bench]
fn eval(_: &mut Bencher) {
    run_wfmpsc();
}

/// Run the bench configuration on a wfmpsc queue (this crate)
fn run_wfmpsc() {
    let mut handlers = vec![];
    let total_bytes = 10 * (1 << CFG.queue_size); // 100 times queue size
    let (consumer, prods) = queue!(
        bitsize: { CFG.queue_size },
        producers: { CFG.producer_count }
    );
    for p in prods.into_iter() {
        let tmp = std::thread::spawn(move || {
            push_wfmpsc(p, total_bytes);
        });
        handlers.push(tmp);
    }
    pop_wfmpsc(consumer, total_bytes * CFG.producer_count);
    for h in handlers {
        h.join().expect("Joining thread");
    }
}

fn push_wfmpsc<const T: usize, const C: usize, const S: usize, const L: usize>(
    mut p: TLQ<T, C, S, L>,
    bytes: usize,
) {
    let mut chunk = vec![0u8; CFG.chunk_size];
    let mut written = 0;
    while written < bytes {
        black_box(&mut chunk);
        written += p.push(&chunk);
        black_box(&mut p);
        // waste time to reducer queue load
        if CFG.load == LoadFactor::Low {
            std::thread::sleep(Duration::from_nanos(100));
        }
        if CFG.load == LoadFactor::Medium {
            unsafe {
                asm!(
                    "nop\nnop\nnop\nnop\nnop\nnop\nnop\nnop\n
                     nop\nnop\nnop\nnop\nnop\nnop\nnop\nnop\n"
                );
            }
        }
    }
}

/// Blocking function that empties the MPSCQ until a total number of
/// `elem_count` elements have been popped in total.
fn pop_wfmpsc(c: impl ConsumerHandle, bytes: usize) {
    let mut counter: usize = 0;
    let mut destination_buffer = [0u8; 1 << 8]; // uart dummy
    let p_count = c.get_producer_count();
    while counter < bytes {
        for i in 0..p_count {
            let written_bytes = c.pop_into(i, &mut destination_buffer);
            counter += written_bytes;
        }
    }
}
