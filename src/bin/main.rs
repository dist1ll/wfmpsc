use wfmpsc;

fn main() {
    let mpscq = wfmpsc::queue!(
        bitsize: 16,
        producers: 9,
        l1_cache: 128
    );
    mpscq.get_producer_handle(8);

    //    let alloc = nostd::FixedAllocStub::<0x2ffff, 0x20000> {};
    //    let no_std_queue = mpscq_alloc!(
    //        bitsize: 16,
    //        producers: 9,
    //        l1_cache: 128,
    //        allocator: alloc
    //    );
    //
    //    eprintln!("0x{:x}", no_std_queue as *const _ as usize);
}
