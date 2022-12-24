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

#[test]
pub fn overflow_push() {
    let (cons, prod) = wfmpsc::queue!(bitsize: 4, producers: 4, l1_cache: 64);
    prod[0].push("hello_world!123..hi what up".as_bytes());
    let mut dst = [0u8; 16];
    cons.pop_elements_into(0, &mut dst);
    println!("{}", std::str::from_utf8(&dst).unwrap());
}
