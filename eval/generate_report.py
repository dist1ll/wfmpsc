# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

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

def run_bench(env, cfg):
    print(f'[x] compiling w/ {prod} threads, {queue_size}-bit width,' + 
        f' {dummy} dummies, {chunk_size}B chunks')
    # current_sub = sub.Popen('cat ../report.txt', # quick testing
    current_sub = sub.Popen(f'cargo rustc --release --bench bench -- {cfg} -Awarnings',
        shell = True,
        stderr = sub.DEVNULL,
        stdout = sub.DEVNULL,
        env = env)
    current_sub.wait()

    rootdir = "../target/release/deps/"
    target = ''
    for root, dirs, files in os.walk(rootdir):
      for file in files:
        if "bench-" in file and ".d" not in file: 
            target = rootdir[3:] + file    
    print(f'  > Running benchmark', end='', flush=True)
    # current_sub = sub.Popen('cat ../report.txt', # quick testing
    current_sub = sub.Popen(f'./{target} --bench',
        shell = True,
        cwd = os.path.abspath('../'),
        stderr = sub.DEVNULL,
        stdout = sub.PIPE,
        env = env)
    raw_out, errs = current_sub.communicate()
    line = raw_out.decode('utf-8').partition('\n')[0]
    result = extract_time(line)
    print('.......DONE')
    
    if cache_line == 0:
        prefix = 'packed'
    else:
        prefix = 'hybrid'

    bench_id = f'{prefix}_q{queue_size}-bit_p{prod}_d{dummy}_c{chunk_size}B chunk'
    with open("report.txt", "a") as f:
        f.write(f'{bench_id};{result}\n')

_env = os.environ.copy()
max_prods = os.cpu_count() - 1

for cache_line in [0, 128]:
    print(f'[x] compiling {cache_line}-bit cache config (may take a while)')
    for queue_size in [16]:
        for prod in [1, 2, 5, 9]: # change this per machine
            for dummy in [0]:
                for chunk_size in [2]:
                    cfg = f'--cfg "cache_line=\\"{cache_line}\\""'
                    _env["WFMPSC_BENCH_PRODUCER_COUNT"] = str(prod)
                    _env["WFMPSC_BENCH_QUEUE_SIZE"] = str(queue_size)
                    _env["WFMPSC_BENCH_DUMMY_INSTRUCTIONS"] = str(dummy)
                    _env["WFMPSC_BENCH_CHUNK_SIZE"] = str(chunk_size)
                    run_bench(_env, cfg)

# print('    > result: ' + result + 'ms')
