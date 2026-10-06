# SHM benchmark: control-plane frames vs shm bulk blocks (TSK-115)

- Date: 2026-10-06 (UTC)
- Machine: Windows 11 x64, 12th Gen Intel i7-12700H, 16 GiB RAM (local dev box)
- Toolchain: rustc 1.98.1, `--release`
- Command: `cargo run -p synthlm-common --example shm_bench --release`
  (source: `crates/common/examples/shm_bench.rs`)
- Method: loopback on this machine, no network.
  - `frame-1kib`: 1 KiB `score.report` frame echoed over an `interprocess`
    local socket (bind + handshake once, then N echo round-trips).
  - `shm-1mib` / `shm-16mib`: `write_pcm_f32` + `read_pcm_f32` + equality
    check per iteration (segment create + open + copy-out + FNV-1a verify +
    owner unlink on drop). Throughput = payload bytes / mean wall time
    (one direction).

## Results

Run 1 / run 2 (stability spot-check, same box, minutes apart):

| payload | path | iters | mean (run 1) | mean (run 2) | throughput |
|---|---|---|---|---|---|
| 1 KiB control frame | local-socket echo RTT | 500 | 20.7 us | 23.4 us | n/a (control plane) |
| 1 MiB PCM block | shm write+read+verify | 20 | 10.70 ms | 10.73 ms | ~93 MiB/s |
| 16 MiB PCM block | shm write+read+verify | 5 | 64.46 ms | 66.65 ms | ~240 MiB/s |

(The announce frame itself is ~100 B of JSON — same cost class as the 1 KiB
control row above — so shm handoff notification stays in the tens-of-us
range; only bulk bytes move through the segment.)

## Conclusion

- Control plane stays on local-socket frames: ~21–23 us RTT for KiB-scale
  messages, three orders of magnitude below any bulk transfer.
- 16 MiB (the `SHM_THRESHOLD_BYTES` / `MAX_FRAME_BYTES` boundary) cannot ride
  a frame at all and completes the full shm loop in ~65 ms (~240 MiB/s),
  dominated by copies, not by segment create/open syscalls.
- The 1 MiB row (~10.7 ms, ~93 MiB/s) shows fixed per-handoff overhead
  (create/open/unlink + verify) amortized over fewer bytes; even so it is far
  below any cloud/render budget, and 1 MiB payloads still fit frames when the
  caller prefers the simpler path (`should_use_shm` only *requires* shm at or
  above 16 MiB).
- No DEC-023 reversal signal: JSON announce + FNV-1a verify are negligible
  next to the copies; no `prost`/binary-codec migration is indicated by these
  numbers.
