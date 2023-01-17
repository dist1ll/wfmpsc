/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
#![feature(test)]
#![feature(allocator_api)]

#[macro_use]
extern crate criterion;
extern crate core_affinity;
use core_affinity::CoreId;
use criterion::Criterion;

mod cfg;
use cfg::{cfg_from_env, BenchCfg};

use std::{
    hint::black_box,
    mem::size_of,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use wfmpsc::{queue, ConsumerHandle, ThreadSafeAlloc, TLQ};

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
            "x_q{}_p{}_d{}_c{}",
            CFG.queue_size, CFG.producer_count, CFG.dummy_count, CFG.chunk_size
        )
        .as_str(),
        |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    total += wfmpsc_bench_iteration();
                }
                total
            })
        },
    );
}

/// Runs a single iteration of our workload scenario and returns measurement
fn wfmpsc_bench_iteration() -> Duration {
    let mut handlers = vec![];
    let prod_counter = Arc::new(AtomicUsize::new(0));
    let total_bytes = 1_000_000 / CFG.producer_count; //2Mb
    let (consumer, prods) = queue!(
        bitsize: { CFG.queue_size },
        producers: { CFG.producer_count }
    );
    core_affinity::set_for_current(CoreId { id: 0 });
    for (idx, p) in prods.into_iter().enumerate() {
        let pc = prod_counter.clone();
        let tmp = std::thread::spawn(move || {
            core_affinity::set_for_current(CoreId { id: idx + 1 });
            while pc.load(Ordering::Acquire) == 0 {} // pseudo semaphore
            push_wfmpsc(p, total_bytes, pc);
        });
        handlers.push(tmp);
    }
    let start = Instant::now();
    // -----------------------------------
    // start measuring
    prod_counter.store(CFG.producer_count, Ordering::Release);
    pop_wfmpsc(consumer, prod_counter);
    // stop measuring
    // -----------------------------------
    let result = start.elapsed();
    for h in handlers {
        h.join().expect("Joining thread");
    }
    result
}

fn push_wfmpsc<
    const T: usize,
    const C: usize,
    const S: usize,
    const L: usize,
    A: ThreadSafeAlloc,
>(
    mut p: TLQ<T, C, S, L, A>,
    bytes: usize,
    prod_counter: Arc<AtomicUsize>,
) {
    let chunk = vec![0u8; CFG.chunk_size];
    let mut written = 0;
    while written < bytes {
        black_box(&mut p);
        written += p.push(&chunk);
        // waste time to reducer queue load
        for mut i in 0..CFG.dummy_count {
            black_box(&mut i);
        }
        black_box(&mut written);
    }
    prod_counter.fetch_sub(1, Ordering::Release);
}

/// Blocking function that empties the MPSCQ until a total number of
/// `elem_count` elements have been popped in total.
fn pop_wfmpsc(c: impl ConsumerHandle, prod_counter: Arc<AtomicUsize>) {
    let mut counter: usize = 0;
    let mut destination_buffer = [0u8; 64]; // uart dummy
    let p_count = c.get_producer_count();
    // this counter is mostly in exclusive state, so checking this atomic
    // variable causes neglible cache coherency traffic
    while prod_counter.load(Ordering::Acquire) != 0 {
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
        .measurement_time(Duration::from_secs(1))
        .warm_up_time(Duration::from_secs(1));
    targets = run_wfmpsc
);
criterion_group!(
    name = accurate;
    config = Criterion::default()
        .sample_size(120)
        .warm_up_time(Duration::from_secs(2));
    targets = run_wfmpsc
);
criterion_main!(accurate);
