/// Creates a compile-time configuration from environment variables. You can
/// set them either in a shell, or via a `.cargo/config.toml` config file.
pub const fn cfg_from_env() -> BenchCfg {
    BenchCfg {
        queue_size: atoi(env!("WFMPSC_BENCH_QUEUE_SIZE")),
        producer_count: atoi(env!("WFMPSC_BENCH_PRODUCER_COUNT")),
        dummy_count: atoi(env!("WFMPSC_BENCH_DUMMY_INSTRUCTIONS")),
        chunk_size: atoi(env!("WFMPSC_BENCH_CHUNK_SIZE")),
    }
}
pub struct BenchCfg {
    pub queue_size: usize,
    /// Number of producer threads to be spawned
    pub producer_count: usize,
    /// Number of dummy instructions inserted between queue ops.
    /// 0: Maximum contention
    /// 10: A bit less contention
    /// 1000: A lot less contention
    pub dummy_count: usize,
    /// Size of chunks inserted into the queue
    pub chunk_size: usize,
    // /// Burstiness of traffic. 0 means constant, homogeneous load and
    // /// 1 means data is added in very short intensive bursts.
    // burstiness: f64,
}

pub const fn atoi(s: &'static str) -> usize {
    let mut result = 0usize;
    let mut i = 0;
    let len = s.as_bytes().len();
    while i < len {
        result += ((s.as_bytes()[i] - ('0' as u8)) as usize)
            * (usize::overflowing_pow(10, (len - i - 1) as u32).0 as usize);
        i += 1;
    }
    result
}
