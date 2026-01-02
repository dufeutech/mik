window.BENCHMARK_DATA = {
  "lastUpdate": 1767360092391,
  "repoUrl": "https://github.com/dufeut/mik",
  "entries": {
    "Rust Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "23062270+hlop3z@users.noreply.github.com",
            "name": "hlop3z",
            "username": "hlop3z"
          },
          "committer": {
            "email": "23062270+hlop3z@users.noreply.github.com",
            "name": "hlop3z",
            "username": "hlop3z"
          },
          "distinct": true,
          "id": "7471fe1522c4fc1841fa0716b3c5b363a73c812c",
          "message": "fix(ci): use bencher output format for benchmark action\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-02T06:34:43-06:00",
          "tree_id": "d7e845862d6ce7d6fd468cd78cdfa00d773057c6",
          "url": "https://github.com/dufeut/mik/commit/7471fe1522c4fc1841fa0716b3c5b363a73c812c"
        },
        "date": 1767360091704,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 358,
            "range": "± 6",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1380,
            "range": "± 16",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3926,
            "range": "± 110",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 39500,
            "range": "± 221",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 405763,
            "range": "± 5820",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_hit",
            "value": 107,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_miss",
            "value": 87,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 4893,
            "range": "± 1025",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 3231,
            "range": "± 66",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 204351,
            "range": "± 871",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2188,
            "range": "± 47",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8414,
            "range": "± 184",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4083,
            "range": "± 48",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 515,
            "range": "± 17",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 388261,
            "range": "± 5412",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 163373,
            "range": "± 2509",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_single",
            "value": 10,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/10",
            "value": 111,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1106,
            "range": "± 8",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11680,
            "range": "± 42",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 148,
            "range": "± 2",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/1024",
            "value": 11,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/8192",
            "value": 11,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/65536",
            "value": 11,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "store_pool/placeholder_acquire_release",
            "value": 10,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_next",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/2",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/4",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/8",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/16",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/32",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "metrics/counter_increment",
            "value": 2,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "metrics/request_lifecycle",
            "value": 14,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "metrics/counter_concurrent",
            "value": 131722,
            "range": "± 1882",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 135164,
            "range": "± 2066",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 131119,
            "range": "± 1588",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}