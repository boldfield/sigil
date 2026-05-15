//! Folded-stacks writer — plan 2026-05-08-sigil-v2-runtime-profile-data
//! Phase 5, Task 7.
//!
//! Folded-stacks is the format Brendan Gregg's
//! [FlameGraph.pl](https://github.com/brendangregg/FlameGraph)
//! consumes: one line per unique stack with a count, frames
//! separated by `;` and root-to-leaf orientation.
//!
//! ```text
//! main;run_loop;sigil_alloc 42
//! main;run_loop;perform_dispatch 7
//! ```
//!
//! Aggregation is by name list (post-symbol-resolution). Sample
//! `value` is summed — for CPU samples that's a tick count; for
//! allocation samples it's bytes allocated.
//!
//! Symbol resolution at write time is done by the [`super::resolve`]
//! module, which reads `prog.symtab` once at flush time and caches
//! the mapping.

use std::collections::BTreeMap;
use std::io::{self, Write};

use crate::profile::resolve;
use crate::profile::sample::Sample;

/// Write `samples` as a folded-stacks file. Returns the number of
/// unique-stack rows emitted.
pub fn write_folded(samples: &[Sample], out: &mut impl Write) -> io::Result<usize> {
    let resolver = resolve::Resolver::from_env_for_main_binary().with_dyld_images();
    // BTreeMap keys are deterministic (lex-sorted) — output bytes
    // are reproducible across runs.
    let mut buckets: BTreeMap<String, u64> = BTreeMap::new();

    for s in samples {
        let key = fold_one(&resolver, s);
        let entry = buckets.entry(key).or_insert(0);
        *entry = entry.saturating_add(s.value);
    }

    for (stack, count) in &buckets {
        writeln!(out, "{stack} {count}")?;
    }
    Ok(buckets.len())
}

/// Produce one folded line's stack-key for `sample`. Root-leftmost.
fn fold_one(resolver: &resolve::Resolver, sample: &Sample) -> String {
    let live = sample.live_frames();
    // Sample frames are leaf-first; FlameGraph convention is root-
    // leftmost. Reverse before joining.
    let mut names: Vec<String> = live.iter().rev().map(|pc| resolver.lookup(*pc)).collect();
    if names.is_empty() {
        names.push("[empty]".to_string());
    }
    names.join(";")
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::profile::sample::SampleKind;
    use crate::profile::unwind::MAX_DEPTH;

    fn sample_with(value: u64, frames: &[usize]) -> Sample {
        let mut s = Sample {
            ts_ns: 0,
            value,
            depth: frames.len() as u32,
            kind: SampleKind::Cpu,
            frames: [0; MAX_DEPTH],
        };
        for (i, f) in frames.iter().enumerate() {
            s.frames[i] = *f;
        }
        s
    }

    #[test]
    fn folded_aggregates_identical_stacks() {
        // Two samples with identical stacks aggregate. The
        // resolver returns `0x<hex>` for any PC it can't map (no
        // symtab loaded), so both lines key on the same string.
        let samples = vec![
            sample_with(1, &[0xAAA, 0xBBB, 0xCCC]),
            sample_with(1, &[0xAAA, 0xBBB, 0xCCC]),
            sample_with(1, &[0xAAA, 0xBBB, 0xDDD]),
        ];
        let mut buf: Vec<u8> = Vec::new();
        let n = write_folded(&samples, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(n, 2, "expected two unique stacks");
        // Aggregated CCC row's value should be 2; DDD row's 1.
        assert!(
            text.lines().any(|l| l.ends_with(" 2") && l.contains("ccc")),
            "expected aggregated CCC row with count 2: text={text:?}"
        );
        assert!(
            text.lines().any(|l| l.ends_with(" 1") && l.contains("ddd")),
            "expected DDD row with count 1: text={text:?}"
        );
    }

    #[test]
    fn folded_reverses_to_root_leftmost() {
        // Leaf is at frames[0]; FlameGraph wants root-leftmost. So
        // the output's leftmost name is the LAST captured frame.
        let samples = vec![sample_with(1, &[0xAAA, 0xBBB, 0xCCC])];
        let mut buf: Vec<u8> = Vec::new();
        write_folded(&samples, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let line = text.lines().next().unwrap();
        // Names are addresses (no symtab); the first column
        // (leftmost) should be CCC, then BBB, then AAA, then count.
        let parts: Vec<&str> = line.split(' ').collect();
        let stack = parts[0];
        let names: Vec<&str> = stack.split(';').collect();
        assert!(
            names[0].contains("ccc"),
            "root (leftmost) should be deepest pre-reverse frame: stack={stack}"
        );
        assert!(names[names.len() - 1].contains("aaa"));
    }

    #[test]
    fn folded_handles_empty_sample_gracefully() {
        // depth=0 sample.
        let mut s = Sample {
            ts_ns: 0,
            value: 5,
            depth: 0,
            kind: SampleKind::Cpu,
            frames: [0; MAX_DEPTH],
        };
        s.frames[0] = 0xDEAD; // ignored — depth=0
        let mut buf: Vec<u8> = Vec::new();
        let n = write_folded(&[s], &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(n, 1);
        assert!(
            text.starts_with("[empty] 5"),
            "depth-0 sample should emit `[empty] <count>`: text={text:?}"
        );
    }

    #[test]
    fn folded_sums_values_within_a_bucket() {
        let samples = vec![
            sample_with(10, &[0x1]),
            sample_with(20, &[0x1]),
            sample_with(30, &[0x1]),
        ];
        let mut buf: Vec<u8> = Vec::new();
        write_folded(&samples, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.lines().any(|l| l.ends_with(" 60")),
            "expected summed value 60: text={text:?}"
        );
    }
}
