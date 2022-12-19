/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use wfmpsc;

fn main() {
    call();
}

fn call() {
    let mpscq = wfmpsc::queue!(
        bitsize: 16,
        producers: 9,
        l1_cache: 128
    );
}
