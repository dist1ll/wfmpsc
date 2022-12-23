/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use wfmpsc::{ConsumerHandle, TLQ};

/**!
This test checks if indexing is correctly implemented for overlapping concurrent
push and pop operations.
*/

pub fn create_queue() -> (impl ConsumerHandle, [TLQ<4, 16>; 4]) {
    let queue = wfmpsc::queue!(
        bitsize: 4,
        producers: 4,
        l1_cache: 64
    );
    // TODO: This split should be done directly by queue!
    let consumer = queue.get_consumer_handle();
    let mut producers: [TLQ<4, 16>; 4] = unsafe { std::mem::zeroed() };
    for (idx, prod) in producers.iter_mut().enumerate() {
        *prod = queue.get_producer_handle(idx as u8);
    }
    (consumer, producers)
}

#[test]
pub fn overflow_push() {}
