import os
import re
import atexit
import subprocess as sub

current_sub = None

# killing subprocess when parent program ends
def cleanup():
    if not current_sub is None: 
        current_sub.kill()
atexit.register(cleanup)

def extract_time(s: str):
    x = float(s.split(' ')[-4])
    if s.split(' ')[-3] == 's':
        x = x * 1_000
    if s.split(' ')[-3] == 'μs':
        x = x / 1_000
    if s.split(' ')[-3] == 'ns':
        x = x / 1_000_000
    return x

def run_bench(env):
    print(f'[x] compiling w/ {prod} threads, {queue_size}-bit width,' + 
        f' {dummy} dummies, {chunk_size}B chunks')
    # current_sub = sub.Popen('cat ../report.txt', # quick testing
    current_sub = sub.Popen('cargo build --release --bench bench',
        shell = True,
        stderr = sub.DEVNULL,
        stdout = sub.DEVNULL,
        env = env)
    current_sub.wait()
    print(f'  > Running benchmark', end='', flush=True)
    # current_sub = sub.Popen('cat ../report.txt', # quick testing
    current_sub = sub.Popen('cargo bench',
        shell = True,
        stderr = sub.DEVNULL,
        stdout = sub.PIPE,
        env = env)
    raw_out, errs = current_sub.communicate()
    line = raw_out.decode('utf-8').partition('\n')[0]
    result = extract_time(line)
    print('.......DONE')
    bench_id = f'c{cache_line}_q{queue_size}_p{prod}_d{dummy}_c{chunk_size}'
    with open("report.txt", "a") as f:
        f.write(f'{bench_id};{result}\n')

_env = os.environ.copy()
max_prods = os.cpu_count() - 1



print("Running benchmarking suite. Can take a long time")
for cache_line in [0, 128]:
    print(f'[x] compiling {cache_line}-bit cache config (may take a while)')
    for queue_size in [8, 16, 24]:
        for prod in [int(max_prods / 2), max_prods]:
            for dummy in [0, 100, 10_000]:
                for chunk_size in [17, 117]:
                    _env["RUSTFLAGS"] = f'--cfg cache_line="{cache_line}"'
                    _env["WFMPSC_BENCH_PRODUCER_COUNT"] = str(prod)
                    _env["WFMPSC_BENCH_QUEUE_SIZE"] = str(queue_size)
                    _env["WFMPSC_BENCH_DUMMY_INSTRUCTIONS"] = str(dummy)
                    _env["WFMPSC_BENCH_CHUNK_SIZE"] = str(chunk_size)
                    run_bench(_env)

# print('    > result: ' + result + 'ms')
