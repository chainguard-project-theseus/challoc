# Lane tidy-tests: maintainability pass

## Done

- Maintainability pass (lane tidy-tests, 2026-09-20): `src/api/ctests.rs` is one `CTest` table and one driver (`run_row`: gate, cached compile, bare run, preloaded run, expectation, skip lines; `ctests!` keeps one `#[test]` per entry, `run_c_negative`, `run_conf_flags` and the readelf tests stay functions; programs compile once per distinct flags through the new `scripts/ctest-cc.sh`, the one compiler line with QUANTUM read from the built library, used by ctests.rs, `scripts/cgroup-tests.sh`, `scripts/pagemap-guard-test.sh` and `tests/as-needed.sh`); skips are one `SKIP <test> <reason>` line (Rust `skip!`, C `skip_note`) that fails unless `CHALLOC_TEST_ALLOW_SKIP` is set; `tests/c/common.h` gained `sleep_ms`, `now_ms`, `rss_bytes`, `fork_and_wait`, `expect_signal`, `mallctl_dl`, the weak API block and the `MALLOCX_*` flags, and the programs that re-spelled them (the cgroup programs, decay, prefault, jemalloc_api, negative, libc_api, tls_threads) use the shared ones; item 43 of the 2026-09-18 review is covered (negative modes `guard_free`, `realloc_stale`, `sallocx_stale`, `dallocx_none_twice`, `free_sized_huge_hint`, `huge_double_free`, `huge_interior_free`; `tests/c/positive.c` with `free_sized`, `xallocx_extra`, `malloc_trim_idle`); `fork_cost.c` is `bench/probes/fork_cost.c` and its two rows are gone; `src/model.rs` is `src/model/{mod,state,steps,percpu,scenarios}.rs`; `docs/testing.md` is the test pyramid. Verified in the container: 411 unit + 128 `api::` (was 120: +10 rows, -2 fork_cost) + 23 cgroup-limit, churn/lazy/pagemap-guard/as-needed green, host `cargo test --features std` 319, codegen compare IDENTICAL for q16 and q8. Commits 4c3699b, b1e9da4, d6e6d2c, 9f714b8, e911c9f, 7211f76.

## Notes for the human

- Makefile diff to apply (this lane does not edit the Makefile): set the skip variable on the local test targets and route the fuzz build through the shared compiler line; a `test-all` target for what `make test` leaves out.

      -test: image
      +test: DOCKER_FLAGS = -e CHALLOC_TEST_ALLOW_SKIP=1
      +test: image
       	$(DOCKER_RUN) sh -c 'cargo build --release \
      ...
      -test-cgroup-limit: DOCKER_FLAGS = --memory=512m -e CHALLOC_EXPECT_CGROUP_LIMIT=536870912
      +test-cgroup-limit: DOCKER_FLAGS = --memory=512m -e CHALLOC_EXPECT_CGROUP_LIMIT=536870912 -e CHALLOC_TEST_ALLOW_SKIP=1
      ...
      -	  && cc -O2 -g -Wall -Wextra -Werror -pthread -DQUANTUM=16 -Itests/c \
      -	       fuzz/fuzz.c -o target/fuzz -ldl \
      +	  && sh scripts/ctest-cc.sh fuzz/fuzz.c -o target/fuzz \
      ...
      +# Everything `make test` does not run: the 10M-operation fuzz, the
      +# 5-minute oscillation, the --as-needed truth (docs/testing.md).
      +test-all: test fuzz test-cgroup-oscillate check-needed

  and add `test-all` to `.PHONY`. The reference VMs should run without `CHALLOC_TEST_ALLOW_SKIP` (`challoc-bench`'s test step), so a feature that stops being checked there fails; on the Mac's Docker VM the run prints 12 SKIP lines (cgroup/bg PSI, cgroup_policy bands and high, guard_slab fault, the two guard negatives, mseal, percpu, slice_ext on, thp auto and process) and passes with the variable.
- Other C compiler lines outside this lane's files, for the tooling lane: `bench/percpu-campaign.sh:139` (`cc ... tests/c/features.c` becomes `sh scripts/ctest-cc.sh tests/c/features.c -o target/lane-features`); `src/tls.rs:292` (`c_program_with_64_threads_sees_distinct_slots` compiles `tests/c/tls_threads.c` with a bare `cc` and no QUANTUM, so tls_threads.c defaults QUANTUM to 0 under `#ifndef` and probes `challoc_version` instead of `require_challoc`; fold it into ctests.rs as a `c_tls_threads` row with no conf and delete the tls.rs driver, then drop the `#ifndef`), `src/api/libc.rs:920` (libc_api.c: two rows `c_libc_api` and `c_libc_api_background` with `conf: Some("abort_conf:true,background_thread:true")`; the "ordinary programs under the preload" loop stays where it is), `src/sys/mod.rs:1033` (sysnr.c is lane tidy-sys's).
- Skipped: merging `random_ops.c` into `fuzz.c`. The two drivers differ in the operation mix (no `malloc(0)`, no aligned flavours, no inspections), the size tiers, the table (1024 vs 2048) and the RNG streams, so a `fuzz --random-ops` mode would either change what the two `c_random_ops_*` rows exercise or bolt a second weight table onto fuzz.c; left as is.
- The `fork_cost` rows are gone rather than kept: the program's only check (the burst faulted at least a quarter of its pages) tests the kernel's copy-on-write, not the allocator; fork correctness is `fork_alloc.c` and `fork_signal.c`. The `CHALLOC_THP_TESTS` opt-in went with it; the probe takes `MALLOC_CONF=thp:auto` directly (header).
- Program-level `SKIP` lines are strict too: a program's `SKIP` on stdout fails the test without `CHALLOC_TEST_ALLOW_SKIP`, the same as a closed Rust gate. This is stricter than the review asked for (it asked only for the line format) and is what makes `grep '^SKIP '` a complete list; loosen by deleting the assert in `report_program_skips` if the VMs turn out to skip something legitimately.
- Two findings while wiring item 43: `malloc_trim(0)` returns 1 after any `fopen`/`fclose` because stdio's FILE and buffer slabs go Empty (not a bug; positive.c trims once after `rss_bytes()` before asserting 0), and `api::tests::lock_inventory` reads every `Lock<` under src/ as a fork-handler lock, so ctests.rs imports `OnceLock as Once`.
- Model/class coupling proposal (not done: class.rs is another lane's): (1) a `#[cfg(test)] pub(crate) mod protocol` in `class.rs` with an enum of the atomic steps of `claim`, `release_owned`/`dec_live` and `purge_empty`, one variant per step in order, each variant's doc quoting the line it is; `model::steps` maps its sub-steps onto that enum (a `match` with no wildcard) so a step added to class.rs without a model step fails to compile; (2) the model's copied constants (`REFILL` = cap/2, `FLUSH_HALF`) read from `class`/`config` consts instead of literals; (3) a class.rs test that parses the `class.rs:NNN` quotes in the model's step descriptions and checks each named function still starts within a few lines of the quoted number, so the quotes cannot rot silently.
