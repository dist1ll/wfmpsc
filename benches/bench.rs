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
    producer_count: 1,
    load: LoadFactor::Maximum,
    chunk_size: 1,
};

pub struct BenchCfg {
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
    let cpus = num_cpus::get();
    run_wfmpsc();
    println!("You have {} cpus", cpus);
}

/// Run the bench configuration on a wfmpsc queue (this crate)
fn run_wfmpsc() {
    let mut handlers = vec![];
    let (_, prods) = queue!(
        bitsize: 4,
        producers: { CFG.producer_count }
    );
    for p in prods.into_iter() {
        let tmp = std::thread::spawn(move || {
            push_wfmpsc(p);
        });
        handlers.push(tmp);
    }
    for h in handlers {
        h.join().expect("Joining thread");
    }
}

fn push_wfmpsc<const C: usize, const L: usize>(mut p: TLQ<C, L>) {
    let chunk = vec![0u8; CFG.chunk_size];
    loop {
        if CFG.load == LoadFactor::Low {
            std::thread::sleep(Duration::from_nanos(100));
        }
        if CFG.load == LoadFactor::Medium {
            unsafe {
                asm!(
                    "
                        nop\nnop\nnop\nnop\nnop\nnop\nnop\nnop\n
                        nop\nnop\nnop\nnop\nnop\nnop\nnop\nnop\n
                        "
                );
            }
        }
        p.push(&chunk);
        black_box(&mut p);
    }
}

/// Blocking function that empties the MPSCQ until a total number of
/// `elem_count` elements have been popped in total.
fn pop_wfmpsc(c: impl ConsumerHandle, elem_count: usize) {
    let mut counter: usize = 0;
    let mut destination_buffer = [0u8; 1 << 8]; // uart dummy
    loop {
        if counter >= elem_count {
            return;
        }
        for i in 0..c.get_producer_count() {
            let written_bytes = c.pop_elements_into(i, &mut destination_buffer);
            counter += written_bytes;
        }
    }
}
