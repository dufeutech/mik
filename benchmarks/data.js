window.BENCHMARK_DATA = {
  "lastUpdate": 1767505850445,
  "repoUrl": "https://github.com/dufeutech/mik",
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
          "id": "12920661b0ea84f206faedac9c88039f8ee07bc3",
          "message": "fix(ci): explicit benchmark data path to avoid conflicts\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-02T07:00:09-06:00",
          "tree_id": "9883123381149ecbc5e9a8f45fc6b1fcb1200a58",
          "url": "https://github.com/dufeut/mik/commit/12920661b0ea84f206faedac9c88039f8ee07bc3"
        },
        "date": 1767360155912,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 357,
            "range": "± 6",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1387,
            "range": "± 81",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3903,
            "range": "± 36",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 39302,
            "range": "± 267",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 404925,
            "range": "± 3497",
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
            "value": 88,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 4455,
            "range": "± 1789",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 2833,
            "range": "± 89",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 206348,
            "range": "± 958",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2121,
            "range": "± 25",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8478,
            "range": "± 33",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4065,
            "range": "± 41",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 515,
            "range": "± 27",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 421057,
            "range": "± 11048",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 156197,
            "range": "± 1699",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_single",
            "value": 10,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/10",
            "value": 111,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1106,
            "range": "± 7",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11719,
            "range": "± 47",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 148,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/1024",
            "value": 11,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/8192",
            "value": 11,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/65536",
            "value": 13,
            "range": "± 1",
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
            "value": 129300,
            "range": "± 1673",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 130637,
            "range": "± 1813",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 128075,
            "range": "± 1583",
            "unit": "ns/iter"
          }
        ]
      },
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
          "id": "a14bc0478157d32feee039d166849f8acc968688",
          "message": "ci: fix benchmark git conflict and add caching improvements\n\n- Clean working directory before benchmark action (fixes Cargo.lock conflict)\n- Add apt package caching for faster Linux builds\n- Add shared cache keys for Rust dependencies across jobs\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-02T17:44:35-06:00",
          "tree_id": "6815a6f884b1386c51ec0d863fe7a993f4e3626b",
          "url": "https://github.com/dufeut/mik/commit/a14bc0478157d32feee039d166849f8acc968688"
        },
        "date": 1767398873455,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 362,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1427,
            "range": "± 16",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3951,
            "range": "± 40",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 39951,
            "range": "± 4829",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 409841,
            "range": "± 3455",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_hit",
            "value": 105,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_miss",
            "value": 86,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 4092,
            "range": "± 2963",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 2786,
            "range": "± 51",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 207029,
            "range": "± 1005",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2219,
            "range": "± 42",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8449,
            "range": "± 113",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4017,
            "range": "± 41",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 505,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 382800,
            "range": "± 19659",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 159796,
            "range": "± 2389",
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
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1105,
            "range": "± 3",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11720,
            "range": "± 45",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 150,
            "range": "± 3",
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
            "value": 10,
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
            "value": 122224,
            "range": "± 896",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 125150,
            "range": "± 1616",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 121738,
            "range": "± 833",
            "unit": "ns/iter"
          }
        ]
      },
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
          "id": "8f69c82afa42ac35a7d75f51d91560d277230798",
          "message": "chore: update GitHub org from dufeut to dufeutech\n\nUpdate all references to the new organization URL across:\n- Repository URLs (Cargo.toml, install scripts)\n- Documentation links (README, docs site)\n- OCI registry references (ghcr.io)\n- Example Cargo.toml files (mik-sdk dependency)\n- Related project links\n- systemd service documentation\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-02T22:38:13-06:00",
          "tree_id": "012d3fef9df8f0b4f8b51fad8de00920f8b1405d",
          "url": "https://github.com/dufeutech/mik/commit/8f69c82afa42ac35a7d75f51d91560d277230798"
        },
        "date": 1767416314329,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 361,
            "range": "± 6",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1384,
            "range": "± 46",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3928,
            "range": "± 33",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 40018,
            "range": "± 357",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 410368,
            "range": "± 3596",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_hit",
            "value": 105,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_miss",
            "value": 86,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 4153,
            "range": "± 1215",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 3109,
            "range": "± 41",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 205368,
            "range": "± 659",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2264,
            "range": "± 50",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8421,
            "range": "± 104",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4049,
            "range": "± 50",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 504,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 380841,
            "range": "± 5387",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 155379,
            "range": "± 1769",
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
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1107,
            "range": "± 4",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11564,
            "range": "± 36",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 151,
            "range": "± 1",
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
            "value": 121975,
            "range": "± 1477",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 124673,
            "range": "± 1000",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 121925,
            "range": "± 2001",
            "unit": "ns/iter"
          }
        ]
      },
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
          "id": "829b6156a07b36d4b6f692abd432fcfdf33ffe74",
          "message": "ci: add pre-release workflow for testing builds\n\nAdds manual workflow to create pre-release builds for testing:\n- Validates tag format (v0.1.0-rc.1, v0.1.0-beta.1, etc.)\n- Builds for all 6 target platforms\n- Creates GitHub pre-release with install instructions\n- Does not modify Cargo.toml version\n\nTrigger via Actions > Pre-Release > Run workflow\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-03T05:05:31-06:00",
          "tree_id": "a83f44e1fa392c2ae51a763f6676e74d7d18ba87",
          "url": "https://github.com/dufeutech/mik/commit/829b6156a07b36d4b6f692abd432fcfdf33ffe74"
        },
        "date": 1767440581316,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 361,
            "range": "± 5",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1434,
            "range": "± 28",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3921,
            "range": "± 45",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 40308,
            "range": "± 265",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 413013,
            "range": "± 3037",
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
            "value": 89,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 3835,
            "range": "± 2099",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 2763,
            "range": "± 31",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 205858,
            "range": "± 784",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2260,
            "range": "± 46",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8377,
            "range": "± 151",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4058,
            "range": "± 68",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 497,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 368450,
            "range": "± 4238",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 148996,
            "range": "± 1407",
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
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1105,
            "range": "± 3",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11580,
            "range": "± 49",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 148,
            "range": "± 0",
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
            "value": 126387,
            "range": "± 1774",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 129150,
            "range": "± 1150",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 126152,
            "range": "± 1211",
            "unit": "ns/iter"
          }
        ]
      },
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
          "id": "d7148ee2cf578c70dd0017377e9fd3a68ba50f76",
          "message": "docs: add multi-tenant routing guide and example configs\n\nAdd comprehensive documentation for the multi-tenant routing feature\nincluding route patterns, isolation guarantees, and configuration.\n\nAdd example mik.toml configurations for common use cases:\n- minimal, multi-module, multi-tenant\n- production, with-scripts, with-load-balancer\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-03T12:22:47-06:00",
          "tree_id": "2c92e508e2682403e85ece2b50ca91219cc4e3d0",
          "url": "https://github.com/dufeutech/mik/commit/d7148ee2cf578c70dd0017377e9fd3a68ba50f76"
        },
        "date": 1767472644239,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 362,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1378,
            "range": "± 14",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3938,
            "range": "± 74",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 39716,
            "range": "± 896",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 406740,
            "range": "± 6137",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_hit",
            "value": 107,
            "range": "± 2",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_miss",
            "value": 88,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 3942,
            "range": "± 1009",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 2789,
            "range": "± 32",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 205052,
            "range": "± 2784",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2205,
            "range": "± 44",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8411,
            "range": "± 296",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4111,
            "range": "± 36",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 488,
            "range": "± 7",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 388690,
            "range": "± 6569",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 163482,
            "range": "± 1484",
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
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1104,
            "range": "± 8",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11537,
            "range": "± 39",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 148,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/1024",
            "value": 10,
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
            "value": 133110,
            "range": "± 1711",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 142111,
            "range": "± 2434",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 133049,
            "range": "± 3881",
            "unit": "ns/iter"
          }
        ]
      },
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
          "id": "9e1dad768d8ac28a76701cdc001ab826d7a276c9",
          "message": "docs: add multi-tenant guide to sidebar\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-03T14:47:25-06:00",
          "tree_id": "18a66e26a881ed48c182bb534413b3889e8858a5",
          "url": "https://github.com/dufeutech/mik/commit/9e1dad768d8ac28a76701cdc001ab826d7a276c9"
        },
        "date": 1767474009141,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 360,
            "range": "± 12",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1393,
            "range": "± 18",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 3934,
            "range": "± 43",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 39665,
            "range": "± 387",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 410689,
            "range": "± 3333",
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
            "value": 88,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 3894,
            "range": "± 1748",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 2710,
            "range": "± 36",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 204773,
            "range": "± 1468",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2197,
            "range": "± 71",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 8478,
            "range": "± 43",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4045,
            "range": "± 35",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 501,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 385205,
            "range": "± 5995",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 162738,
            "range": "± 1644",
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
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 1105,
            "range": "± 31",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 11690,
            "range": "± 76",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 148,
            "range": "± 0",
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
            "value": 129142,
            "range": "± 2216",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 135054,
            "range": "± 2967",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 129585,
            "range": "± 1893",
            "unit": "ns/iter"
          }
        ]
      },
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
          "id": "df3b895603d3b7c52d26e9f1fd6bfdd63628bb2d",
          "message": "chore: remove stale Phase B test placeholders\n\n- Delete tests/runtime/ directory (orphaned, never executed)\n- Remove 17 ignored integration tests for unimplemented features\n- Remove duplicate property tests (tests/property_tests.rs is canonical)\n- Clean up Phase B TODO references in runtime benchmarks\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nCo-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>",
          "timestamp": "2026-01-03T23:35:44-06:00",
          "tree_id": "d6be948398f9e0960861234f18eedae0ce3b2b56",
          "url": "https://github.com/dufeutech/mik/commit/df3b895603d3b7c52d26e9f1fd6bfdd63628bb2d"
        },
        "date": 1767505849656,
        "tool": "cargo",
        "benches": [
          {
            "name": "circuit_breaker/check_request_closed",
            "value": 491,
            "range": "± 6",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_and_record_success",
            "value": 1825,
            "range": "± 14",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/10",
            "value": 5725,
            "range": "± 31",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/100",
            "value": 56456,
            "range": "± 320",
            "unit": "ns/iter"
          },
          {
            "name": "circuit_breaker/check_multiple_keys/1000",
            "value": 560266,
            "range": "± 4623",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_hit",
            "value": 124,
            "range": "± 6",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_miss",
            "value": 80,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_insert",
            "value": 6827,
            "range": "± 1700",
            "unit": "ns/iter"
          },
          {
            "name": "module_cache/cache_with_eviction",
            "value": 4834,
            "range": "± 154",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/runtime_create",
            "value": 215724,
            "range": "± 817",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_simple",
            "value": 2282,
            "range": "± 54",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_json_transform",
            "value": 7780,
            "range": "± 85",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/eval_function_call",
            "value": 4107,
            "range": "± 18",
            "unit": "ns/iter"
          },
          {
            "name": "script_execution/script_preprocess",
            "value": 511,
            "range": "± 2",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/circuit_breaker_concurrent",
            "value": 471893,
            "range": "± 6851",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent_access/module_cache_concurrent",
            "value": 129400,
            "range": "± 1545",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_single",
            "value": 32,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/10",
            "value": 338,
            "range": "± 1",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/100",
            "value": 3396,
            "range": "± 10",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_batch/1000",
            "value": 34031,
            "range": "± 99",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_exhausted",
            "value": 109,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/1024",
            "value": 32,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/8192",
            "value": 32,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "buffer_pool/acquire_release_size/65536",
            "value": 32,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "store_pool/pool_acquire_release",
            "value": 32,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_next",
            "value": 9,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/2",
            "value": 9,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/4",
            "value": 9,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/8",
            "value": 9,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/16",
            "value": 9,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "scheduling/round_robin_workers/32",
            "value": 9,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "metrics/counter_increment",
            "value": 6,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "metrics/request_lifecycle",
            "value": 33,
            "range": "± 0",
            "unit": "ns/iter"
          },
          {
            "name": "metrics/counter_concurrent",
            "value": 84270,
            "range": "± 1167",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/buffer_pool_contention",
            "value": 91544,
            "range": "± 1008",
            "unit": "ns/iter"
          },
          {
            "name": "concurrent/mixed_workload_read_heavy",
            "value": 82483,
            "range": "± 1287",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}