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
    fn eval_checked_fill(b: &mut Bencher) {
        fill_mpscq();
        // b.iter(|| {
        //     black_box(fill_mpscq());
        // })
    }

    fn fill_mpscq() {
        let mut handlers = vec![];
        let queue = queue!(
            bitsize: 16,
            producers: 8,
            l1_cache: 128
        );
        for i in 0..8 {
            let tlq = queue.get_producer_handle(i);
            let tmp = std::thread::spawn(move || unsafe {
                // fill the queue ffs
                fill_mpscq_thread(i, tlq);
            });
            handlers.push(tmp);
        }
        empty_mpscq_thread(queue.get_consumer_handle());
        for h in handlers {
            h.join().expect("Joining thread");
        }
    }

    fn empty_mpscq_thread(c: impl ConsumerHandle) {
        loop {}
    }

    fn fill_mpscq_thread<const C: usize, const L: usize>(qid: u8, tlq: TLQ<C, L>) {
        for i in 0u64..((1u64 << C) - 1) {
            black_box(&tlq).push_single(i as u8);
        }
    }
}
