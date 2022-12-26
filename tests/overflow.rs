/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use wfmpsc::ConsumerHandle;

/**!
This test checks if indexing is correctly implemented for overlapping concurrent
push and pop operations.
*/


/// Check if partial writes are executed correctly on the buffer.
#[test]
pub fn partial_write() {
    let (cons, prod) = wfmpsc::queue!(bitsize: 4, producers: 4);
    // push more than 15 bytes into the queue
    prod[0].push("Hello World, how are you doing".as_bytes());
    //                           ^
    //                     the 'w' is the 16th letter

    let mut dst = [0u8; 15];
    cons.pop_elements_into(0, &mut dst);
    assert_eq!("Hello World, ho", conv(&dst));
}

/// utility function that converts byte slice into utf8 and unwraps
fn conv(b: &[u8]) -> &str {
    core::str::from_utf8(b).unwrap()
}
