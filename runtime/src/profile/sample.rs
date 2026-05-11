//! Sample types — plan 2026-05-08-sigil-v2-runtime-profile-data
//! Phase 3 / Phase 4. The same `Sample` shape is used by CPU and
//! allocation profilers; the discriminant lives in [`SampleKind`] and
//! the writers (Phase 5) emit different `sample_type` headers based
//! on it.

use crate::profile::unwind::MAX_DEPTH;

/// A captured profile sample: timestamp, kind-specific weight, and
/// the stack trace.
///
/// **Stable on-wire layout.** This struct is read by the Phase 5
/// writers (`pprof`, `folded`) and pushed by the SIGPROF handler.
/// Changing the layout requires updating both producer and consumer.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Sample {
    /// Nanoseconds since process start, captured via
    /// [`std::time::Instant`]. Roughly monotonic across samples.
    pub ts_ns: u64,
    /// Per-sample weight in `value` units:
    /// - CPU samples: 1 (one observation).
    /// - Alloc samples: bytes allocated at the sample point.
    pub value: u64,
    /// Number of valid frames in [`Sample::frames`]. Always
    /// `<= MAX_DEPTH`.
    pub depth: u32,
    /// Kind tag — the writer keys `sample_type` on this.
    pub kind: SampleKind,
    /// Stack trace, leaf-first. Entries beyond [`Sample::depth`] are
    /// undefined.
    pub frames: [usize; MAX_DEPTH],
}

/// What kind of sample this is — selects the `sample_type` headers
/// the Phase 5 writers emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SampleKind {
    /// SIGPROF-driven CPU sample. `value` is always 1 (one tick).
    Cpu = 0,
    /// `sigil_alloc`-driven allocation sample. `value` is the
    /// allocation size in bytes, weighted by sample rate.
    Alloc = 1,
}

impl Sample {
    /// An all-zero sample. Used as the default value for ring-buffer
    /// slots so the producer's first write goes to known memory.
    pub const fn zero() -> Self {
        Self {
            ts_ns: 0,
            value: 0,
            depth: 0,
            kind: SampleKind::Cpu,
            frames: [0; MAX_DEPTH],
        }
    }

    /// Borrow the live portion of the frame buffer.
    #[inline]
    pub fn live_frames(&self) -> &[usize] {
        let n = (self.depth as usize).min(MAX_DEPTH);
        &self.frames[..n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_zero_has_zero_depth_and_cpu_kind() {
        let s = Sample::zero();
        assert_eq!(s.depth, 0);
        assert_eq!(s.kind, SampleKind::Cpu);
        assert!(s.live_frames().is_empty());
    }

    #[test]
    fn live_frames_truncates_to_max_depth() {
        let mut s = Sample::zero();
        s.depth = (MAX_DEPTH as u32) + 99;
        // Sanity check: even with a poisoned depth value, live_frames
        // saturates at MAX_DEPTH rather than walking past the array.
        assert_eq!(s.live_frames().len(), MAX_DEPTH);
    }

    #[test]
    fn live_frames_clamps_to_depth_below_max() {
        let mut s = Sample::zero();
        s.depth = 3;
        s.frames[0] = 0xAA;
        s.frames[1] = 0xBB;
        s.frames[2] = 0xCC;
        s.frames[3] = 0xDD;
        let live = s.live_frames();
        assert_eq!(live, &[0xAA, 0xBB, 0xCC]);
    }
}
