/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

#![feature(test)]
#![feature(allocator_api)]

extern crate test;

#[cfg(test)]
mod _t {
    use test::{black_box, Bencher};
    use wfmpsc::{queue, ConsumerHandle, TLQ};

    #[bench]
    fn eval_single(_: &mut Bencher) {
        eprintln!("Starting...");
        fill_mpscq();
        eprintln!("Done!");
    }

    #[bench]
    fn eval_iter(b: &mut Bencher) {
        b.iter(|| {
            black_box(fill_mpscq());
        })
    }

    fn fill_mpscq() {
        let mut handlers = vec![];
        let (_, prods) = queue!(
            bitsize: 4,
            producers: 8,
            l1_cache: 128
        );
        for (idx, producer) in prods.into_iter().enumerate() {
            let tmp = std::thread::spawn(move || {
                // fill the queue ffs
                fill_mpscq_thread(idx, producer);
            });
            handlers.push(tmp);
        }
        // empty_mpscq_thread(queue.get_consumer_handle(), 8 * ((1 << 16) - 1));
        for h in handlers {
            h.join().expect("Joining thread");
        }
    }

    /// Blocking function that empties the MPSCQ until a total number of
    /// `elem_count` elements have been popped in total.
    fn empty_mpscq_thread(c: impl ConsumerHandle, elem_count: usize) {
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

    fn fill_mpscq_thread<const C: usize, const L: usize>(qid: usize, tlq: TLQ<C, L>) {
        let b = [0u8, 1, 2, 3];
        for _ in 0u64..(2 * (1u64 << C) - 1) {
            black_box(&tlq).push(&b);
        }
        eprintln!("TLQ: #{}\n{}\n", qid, tlq);
    }
}