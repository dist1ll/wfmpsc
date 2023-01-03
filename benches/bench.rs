/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
#![feature(test)]
#![feature(allocator_api)]

#[macro_use]
extern crate criterion;
use criterion::Criterion;

mod cfg;
use cfg::{cfg_from_env, BenchCfg};

use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use wfmpsc::{queue, ConsumerHandle, TLQ};

/// Our wfmpsc requires certain configuration parameters to be known at
/// compile-time. This is why we define a `const` config object,
///
/// To make runs with different configurations, we pass the correct env
/// variables at build time.
const CFG: BenchCfg = cfg_from_env();

/// Run the bench configuration on a wfmpsc queue (this crate)
fn run_wfmpsc(c: &mut Criterion) {
    c.bench_function(
        format!(
            "wfmpsc_q{}_p{}_d{}_c{}",
            CFG.queue_size, CFG.producer_count, CFG.dummy_count, CFG.chunk_size
        )
        .as_str(),
        |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let mut handlers = vec![];
                    let total_bytes = 20_000_000 / CFG.producer_count; //10Mb
                    let (consumer, prods) = queue!(
                        bitsize: { CFG.queue_size },
                        producers: { CFG.producer_count }
                    );
                    // -----------------------------------
                    // start measuring
                    let start = Instant::now();
                    for p in prods.into_iter() {
                        let tmp = std::thread::spawn(move || {
                            push_wfmpsc(p, total_bytes);
                        });
                        handlers.push(tmp);
                    }
                    pop_wfmpsc(consumer, total_bytes * CFG.producer_count);
                    // stop measuring
                    // -----------------------------------
                    total += start.elapsed();
                    for h in handlers {
                        h.join().expect("Joining thread");
                    }
                }
                total
            })
        },
    );
}

fn push_wfmpsc<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
>(
    mut p: TLQ<T, C, S, L>,
    bytes: usize,
) {
    let chunk = vec![0u8; CFG.chunk_size];
    let mut written = 0;
    while written < bytes {
        black_box(&mut p);
        // let's make it exact
        let diff = bytes - written;
        if diff < chunk.len() {
            written += p.push(&chunk[0..diff])
        } else {
            written += p.push(&chunk);
        }
        // waste time to reducer queue load
        for mut i in 0..CFG.dummy_count {
            black_box(&mut i);
        }
        black_box(&mut written);
    }
}

/// Blocking function that empties the MPSCQ until a total number of
/// `elem_count` elements have been popped in total.
fn pop_wfmpsc(c: impl ConsumerHandle, bytes: usize) {
    let mut counter: usize = 0;
    let mut destination_buffer = [0u8; 1 << 7]; // uart dummy
    let p_count = c.get_producer_count();
    while counter < bytes {
        for i in 0..p_count {
            black_box(&mut destination_buffer);
            counter += c.pop_into(i, &mut destination_buffer);
            black_box(&mut counter);
        }
    }
}

criterion_group!(
    name = fast;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1));
    targets = run_wfmpsc
);
criterion_group!(
    name = accurate;
    config = Criterion::default()
        .sample_size(300)
        .warm_up_time(Duration::from_secs(2));
    targets = run_wfmpsc
);
criterion_main!(accurate);
