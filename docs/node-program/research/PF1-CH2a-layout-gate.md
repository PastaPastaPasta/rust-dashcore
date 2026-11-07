# PF1: the CH2a layout gate

The layout gate of CH2 §2.3 and §7.1 a1, under the policy of rev4 and rev5 (DEC-95; DEC-99 and DEC-102: the native ledger is the only counter). It decides which `CoinLayerOps` layout `dashd-chainstate` keeps:
- the **arena**: 72-byte slots in 3,640-slot chunks behind a `HashTable<u32>` index;
- the **fallback**: 64 hashbrown shards of the same slots, presized at the layer's creation, with rev4's shard trigger.

The loser is deleted in the same PR.

**Decision: the fallback ships; the arena is deleted.**
- **Density.** The arena fails the density bar. The gate keeps it only if r_a/r_f ≤ 0.8 both at B_DEC43 and as the sweep's geometric mean. On the mainnet trace, r_a/r_f is **0.9067** at B_DEC43 and **0.8800** as the geometric mean.
- **Latency.** The arena also fails the latency bar: 1.65× the fallback's L3 misses per hit, against a 1.1× limit. That alone decides.
- **The fallback's floor** holds at all 17 budgets: N_res / N_core ≥ **1.043**, narrowest at 4 MiB. At B_DEC43 the fallback holds 2,676,754 entries, 1.216× Core's 2,200,586. Core's count is taken at a P2PKH coin's 115.67 B, measured at 1 M coins (see "Core's floor" below). Core's bucket share falls as low as 8 B, so N_core can be up to 3.2% higher; the floor is still ≥ 1.010 and the lead at B_DEC43 ≥ 1.178×.
- **No shard grows while no F is in flight** in the gate's measurement (one fill per budget), at B_DEC43 (the gated budget, r6 m4) and at the other 16 budgets too. Over 10 passes of repeated freezes none does at B_DEC43 either, but some do at 76.99 MiB and below (see the stalls section). With F in flight the shards do grow, 9 to 13 doublings per budget.

## Method

**Harness.** `dashd-chainstate/benches/coins_layer_density.rs` (feature `alloc-tag`). It drives the real layer implementations, not a model. Two commits give the numbers:
- **`d5c56b8d`**, the last commit with both layouts, gives the arena: its trace numbers (policy `ArenaThr`), the latency comparison, and its worst single block. The latency, sign-test and per-coin numbers were measured on a clean tree at 12b8688a, `d5c56b8d`'s first version, whose bench `d5c56b8d` only extends by the `trace` and `worst` modes. The trace numbers were measured before `trace` was committed, and a re-run at `d5c56b8d` reproduced all 17 `ArenaThr` rows exactly.
- **The branch head** gives the fallback as it ships, under rev5's policy.

The arena's code did not change after `d5c56b8d`, and neither rev4 nor rev5 changed a rule for it: its THRESHOLD is still allocated bytes ≥ f·B, and it is not presized (it allocates chunks and grows its index as it fills), so m6 has nothing to cap. Its `d5c56b8d` numbers stand under rev5.

**The gate's measurement.** `trace` mode replays ST1's mainnet DSTRACE1 trace (`/work/bench-store/traces/mainnet-453371.trace`: heights 1..453,371, 8,209,924 creates, 7,200,446 spends) block by block into the layers, spends included. Each event leaves active as Core's cache would:
- A create is DIRTY | FRESH, or DIRTY alone over a DIRTY tombstone.
- A spend erases a FRESH entry, and otherwise leaves a DIRTY | SPENT tombstone, for a coin in F or on disk.

The budget is checked only at block boundaries, with §2.5's policy (rev4):
- With no F, active freezes into F at THRESHOLD: its allocated bytes reach f·B (f = 1/2), or, for the fallback, a shard of it holds at least its trigger, 55/64 of its buckets (`ShardedLayer::at_shard_trigger`).
- The new active is presized at f·(B − R − B/16), or at what is left of B − R − B/16 beside F if that is less, but at least 256 buckets a shard (rev5, O-R6-m6).
- With F in flight, nothing freezes, and V waits at Core's CRITICAL: Σ > B, where Σ = F's allocated bytes + active's + R, and R = S + 64 KiB is the reserve (`EngineTuning::batch_reserve`; DEC-99's "adapter reserve").
- At every boundary the run asserts that the tagging allocator's coins bytes equal the layers' `allocated_bytes()` exactly: DEC-99's native ledger. After each budget it asserts that every tagged byte was freed.

`N_res` is the entries in F and active, tombstones included, at the first boundary where V waits. That is the design's measurement point. Then r = B / N_res. Past the trace's end, the replay repeats it with fresh keys: at 1 GiB the first wait comes after about 4.8 M blocks, about 10.6 passes.

The budgets are B_DEC43 = 252.75 MiB (Core's coins share at `-dbcache=300` with txindex on) and 16 log-uniform budgets from 4 MiB to 1 GiB.

**The fallback's layer (rev4).** A shard has room for inserts up to hashbrown's growth load, 7/8 of its buckets. hashbrown marks a removed entry's bucket DELETED, which still uses up that room. When an insert finds a shard's room used up while the shard holds fewer entries than its trigger, the shard is rebuilt at its size with the removed entries dropped. It doubles only at or above its trigger. The trace mode counts the rebuilds, the entries they move, and the time of the ops that ran them, per block.

**Core's floor.** `N_core(B) = max(0.9·B, B − 10 MiB) / 115.67 B` is Core's resident count at its LARGE flush threshold, using Core's bytes per cached P2PKH coin (CH2 §2.3). That is the baseline behind every N_res / N_core ratio here (1.216× at B_DEC43). Its script part is Core's least: a script of up to 28 bytes, P2PKH's 25 or P2SH's 23, sits inline in `CScript`'s `prevector<28>`, and a tombstone's is empty, so each costs the same. A longer script adds its heap block, as a compressed P2PK's 35 bytes take Core to about 180 B. Its bucket part is not: 115.67 B is a 104.05 B pool node plus 11.62 B of buckets at 1 M coins, and the bucket share ranges from 8 to 16 B with the map's load (CH2 §2.3). At 8 B a coin costs Core ≈ 112.05 B, and N_core is up to 3.2% higher than the tables'. The ratios are therefore not strict lower bounds; at worst the 4 MiB floor is ≥ 1.010 and the lead at B_DEC43 ≥ 1.178×.

**Latency** (both layouts, so measured at `d5c56b8d`, on the per-coin build of the B_DEC43 state):
- **PMU.** `perf stat --control` counts around n lookups (`arena` against `fb-grow`), pinned to one CPU. Each per-hit count is the slope between n = 2 M and n = 12 M, net of the key-generation baseline measured the same way in the same binary.
- **LLC misses.** The VM's PMU does not expose `LLC-load-misses`, so `mem_load_uops_retired.l3_miss:u` stands in for it (memory note `agentbox-perf-counters`).
- **Sign test.** A paired wall-clock test: 5 rotations of 20 M hits in the frozen layer, alternating which layout goes first.
- **Load.** agentbox has no reserved bench window (no `~/.config/node-program/bench-windows`), so the sign test ran under the box's normal load (load average 20–30 on 32 vCPUs). It confirms the PMU result; it does not decide on its own.

The fallback's state at `d5c56b8d` (the `fb-grow` reading) is not the shipped one. But a hit's path, the probe of one shard, is the same in both, apart from long coins.

**Reproduce.** Build with:

```
cargo bench -p dashd-chainstate --features alloc-tag --bench coins_layer_density --no-run
```

Then, at the head:

```
<bin> trace /work/bench-store/traces/mainnet-453371.trace   # the gate's table, rebuilds, floor and no-growth lines
<bin> sweep                                                 # the per-coin cross-check
<bin> worst                                                 # the worst single block
<bin> stalls /work/bench-store/traces/mainnet-453371.trace [passes]  # V's stalls with and without m6's rule (1 and 10 passes here)
<bin> timing                                                # iter_dirty throughput
```

At `d5c56b8d` the same `trace`, `sweep` and `worst` modes run both layouts (trace policies `ArenaThr`, `FbThr`, `FbGrow` and `FbWait`; only `ArenaThr` is used here), and these run too:

```
<bin> latency <arena|fallback> <get|path|base> <n>          # under perf stat --control, PERF_CTL_FIFO/PERF_ACK_FIFO
<bin> timing 20000000 5                                     # the sign test and iter_dirty throughput
```

A trace sweep takes 7–12 minutes and 1.9 GB; `stalls` takes about 2 minutes and 2.7 GB for one pass, and 14 minutes and 4.4 GB for 10.

## Density (the trace)

The arena column comes from `d5c56b8d`, the fallback's from the head. r is in budget bytes per resident entry.

| B (MiB) | N_core (P2PKH, 115.67 B) | arena N_res | r | fallback N_res | r | fallback / N_core | N_fb / N_arena |
|---|---:|---:|---:|---:|---:|---:|---:|
| 4.00 | 32,635 | 36,413 | 115.19 | 34,032 | 123.25 | 1.043 | 0.9346 |
| 5.79 | 47,231 | 58,577 | 103.63 | 50,370 | 120.51 | 1.066 | 0.8599 |
| 8.38 | 68,357 | 87,518 | 100.38 | 76,312 | 115.12 | 1.116 | 0.8720 |
| 12.13 | 98,930 | 128,484 | 98.96 | 111,699 | 113.83 | 1.129 | 0.8694 |
| 17.55 | 143,179 | 187,667 | 98.05 | 162,226 | 113.43 | 1.133 | 0.8644 |
| 25.40 | 207,218 | 276,828 | 96.20 | 240,669 | 110.66 | 1.161 | 0.8694 |
| 36.76 | 299,901 | 404,557 | 95.27 | 354,136 | 108.84 | 1.181 | 0.8754 |
| 53.20 | 434,037 | 593,422 | 94.00 | 512,520 | 108.84 | 1.181 | 0.8637 |
| 76.99 | 628,169 | 859,357 | 93.95 | 750,081 | 107.63 | 1.194 | 0.8728 |
| 111.43 | 919,491 | 1,260,330 | 92.71 | 1,095,219 | 106.68 | 1.191 | 0.8690 |
| 161.27 | 1,371,297 | 1,874,646 | 90.21 | 1,648,613 | 102.57 | 1.202 | 0.8794 |
| 233.40 | 2,025,183 | 2,817,568 | 86.86 | 2,452,411 | 99.80 | 1.211 | 0.8704 |
| **252.75 (DEC-43)** | **2,200,586** | **2,952,162** | **89.77** | **2,676,754** | **99.01** | **1.216** | **0.9067** |
| 337.79 | 2,971,531 | 4,040,557 | 87.66 | 3,633,446 | 97.48 | 1.223 | 0.8992 |
| 488.88 | 4,341,151 | 6,220,852 | 82.40 | 5,372,113 | 95.42 | 1.237 | 0.8636 |
| 707.54 | 6,323,360 | 8,801,715 | 84.29 | 7,843,789 | 94.59 | 1.240 | 0.8912 |
| 1024.00 | 9,192,151 | 12,801,021 | 83.88 | 11,563,539 | 92.86 | 1.258 | 0.9033 |

**The gate's density lines.** r_a/r_f equals N_f/N_a at equal B. The arena is kept only if both are ≤ 0.8.

| Pair | r_a/r_f at B_DEC43 | Geometric mean over the sweep | Density says |
|---|---|---|---|
| arena / fallback (rev5) | 0.9067 | 0.8800 | fallback |

**The floor.** N_res ≥ N_core is required at every sweep point.

| Layout | Min N_res / N_core | Where | Floor |
|---|---|---|---|
| arena | 1.116 | 4 MiB | holds |
| fallback (rev5) | 1.043 | 4 MiB | holds |

The r6 review expected the trigger to take about 1/56 off rev3's `grow` margin (1.054), leaving 1.025–1.035. The margin is 1.043. Two changes moved it, in opposite directions:
- **rev4's rebuild raised it.** rev3's rebuild stopped at 13/16 of a shard's buckets, so a shard between 13/16 and 7/8 load with tombstones in it doubled. rev4 rebuilds it up to the trigger. Before m6 the margin was 1.083: at 4 MiB F held 17,150 entries and active 18,207, against 17,240 under `grow`. At B_DEC43 each layer holds more than under `grow` (F 1,328,120 against 1,293,050; active 1,348,634 against 1,290,363).
- **m6 lowered it at small budgets.** F's allocated bytes are its presized tables plus its long coins (the boxes of coins over 34 B, and their slab): 110 KB to 1.5 MB on the trace. Where those pass the gap between the presize and the tables `ShardSizes::for_bytes` rounds it down to, F passes the presize, and m6 gives the second layer less. At 4 MiB that is 1,513,478 bytes against 1,671,168: 80 steps of 256 buckets against 89, and 16,882 entries when V waits against 18,207. m6 binds at 9 of the 17 budgets. From 53.20 MiB up the smaller request rounds to the same buckets, so no row changes there.

In bytes, with F as it froze (`trace`'s `alloc1`, `tables1` and `presize2` columns), and N2 the second layer's entries where V waits:

| B (MiB) | presize | F allocated | F tables | F long coins | second presize | N2 |
|---|---:|---:|---:|---:|---:|---:|
| 4.00 | 1,671,168 | 1,828,858 | 1,666,304 | 162,554 | 1,513,478 | 16,882 |
| 5.79 | 2,433,287 | 2,558,358 | 2,432,512 | 125,846 | 2,308,216 | 25,621 |
| 8.38 | 3,536,276 | 3,626,989 | 3,516,416 | 110,573 | 3,445,564 | 39,399 |
| 12.13 | 5,132,600 | 5,208,504 | 5,086,208 | 122,296 | 5,056,696 | 57,318 |
| 17.55 | 7,442,910 | 7,534,812 | 7,403,520 | 131,292 | 7,351,009 | 84,689 |
| 25.40 | 10,786,551 | 10,912,727 | 10,767,360 | 145,367 | 10,660,376 | 123,843 |
| 36.76 | 15,625,699 | 15,712,496 | 15,551,488 | 161,008 | 15,538,903 | 179,820 |
| 53.20 | 22,629,251 | 22,609,401 | 22,428,672 | 180,729 | 22,629,251 | 261,171 |
| 76.99 | 32,765,275 | 32,809,624 | 32,594,944 | 214,680 | 32,720,926 | 381,601 |
| 111.43 | 47,434,830 | 47,567,638 | 47,246,336 | 321,302 | 47,302,023 | 552,898 |
| 161.27 | 70,846,002 | 70,833,353 | 70,568,960 | 264,393 | 70,846,002 | 831,878 |
| 233.40 | 106,299,862 | 105,661,591 | 105,253,888 | 407,703 | 106,299,862 | 1,235,687 |
| **252.75 (DEC-43)** | 115,810,304 | 115,257,357 | 114,822,144 | 435,213 | 115,810,304 | 1,348,634 |
| 337.79 | 157,611,143 | 156,051,798 | 155,487,232 | 564,566 | 157,611,143 | 1,827,853 |
| 488.88 | 231,872,381 | 230,368,517 | 229,641,216 | 727,301 | 231,872,381 | 2,702,892 |
| 707.54 | 339,348,387 | 336,064,129 | 334,892,032 | 1,172,097 | 339,348,387 | 3,937,711 |
| 1024.00 | 494,895,104 | 494,269,728 | 492,768,256 | 1,501,472 | 494,895,104 | 5,802,996 |

**Peak Σ** at each budget, as Σ / B: first the largest Σ at any boundary, then the in-block peak of the coins bytes plus R. The in-block peak includes growth and rebuild transients, and R counts in it whether or not F was in flight.

| B (MiB) | arena | fallback |
|---|---|---|
| 4.00 | 1.017 / 1.017 | 1.003 / 1.012 |
| 5.79 | 1.037 / 1.037 | 1.006 / 1.012 |
| 8.38 | 1.015 / 1.015 | 1.008 / 1.016 |
| 12.13 | 1.018 / 1.044 | 1.001 / 1.006 |
| 17.55 | 1.006 / 1.006 | 1.007 / 1.016 |
| 25.40 | 1.006 / 1.011 | 1.005 / 1.016 |
| 36.76 | 1.007 / 1.007 | 1.005 / 1.013 |
| 53.20 | 1.003 / 1.003 | 1.009 / 1.020 |
| 76.99 | 1.002 / 1.002 | 1.004 / 1.011 |
| 111.43 | 1.002 / 1.002 | 1.004 / 1.009 |
| 161.27 | 1.000 / 1.000 | 1.005 / 1.012 |
| 233.40 | 1.000 / 1.000 | 1.004 / 1.014 |
| 252.75 | 1.000 / 1.000 | 1.005 / 1.014 |
| 337.79 | 1.000 / 1.000 | 1.002 / 1.009 |
| 488.88 | 1.000 / 1.000 | 1.001 / 1.011 |
| 707.54 | 1.000 / 1.000 | 1.006 / 1.013 |
| 1024.00 | 1.000 / 1.000 | 1.003 / 1.008 |

The largest over the sweep are 1.037 at a boundary and 1.044 inside a block for the arena, and 1.009 and 1.020 for the fallback. Σ passes B at the wait boundary by construction, since the measurement point is the first boundary with Σ > B. With checks only at Core's points, a block can carry Σ past B, as it carries Core's usage past its CRITICAL threshold.

**The trace's coins are short.** The longest is 42 B, so the gate never meets a coin over 8 KiB; the long-coin witnesses (CH2 §7.1 a2) cover those. But 14,775 of its 8,209,924 creates pass the 34 inline bytes, and they are what F holds beyond its tables above.

## Shard growth and rebuilds (the fallback, the trace)

rev4 asks a1 to assert that no shard grows while no F is in flight on the mainnet trace, and to measure the rebuilds' frequency and cost. The r6 review (m4) moved the gate to B_DEC43 alone, since the trigger's margin is a few entries at small B. The harness counts doublings at every boundary, in both layers' fills, and fails if any happens with no F in flight at B_DEC43. "Blocks" counts every boundary of the build, both fills. Per-block rebuild times are the wall time of the ops that ran a rebuild, over the blocks that had one, under the box's load (load average about 55–110). They are from one run; a second run of the same code gave 0.97 ms, 3.99 ms and 4.49 ms at B_DEC43 for the median, p99 and maximum, against 0.97 ms, 3.05 ms and 3.42 ms here. The counts were identical.

| B (MiB) | blocks | grew no F | grew F | rebuilds | entries moved | total ms | blocks rebuilding | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 4.00 | 15,720 | 0 | 13 | 80 | 20,671 | 0.8 | 62 | 0.009 | 0.061 | 0.061 |
| 5.79 | 19,670 | 0 | 11 | 38 | 15,432 | 0.6 | 29 | 0.016 | 0.073 | 0.073 |
| 8.38 | 30,445 | 0 | 13 | 62 | 35,811 | 1.6 | 56 | 0.023 | 0.116 | 0.116 |
| 12.13 | 37,649 | 0 | 10 | 9 | 7,792 | 0.4 | 9 | 0.027 | 0.121 | 0.121 |
| 17.55 | 45,121 | 0 | 12 | 22 | 28,768 | 1.6 | 20 | 0.051 | 0.301 | 0.301 |
| 25.40 | 55,846 | 0 | 10 | 32 | 64,593 | 3.2 | 25 | 0.068 | 0.487 | 0.487 |
| 36.76 | 64,172 | 0 | 12 | 25 | 62,989 | 2.1 | 22 | 0.070 | 0.371 | 0.371 |
| 53.20 | 79,751 | 0 | 11 | 38 | 159,176 | 6.4 | 33 | 0.103 | 0.742 | 0.742 |
| 76.99 | 155,550 | 0 | 10 | 150 | 852,033 | 35.2 | 144 | 0.219 | 1.085 | 1.213 |
| 111.43 | 247,056 | 0 | 12 | 257 | 2,186,828 | 85.4 | 241 | 0.252 | 1.562 | 2.117 |
| 161.27 | 493,987 | 0 | 10 | 238 | 2,922,936 | 107.2 | 223 | 0.452 | 1.443 | 1.684 |
| 233.40 | 803,546 | 0 | 11 | 463 | 8,536,061 | 337.2 | 453 | 0.537 | 2.949 | 7.531 |
| **252.75 (DEC-43)** | 991,561 | 0 | 10 | 405 | 7,992,926 | 345.2 | 391 | 0.965 | 3.047 | 3.423 |
| 337.79 | 1,246,457 | 0 | 10 | 492 | 13,733,507 | 581.0 | 473 | 1.107 | 4.079 | 12.453 |
| 488.88 | 2,085,322 | 0 | 10 | 347 | 14,277,364 | 598.3 | 340 | 1.781 | 6.285 | 6.750 |
| 707.54 | 3,229,440 | 0 | 9 | 517 | 31,068,462 | 1422.1 | 513 | 2.332 | 7.632 | 36.793 |
| 1024.00 | 4,823,444 | 0 | 10 | 342 | 30,148,267 | 1096.7 | 340 | 3.588 | 7.709 | 15.379 |

- **No F in flight:** in the gate's one fill per budget, no shard doubled at any budget, 4 MiB included; at small B the active layer fills during mainnet's early, small blocks. Over repeated freezes shards do double with no F in flight at 76.99 MiB and below, and none did at B_DEC43 (the stalls section below).
- **The trigger's margin is not always 1/64 of a shard's buckets.** A rebuild leaves room for over 1/64 of them, but removes that leave DELETED buckets use the room up again while the shard stays below its trigger, until the next rebuild. So when a block takes a shard to its trigger, the room left can be as little as one insert, and the shard can double inside the block, before the boundary freezes the layer (DEC-95 allows it). rev5 §2.3's estimate (a block of ≈ 20,000 entries ordinarily grows no shard at B_DEC43) assumes freshly rebuilt shards. The open questions below take this up.
- **F in flight:** shards do grow, 9 to 13 doublings before V waits, as rev3 and rev4 expect (the tagging allocator counts them in Σ at every boundary).
- **Rebuilds:** at B_DEC43, 405 rebuilds over 991,561 blocks, so about 1 block in 2,500 runs one. They moved 7,992,926 entries in 345 ms. Per rebuilding block the time is 0.97 ms at the median, 3.05 ms at p99 and 3.42 ms at most. From 53.20 MiB up, where m6 changes no row, the counts equal the run without m6's rule; only the times differ, by the box's load. Over all blocks the p99 is 0. Against connect-latency metric #5 (§5.1: at most dashd's p50 and p95 at the tip), a rebuild adds 3.05 ms at p99 to one block in 2,500 on this trace. That can raise a p95 at most to the old p95.04, and a p50 to the old p50.04. No run has yet measured dashd's metric #5, so there is no figure in ms to set it against. Each rebuild allocates one shard's table while the old one lives (2.28 MiB for a presized 32,768-bucket shard at B_DEC43, twice that for one that has doubled), counted in native Σ and inside the peak above.

## The worst single block

rev4 §2.3: a 2 MB block of minimum-size outputs, arriving when active, with no F in flight, is one coin short of its THRESHOLD freeze. `worst` fills active one coin a boundary with the per-coin stream until the next coin would freeze it, then inserts one transaction's `MAX_OUTPUTS_PER_BLOCK` = 222,222 outputs, value 0 and an empty script (5 coin bytes each, inline), with no boundary between them. **Kept** is the layer's allocated bytes after the block minus before. **Transient** is the tagged peak during the block minus the bytes after it. **Σ frozen** is the layer after the block plus R and a new active, for the fallback presized either in full or by m6's rule: what Σ is once that layer freezes at the boundary. It is reported, not gated (DEC-95 makes the peak evidence).

**The arena** (at `d5c56b8d`; its THRESHOLD is allocated bytes ≥ f·B, and it allocates a new active's chunks as it fills):

| B (MiB) | N before | kept (MiB) | transient (MiB) | index doublings | block ms | Σ frozen / B |
|---|---:|---:|---:|---:|---:|---:|
| 4.00 | 25,480 | 17.84 | 0.00 | 4 | 162 | 5.081 |
| 5.79 | 36,400 | 17.68 | 0.00 | 3 | 152 | 3.679 |
| 8.38 | 54,600 | 17.68 | 0.00 | 3 | 144 | 2.730 |
| 12.13 | 76,440 | 17.37 | 0.00 | 2 | 159 | 2.049 |
| 17.55 | 114,688 | 17.12 | 0.00 | 2 | 151 | 1.596 |
| 25.40 | 163,800 | 16.75 | 0.00 | 1 | 132 | 1.280 |
| 36.76 | 229,376 | 16.50 | 0.00 | 1 | 106 | 1.046 |
| 53.20 | 349,440 | 18.00 | 0.00 | 1 | 240 | 0.963 |
| 76.99 | 484,120 | 15.50 | 0.00 | 0 | 97 | 0.824 |
| 111.43 | 735,280 | 20.50 | 2.25 | 1 | 396 | 0.808 |
| 161.27 | 1,026,480 | 15.50 | 0.00 | 0 | 111 | 0.695 |
| 233.40 | 1,550,640 | 15.50 | 0.00 | 0 | 92 | 0.635 |
| **252.75 (DEC-43)** | 1,692,600 | 25.50 | 4.50 | 1 | 765 | 0.664 |
| 337.79 | 2,165,800 | 15.50 | 0.00 | 0 | 75 | 0.593 |
| 488.88 | 3,265,080 | 15.50 | 0.00 | 0 | 71 | 0.565 |
| 707.54 | 4,564,560 | 15.50 | 0.00 | 0 | 138 | 0.545 |
| 1024.00 | 6,865,040 | 15.50 | 0.00 | 0 | 196 | 0.531 |

**The fallback** (the head):

| B (MiB) | N before | kept (MiB) | transient (MiB) | doublings | most per shard | block ms | Σ frozen / B, full presize | Σ frozen / B, m6 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 4.00 | 17,642 | 23.79 | 0.14 | 256 | 4 | 118 | 6.884 | 6.773 |
| 5.79 | 25,729 | 34.75 | 0.29 | 256 | 4 | 128 | 6.941 | 6.738 |
| 8.38 | 38,502 | 23.45 | 0.14 | 192 | 3 | 111 | 3.733 | 3.469 |
| 12.13 | 56,449 | 33.93 | 0.29 | 192 | 3 | 125 | 3.729 | 3.423 |
| 17.55 | 82,935 | 21.17 | 0.14 | 128 | 2 | 112 | 2.140 | 1.803 |
| 25.40 | 119,452 | 30.80 | 0.29 | 128 | 2 | 181 | 2.149 | 1.790 |
| 36.76 | 177,933 | 44.48 | 0.29 | 128 | 2 | 163 | 2.144 | 1.772 |
| 53.20 | 254,027 | 21.39 | 0.29 | 64 | 1 | 137 | 1.333 | 0.952 |
| 76.99 | 371,579 | 31.08 | 0.57 | 64 | 1 | 154 | 1.338 | 0.949 |
| 111.43 | 540,912 | 45.05 | 0.57 | 64 | 1 | 194 | 1.339 | 0.945 |
| 161.27 | 811,041 | 67.30 | 1.14 | 64 | 1 | 238 | 1.352 | 0.942 |
| 233.40 | 1,220,814 | 100.38 | 1.14 | 64 | 1 | 244 | 1.360 | 0.937 |
| **252.75 (DEC-43)** | 1,320,677 | 109.50 | 1.14 | 64 | 1 | 326 | 1.364 | 0.937 |
| 337.79 | 1,808,585 | 148.28 | 2.28 | 64 | 1 | 285 | 1.365 | 0.937 |
| 488.88 | 2,675,577 | 219.00 | 2.28 | 64 | 1 | 526 | 1.377 | 0.937 |
| 707.54 | 3,900,403 | 319.38 | 4.56 | 64 | 1 | 621 | 1.377 | 0.937 |
| 1024.00 | 5,748,719 | 469.94 | 4.56 | 64 | 1 | 714 | 1.393 | 0.937 |

- **Kept.** The arena keeps the block's 222,222 slots in 62 chunks (15.50 MiB), plus its index's growth where the block crosses an index doubling (at B_DEC43, 25.50 MiB in all). The fallback keeps about a whole table: every shard doubles, once from 53.20 MiB up and up to 4 times at 4 MiB. At B_DEC43 it keeps 109.50 MiB, 0.43·B, as rev5 §2.3 estimates (≈ 109.5 MiB, against ≈ 15.3 MiB for the arena and ≈ 22 MiB of pool nodes for Core).
- **Transient.** The fallback's shards double one at a time, so its transient is the last doubled shard's old table (1.14 MiB at B_DEC43, under §2.3's 4.5625 MiB bound). The arena's is non-zero only where its index doubles late in the block (2.25 MiB at 111.43 MiB, 4.50 MiB at B_DEC43). Where the index doubles earlier, the chunks the rest of the block allocates take the bytes after the block above the doubling's peak.
- **Time.** At B_DEC43 the block takes 326 ms in the fallback and 765 ms in the arena, whose index doubles there. These are wall times under the box's load.
- **Σ once the layer freezes.** With the full presize the new active takes Σ to 1.33–1.39·B from 53.20 MiB up, so V would wait at the boundary of the freeze. With m6's presize Σ is 0.937·B from 233.40 MiB up, that is B − B/16, since the new active gets what is left. From 53.20 to 161.27 MiB what is left is under the 256-bucket floor, so the new active gets the floor and Σ is 0.942–0.952·B. Below 53.20 MiB the frozen layer alone is past B, so Σ is past B either way, and V waits at that boundary for W.

## Stalls with and without the presize-to-what-is-left rule

rev5 (O-R6-m6) presizes a new active to what is left of B − R − B/16 beside F, so that a frozen layer that has grown does not make V wait again at once, and asks a1 for the stall count both ways. The gate's measurement ends at V's first wait, so it cannot count stalls. `stalls` replays the trace at each budget through repeated freezes, with a model of W on a clock that counts events:
- **V takes one unit of time per event** (a create or a spend), and **W writes each F in ρ times the events V applied while F was active.** ρ < 1 is a W faster than V, ρ > 1 a slower one. V never waits for W except at CRITICAL.
- At a boundary: F is released if W is done. With no F in flight, THRESHOLD freezes active, and the new active is presized by the rule under test. Then, while Σ > B, V **stalls**: the clock moves on to W's release, and the freeze rule runs again. A stall right after such a release, at the same boundary, is a **double stall**, the case m6 is about.
- **Wait** is V's time stalled, as a share of its time applying events.

This W model is the harness's, not the design's. CH2c measures the real stalls (§5.5).

One pass (heights 1..453,371) freezes nothing from 233.40 MiB up, B_DEC43 included, so `stalls` runs 10 passes, the trace repeated with fresh keys as in the gate's measurement. Summed over the 17 budgets:

| ρ | freezes, m6 | stalls, m6 | double, m6 | wait, m6 | freezes, full | stalls, full | double, full | wait, full |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0.5 | 12,062 | 46 | 0 | 0.0 % | 11,998 | 73 | 11 | 0.1 % |
| 1 | 12,130 | 4,769 | 33 | 9.7 % | 11,519 | 5,386 | 1,871 | 25.1 % |
| 2 | 12,448 | 12,079 | 314 | 87.1 % | 11,323 | 11,261 | 4,024 | 122.0 % |
| 4 | 13,077 | 12,987 | 2,040 | 258.9 % | 11,327 | 11,316 | 4,130 | 297.8 % |

At B_DEC43:

| ρ | freezes, m6 | stalls, m6 | double, m6 | wait, m6 | freezes, full | stalls, full | double, full | wait, full |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 0.5 | 9 | 0 | 0 | 0.0 % | 9 | 0 | 0 | 0.0 % |
| 1 | 11 | 6 | 0 | 9.5 % | 9 | 6 | 1 | 12.8 % |
| 2 | 12 | 11 | 0 | 91.7 % | 9 | 8 | 3 | 112.5 % |
| 4 | 12 | 11 | 0 | 266.0 % | 9 | 8 | 3 | 274.1 % |

- **m6 does what it is for.** Double stalls fall from 10,036 to 2,387 over the sweep, and at B_DEC43 from 1 (ρ = 1) and 3 (ρ ≥ 2) to none. With m6 the new active is at most what is left beside F unless the 256-bucket floor binds, so Σ stays at B − B/16 after a freeze and a double stall needs the floor. All 2,387 are at 4–8.38 MiB, where a grown F leaves less than the floor.
- **V waits less.** Over the sweep m6 cuts V's wait by 20 % in all, and at ρ = 1 from 25.1 % of V's time to 9.7 %. At B_DEC43 it cuts it at every ρ that stalls, from 12.8 % to 9.5 % at ρ = 1.
- **But it freezes more, and stalls more often when W is slower than V.** The capped actives are smaller and reach THRESHOLD sooner: 0.5–15 % more freezes over the sweep, more at slower W. At ρ ≥ 2 nearly every freeze stalls either way, so m6 stalls more often (at B_DEC43, 11 stalls in 12 freezes against 8 in 9), for less time in all.
- **A slow W bounds V either way.** At ρ ≥ 2 V waits about ρ − 1 times its working time, with or without m6. That is W's speed, which CH2c measures (§5.5), not the presize.

`stalls` also counts shard doublings with no F in flight, over all its freezes (summed over the four ρ):

| B (MiB) | freezes, m6 | doublings with no F, m6 | freezes, full | doublings with no F, full |
|---|---:|---:|---:|---:|
| 4.00 | 19,090 | 3,485 | 17,848 | 3,871 |
| 5.79 | 11,830 | 1,349 | 10,895 | 1,868 |
| 8.38 | 7,337 | 406 | 6,824 | 722 |
| 12.13 | 4,505 | 174 | 4,199 | 338 |
| 17.55 | 2,799 | 83 | 2,581 | 134 |
| 25.40 | 1,693 | 25 | 1,567 | 37 |
| 36.76 | 1,029 | 8 | 953 | 10 |
| 53.20 | 614 | 4 | 573 | 8 |
| 76.99 | 347 | 1 | 316 | 4 |
| 111.43 | 208 | 0 | 193 | 0 |
| 161.27 | 103 | 0 | 76 | 0 |
| 233.40 | 55 | 0 | 48 | 0 |
| **252.75 (DEC-43)** | 44 | 0 | 36 | 0 |
| 337.79 | 35 | 0 | 30 | 0 |
| 488.88 | 16 | 0 | 16 | 0 |
| 707.54 | 8 | 0 | 8 | 0 |
| 1024.00 | 4 | 0 | 4 | 0 |

The gate's one fill per budget has none at any budget. Over many freezes, shards do double with no F in flight at 76.99 MiB and below, and the count falls with B. None did at 111.43 MiB or above, B_DEC43 included (36 to 44 freezes there). The cause is the rebuild's margin (Shard growth and rebuilds, above): a shard's room below its trigger is used up by removes between rebuilds, so a block can take a shard past its trigger and on to a doubling before the boundary freezes it. At small B a shard's margin is a few entries and a block's churn often exceeds it.

## Cross-check: one coin per block

The per-coin `sweep` puts a block boundary after every coin, with creates only, lengths drawn from the trace's coin-length histogram (`benches/data/mainnet-453371-coin-lengths.txt`), random keys and a fixed salt. It needs no trace. Its `N_res` is the coins at the last boundary within B, one coin short of the trace mode's point. At the head, under rev5's policy:
- **Floor.** It holds at all 17 budgets; the narrowest margin is 1.126 (5.79 MiB).
- **Trigger.** Every first layer froze at its shard trigger, and no shard grew with no F in flight at any budget. This stream only creates, so no remove uses up a shard's room.
- **Density.** At B_DEC43 the fallback holds 2,689,737 coins (r = 98.53). The arena held 3,148,601 at `d5c56b8d`, so r_a/r_f = 0.854.

## Latency at B_DEC43

**PMU counts per hit** at `d5c56b8d`, net of the key-generation baseline (`counts-v3`, CPU 7):

| Lookup | Event | Arena | Fallback | Arena / fallback |
|---|---|---|---|---|
| hit in F (`get`) | `instructions:u` | 145.46 | 152.86 | 0.952 |
| | `mem_load_uops_retired.l3_miss:u` | 2.36 | 1.43 | **1.654** |
| | `cycles:u` | 1,842 | 1,066 | 1.729 |
| resident key, active then F (`path`) | `instructions:u` | 194.04 | 216.20 | 0.897 |
| | `mem_load_uops_retired.l3_miss:u` | 2.77 | 1.73 | **1.601** |
| | `cycles:u` | 2,236 | 1,552 | 1.441 |

The arena executes about 5–10 % fewer instructions per hit. But it misses L3 about 0.9–1.0 more times per hit: the index probe and the slot it names are two dependent misses, where a fallback bucket holds the slot inline. The bar is each count ≤ 1.1× the fallback's, so the arena fails on L3 misses.

**The routing bits** (r6 m8). The fallback picks a shard from the hash's bits 25..=56, disjoint from hashbrown's bucket bits (the low ones) and its 7-bit tag (the top seven), so each shard's keys spread over all 128 tags. The numbers above were measured with that routing. It is now a stated requirement in `sharded.rs`, with a test (`routing_uses_bits_hashbrown_does_not`) and a debug assertion.

**Sign test** (`timing-v3`, 20 M hits in F per rotation, CPU 9, under load):

| Rotation | Arena ns/hit | Fallback ns/hit | Arena / fallback |
|---|---|---|---|
| 0 | 628.69 | 409.07 | 1.537 |
| 1 | 664.47 | 424.90 | 1.564 |
| 2 | 645.77 | 408.72 | 1.580 |
| 3 | 638.64 | 418.30 | 1.527 |
| 4 | 652.19 | 409.91 | 1.591 |

The arena was faster in 0 of 5 rotations; it is about 1.56× slower per hit. Wall-clock latency includes the box's load, so the ratio is the evidence, not the absolute ns.

**Recorded, not gated** (CH2 §2.3): `iter_dirty` over F at B_DEC43, copying key and coin bytes as a flush batch would:

| Layout | Entries | M entries/s | MiB/s |
|---|---|---|---|
| arena (`d5c56b8d`) | 1,692,601 | 18.7 | 1,129 |
| fallback (`d5c56b8d`, `fb-grow`) | 1,345,990 | 16.0 | 965 |
| fallback (head, rev5) | 1,320,678 | 17.7 | 1,067 |

The fallback rows differ by the box's load, not by the code.

## Decision record

| Criterion | Bar for keeping the arena | Measured | Result |
|---|---|---|---|
| Density at B_DEC43 (trace) | r_a/r_f ≤ 0.8 | 0.9067 | fails |
| Density, geometric mean over 17 budgets (trace) | ≤ 0.8 | 0.8800 | fails |
| Latency, PMU per hit | each ≤ 1.1× | instructions 0.952×, L3 misses 1.654× | fails |
| Sign test | confirms PMU | arena faster in 0 of 5 rotations, ≈ 1.56× slower per hit | confirms |
| The winner's floor (trace) | N_res ≥ N_core at every point | min 1.043× (4 MiB) | passes |
| No shard grows with no F in flight (trace) | none at B_DEC43 | none at any budget in the gate's fill; none at B_DEC43 over 10 passes of freezes (`stalls`) | passes |

The fallback ships. The arena (`dashd-chainstate/src/coins/layer/arena.rs`) and its tests are deleted in `cc27dca0`.

## What changed from the gate's plan

- **DEC-95 replaced rule O's accounting.** The harness checks Σ at block boundaries and lets the layers grow inside a block. An earlier run under rule O's accounting reached the same decision and is superseded.
- **The trace method is the gate's measurement** (rev3). It replaced the per-coin harness, which stays as the cross-check above.
- **rev3's readings are superseded by rev4's trigger.** Under rev3 the presized layer reached THRESHOLD (allocated bytes ≥ f·B) only by growing, so a1 measured three readings at `a5a168b3`: rev3 as written (`thr`), which failed the floor at the 12 budgets below B_DEC43; freezing after the first growth (`grow`, 0.8751 / 0.8605); and also waiting there (`wait`, 0.8699 / 0.8512). All three said "fallback". rev4's trigger replaces them; `a5a168b3` still runs them.
- **The fallback grows past its presize** (DEC-95, rev3 G-R3-M3). A shard with no room left doubles on its own, and routing still follows the presized bucket counts. `a_presized_layer_fills_its_presize_then_grows_a_shard_at_a_time` asserts, with the tagging allocator, that nothing reallocates until a shard holds 7/8 of its presized buckets, that the trigger fires before that, and that each step past it doubles exactly one shard.
- **Churn rebuilds a shard instead of doubling it.** `a5a168b3` rebuilt at up to 13/16 load; the head rebuilds up to the trigger, as rev4 has it. `a_shard_rebuilds_below_its_trigger_and_doubles_at_it` holds one 128-bucket shard at 109 entries (its trigger is 110) through 1,000 inserts and removes, then churns it at 110 until it doubles, with no rebuild first. `churn_below_the_trigger_never_grows_the_layer` runs 10× the buckets of churn at 40 % and 60 % load.
- **Long coins are boxed**, one exact-size box each, as rev3 has the fallback. They are no longer in 16 KiB chunks, which left most of a chunk unused for a coin over 8 KiB.
- **The no-growth row is gated at B_DEC43 only** (r6 m4). In the gate's one fill it held at every budget. Over repeated freezes it fails at 76.99 MiB and below and holds at B_DEC43.
- **The second layer is presized to what is left** (rev5, O-R6-m6): `min(f·(B − R − B/16), B − R − B/16 − F.allocated)`, at least 256 buckets a shard. It moved the 4 MiB floor from 1.083 to 1.043 and the geometric mean from 0.8852 to 0.8800, and changed no row from 53.20 MiB up.

## a2: the boundary rebuild (DEC-108)

DEC-108 took up the first open question below as a §2.3 amendment. At each block boundary with no F in flight, `ShardedLayer::boundary_rebuild` rebuilds every shard whose entries plus DELETED buckets have reached its trigger while its entries have not. The harness runs it at every such boundary. The trace sweep now also times it, per boundary that rebuilt a shard. The stalls figures are from the harness at `194f441b`. The trace figures are from `0385c05e`, run at a load average of 30–55, against 55–110 for a1's.

**Doublings with no F in flight**, over 10 passes of freezes (`stalls`, summed over ρ ∈ {0.5, 1, 2, 4}), without the boundary rebuild (a1, the table above) and with it (a2):

| B (MiB) | freezes, m6 (a2) | doublings, m6: a1 → a2 | doublings, full presize: a1 → a2 |
|---|---:|---:|---:|
| 4.00 | 19,083 | 3,485 → 2,941 | 3,871 → 2,792 |
| 5.79 | 11,898 | 1,349 → 1,092 | 1,868 → 1,291 |
| 8.38 | 7,333 | 406 → 342 | 722 → 494 |
| 12.13 | 4,500 | 174 → 66 | 338 → 216 |
| 17.55 | 2,804 | 83 → 38 | 134 → 93 |
| 25.40 | 1,693 | 25 → 2 | 37 → 0 |
| 36.76 | 1,028 | 8 → 0 | 10 → 2 |
| 53.20 | 614 | 4 → 0 | 8 → 0 |
| 76.99 | 347 | 1 → 0 | 4 → 0 |
| 111.43 and up | 473 in all | 0 → 0 | 0 → 0 |

- **From 36.76 MiB up no shard doubles with no F in flight** under m6, 76.99 MiB (DEC-108's example) included. Below that, the rest are blocks that put more than 1/64 of a shard's buckets into it. A 4 MiB layer's shards have 256 buckets, so their margin is 4 entries, and growth is expected there (r6 m4). Freezes and stalls change by under 1 % (m6: 49,717 → 49,773 freezes, 29,881 → 29,874 stalls). Double stalls rise from 2,387 to 2,421 under m6, and from 10,036 to 10,745 with the full presize.
- **`dec108_no_shard_grows_without_f_in_flight`** (`coins/tests/cache.rs`) is DEC-108's churn test at 76.99 MiB. Blocks create coins, spend most of them soon (removed entries) and some old ones, from an active at 72 % of its buckets. With the rebuild, every shard below its trigger starts each block with no F in flight with room for over 1/64 of its buckets, and none grows (57 blocks, 71 rebuilds, 1 freeze). The control without it starts 202 shard-blocks short of that room.
- **The gate's one fill** still has no doubling with no F in flight at any budget (`grew_no_F` 0, all 17), and the floor holds at every point. Neither the floor nor `N_res` changed at any budget.

**Rebuilds in the gate's fill**, per budget: in-op rebuilds (an insert into a shard whose room is gone), and the boundary rebuild's shards, boundaries and time per boundary that rebuilt:

| B (MiB) | in-op rebuilds | boundary: shards | boundaries | of blocks | p50 ms | p99 ms | max ms |
|---|---:|---:|---:|---:|---:|---:|---:|
| 4.00 | 79 | 1 | 1 | 15,720 | 0.029 | 0.029 | 0.029 |
| 5.79 | 34 | 7 | 7 | 19,670 | 0.012 | 0.023 | 0.023 |
| 8.38 | 22 | 49 | 47 | 30,445 | 0.013 | 0.093 | 0.093 |
| 12.13 | 6 | 9 | 7 | 37,649 | 0.023 | 0.067 | 0.067 |
| 17.55 | 21 | 4 | 4 | 45,121 | 0.018 | 0.075 | 0.075 |
| 25.40 | 31 | 6 | 6 | 55,846 | 0.032 | 0.162 | 0.162 |
| 36.76 | 21 | 16 | 13 | 64,172 | 0.065 | 0.382 | 0.382 |
| 53.20 | 37 | 6 | 6 | 79,751 | 0.084 | 0.309 | 0.309 |
| 76.99 | 84 | 87 | 83 | 155,550 | 0.176 | 0.645 | 0.645 |
| 111.43 | 147 | 236 | 213 | 247,056 | 0.193 | 0.751 | 1.254 |
| 161.27 | 124 | 221 | 204 | 493,987 | 0.345 | 1.000 | 1.264 |
| 233.40 | 226 | 415 | 394 | 803,546 | 0.486 | 1.639 | 12.717 |
| **252.75 (DEC-43)** | 247 | 291 | 280 | 991,561 | 0.811 | 2.179 | 2.962 |
| 337.79 | 352 | 312 | 292 | 1,246,457 | 0.915 | 3.029 | 4.674 |
| 488.88 | 175 | 379 | 369 | 2,085,322 | 1.669 | 3.493 | 4.975 |
| 707.54 | 375 | 377 | 360 | 3,229,440 | 1.871 | 5.514 | 11.020 |
| 1024.00 | 185 | 401 | 392 | 4,823,444 | 3.414 | 5.484 | 8.976 |

At B_DEC43 the boundary rebuild moves 291 of the 538 rebuilds to boundaries, and in-op rebuilds fall from 405 (a1) to 247. Entries moved rise from 7,992,926 to 10,798,709. One boundary in 3,541 runs a boundary rebuild, at 0.81 ms median, 2.18 ms p99 and 2.96 ms at most. Blocks with an in-op rebuild take 0.84 / 2.10 / 3.29 ms (a1: 0.97 / 3.05 / 3.42 at the higher load). Against connect-latency metric #5, 519 of 991,561 blocks (1 in 1,910) now pay a rebuild of about 2 ms at p99. That can raise a p95 at most to the old p95.05.

## a2: the completion witnesses beside Core

The witnesses of §7.1 a2 (`coins/tests/witness.rs`) run on the kept layout with the tagging allocator equal to native Σ − R after every op. The table records native Σ beside Core's `DynamicMemoryUsage` for the same block, a reported number only (no assertion compares them). Core's figures come from a probe built against the v24 pin's headers (`contrib/node/coins-meter/probe.cpp`: the real `CCoinsMap` node, `PoolAllocator` and `memusage`). It reproduces Sol's 20,000-output value, 2,263,712 B, exactly.

Σ at the boundary is measured where Core reads its cache's size, at `ConnectTip`'s `FlushStateToDisk` (`validation.cpp:3457`). By then the block's undo has been written and dropped with `ConnectBlock`'s `CBlockUndo` (`:2641`, `:2888`), so the witnesses drop theirs first too (DEC-142 O3; a2 measured with the undo still held).

| Witness | B | R | Σ at the boundary | peak Σ, op boundaries | allocator peak + R | `boundary_state` | Core's tip meter |
|---|---:|---:|---:|---:|---:|---|---:|
| (i) 50,000 P2PKH outputs, `-dbcache=4` | 1,835,008 | 0 | 5,609,472 | 5,609,472 | 5,730,992 | CRITICAL | 5,926,016 |
| (ii) long-input connect, `-dbcache=4` | 1,835,008 | 0 | 6,920,192 | 944,829,448 | 944,829,448 | CRITICAL | 473,446,016 at its peak; 5,926,016 after the block |
| (ii) its disconnect | 1,835,008 | 0 | 473,350,148 | — | — | CRITICAL | — |
| (iii) 50,000 outputs, B_DEC43, F in flight | 265,027,584 | 33,193,984 | 246,093,824 | 250,881,024 | 250,918,416 | OK | — |
| (iii) long-input connect, B_DEC43, F in flight | 265,027,584 | 33,193,984 | 247,404,544 | 1,185,313,800 | 1,185,313,800 | OK | — |
| (iii) its disconnect | 265,027,584 | 33,193,984 | 714,096,644 | — | — | CRITICAL | — |
| (iv) a shard doubles mid-block, B_DEC43 | 265,027,584 | 0 | 108,841,984 | 108,882,432 | 111,274,512 | OK | — |

- **(i)** Native Σ is 0.947× Core's meter at the boundary and 0.967× inside the block. Both are CRITICAL; the active layer doubled 192 times inside the block (DEC-95).
- **(ii)** 48,700 inputs spend 9,577-byte consensus-valid scripts (9,583-byte coins). Core's meter covers only the tip: 473,446,016 B once the inputs are fetched, and 5,926,016 B after the block, when the spent coins sit in `CBlockUndo` outside it. Native Σ counts the undo (`held_out`) while the block holds it: Core's tip plus the undo, to 1.0004×. At the boundary, with the undo dropped, native Σ is 6,920,192 B, 1.168× Core's tip; both hold the 48,700 spent entries, ours in shards that doubled 192 times inside the block. The peak is two copies of every input: active's, fetched by `have_inputs`, and the child's spend copy (rev5: the child owns a copy of what it spends, so a failed connect leaves the parent whole). Core holds two copies too. `ConnectBlock`'s child view copies each coin it fetches from the tip (`coins.cpp:44-68`), and the tip's copy lives until `view.Flush()` (`validation.cpp:3446`). By the same `memusage` arithmetic Core holds, at the end of `ConnectBlock`, 473,446,016 B (tip) + 5,926,016 B (the child's spent entries) + 467,520,000 B (the undo's scripts, 48,700 × `MallocUsage(9,577)`) = 946,892,032 B. Native Σ's peak is 0.998× of that. The disconnect restores every coin byte for byte. Its boundary, before the flush it forces, is CRITICAL at 473,350,148 B: active holds the 48,700 restored coins, as Core's tip would (DEC-142 review: a2 and the first fix series reported it after that flush, 703,872 B).
- **(iii)** With F in flight at B_DEC43, a 50,000-output block grows no shard and stays OK. So does the long-input block: with its undo dropped, Σ at its boundary is 247,404,544 B, under Core's LARGE threshold `max(0.9·B, B − 10 MiB)` = 254,541,824 B. Its undo (466,692,100 B of spent coins) takes Σ past B only inside the block. The disconnect restores those coins into active, so the boundary after it is CRITICAL at 714,096,644 B, and V waits there for W.
- **(iv)** Shard 0 doubles from 32,768 to 65,536 buckets inside a block: its table goes from 2,392,080 to 4,784,144 B, and the op's transient is the new table, 4,784,144 B, counted to the byte.

## Open questions

rev3's questions are answered: the freeze rule by rev4's trigger, the "no shard grows" row by rev4 and r6 m4, and R inside Σ by DEC-99.

**The trigger's margin after churn.** *Answered by DEC-108 (the boundary rebuild above).* Removes use up the room a rebuild leaves, so a shard below its trigger can have as little as one insert of room when a block takes it to its trigger, and then double inside that block with no F in flight. B_DEC43 shows none over 10 passes; 76.99 MiB and below do. The code review proposes a fix: at each boundary with no F in flight, also rebuild any shard whose used room (entries plus DELETED buckets) has reached its trigger while its entries have not. A block would then need over 1/64 of a shard's buckets in inserts to double it, as rev5 §2.3 assumes. That is a policy change to §2.3, so a1 asks rather than adds it.

*Answered by DEC-108: the event-clock model is fine for a1's stall report.* The stall count with and without m6's rule needs a model of W's speed, which the design does not give. The harness's model (on a clock of events, W writes F in ρ times the events V applied to it, for ρ ∈ {0.5, 1, 2, 4}) is a1's choice; the manager may want another.
