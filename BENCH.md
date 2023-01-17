# Benchmarking Guide


Requirements for running the benchmark: 

- Cargo and rustc ([installation instructions](https://www.rust-lang.org/tools/install))
- Python

You can run the benchmarks from the `eval/` folder. 

```shell
zk@zk eval~: python3 generate_report.py
```

This will create a `report.txt` file that creates measurements for the different
configurations. 

### Customization

You can perform a benchmark across different configurations. In the python script
you find the following section:

```Python
for cache_line in [0, 128]:
    for queue_size in [15]:
        for prod in [1, 3, 5, 9]: # change this per machine
            for dummy in [0, 500]:
                for chunk_size in [1]:
```

As you can see, we're iterating over all possible parameter configurations. The
parameters are explained in the following.

| variable | meaning |
|----------|---------|
| `cache_line` | The alignment of the queue indices (should be either 0 or your cache line size) |
| `queue_size` | `queue capacity = 2 ^ queue_size` |
| `prod`  | Number of concurrent producers |
| `dummy` | Number of dummy instructions inserted between every push to reduce contention |
| `chunk_size` | Number of bytes submitted in every push operation by consumers |

You can set the parameters to suit your particular system.
