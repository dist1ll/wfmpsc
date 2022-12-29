pub struct BenchCfg {
    pub queue_size: usize,
    /// Number of producer threads to be spawned
    pub producer_count: usize,
    /// Type of CPU load with which data is pushed into the MPSC queue
    pub load: LoadFactor,
    /// Size of chunks inserted into the queue
    pub chunk_size: usize,
    // /// Burstiness of traffic. 0 means constant, homogeneous load and
    // /// 1 means data is added in very short intensive bursts.
    // burstiness: f64,
}

#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub enum LoadFactor {
    /// Hammering the queue constantly
    #[default]
    Maximum,
    /// Padding with a few dozen instructions
    Medium,
    /// Blocking wait for short time
    Low,
}
pub const fn atoi(s: &'static str) -> usize {
    let mut result = 0usize;
    let mut i = 0;
    while i < s.as_bytes().len() {
        result += (s.as_bytes()[i] - ('0' as u8)) as usize;
        i += 1;
    }
    result
}

pub const fn str_eq(a: &'static str, b: &'static str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let ax = a.as_bytes();
    let bx = b.as_bytes();
    let mut i = 0;
    while i < a.len() {
        if ax[i] != bx[i] {
            return false;
        }
        i += 1;
    }
    true
}
pub const fn conv(s: &'static str) -> LoadFactor {
    if str_eq(s, "maximum") {
        return LoadFactor::Maximum;
    } else if str_eq(s, "medium") {
        return LoadFactor::Medium;
    } else if str_eq(s, "low") {
        return LoadFactor::Low;
    } else {
        panic!("LoadFactor env needs to be in [maximum, medium, low]");
    }
}
