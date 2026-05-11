//! Output dispatcher — plan 2026-05-08-sigil-v2-runtime-profile-data
//! Phase 5, Task 9.
//!
//! Auto-detects the output format from the path's file extension and
//! routes to the appropriate writer. `SIGIL_PROFILE_FORMAT` overrides
//! the auto-detection.
//!
//! | Path ends in | Format     |
//! |--------------|------------|
//! | `.txt`       | folded     |
//! | anything else| pprof (proto) |
//!
//! `SIGIL_PROFILE_FORMAT=pprof` or `folded` (case-insensitive)
//! overrides the extension-based pick.

use crate::profile::cpu;
use crate::profile::sample::SampleKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Pprof,
    Folded,
}

/// Decide the output format for a given path. Honours
/// `SIGIL_PROFILE_FORMAT` env-var override.
pub fn pick_format(path: &str) -> OutputFormat {
    if let Ok(override_) = std::env::var("SIGIL_PROFILE_FORMAT") {
        let norm = override_.trim().to_ascii_lowercase();
        match norm.as_str() {
            "pprof" => return OutputFormat::Pprof,
            "folded" => return OutputFormat::Folded,
            other if !other.is_empty() => {
                eprintln!(
                    "sigil profile: SIGIL_PROFILE_FORMAT=`{override_}` not recognised; \
                     falling back to extension detection"
                );
            }
            _ => {}
        }
    }
    if path.to_ascii_lowercase().ends_with(".txt") {
        OutputFormat::Folded
    } else {
        OutputFormat::Pprof
    }
}

/// Phase 5 entry point invoked by [`cpu::cpu_atexit_cb`]. Drains
/// CPU samples and writes to `path` in the chosen format. Errors
/// (file open, I/O) go to stderr — atexit can't propagate.
pub fn write_cpu_profile(path: &str) {
    let samples = cpu::take_samples();
    if samples.is_empty() {
        eprintln!("sigil profile: no CPU samples captured; output `{path}` not written");
        return;
    }
    let fmt = pick_format(path);
    write_samples(path, &samples, fmt, SampleKind::Cpu);
}

/// Phase 5 entry point for the allocation profiler. Same shape as
/// [`write_cpu_profile`] but reads the alloc-sample buffer.
pub fn write_alloc_profile(path: &str) {
    let samples = crate::profile::alloc::take_samples();
    if samples.is_empty() {
        eprintln!("sigil profile: no allocation samples captured; output `{path}` not written");
        return;
    }
    let fmt = pick_format(path);
    write_samples(path, &samples, fmt, SampleKind::Alloc);
}

fn write_samples(
    path: &str,
    samples: &[crate::profile::sample::Sample],
    fmt: OutputFormat,
    kind: SampleKind,
) {
    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("sigil profile: cannot create `{path}`: {e}");
            return;
        }
    };
    let mut writer = std::io::BufWriter::new(file);
    let res: std::io::Result<()> = match fmt {
        OutputFormat::Folded => {
            crate::profile::folded::write_folded(samples, &mut writer).map(|_| ())
        }
        OutputFormat::Pprof => crate::profile::pprof::write_pprof(samples, kind, &mut writer),
    };
    if let Err(e) = res {
        eprintln!("sigil profile: write to `{path}` failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pprof_for_pb_extension() {
        std::env::remove_var("SIGIL_PROFILE_FORMAT");
        assert_eq!(pick_format("out.pb"), OutputFormat::Pprof);
        assert_eq!(pick_format("/tmp/profile.proto"), OutputFormat::Pprof);
        assert_eq!(pick_format("noext"), OutputFormat::Pprof);
    }

    #[test]
    fn folded_for_txt_extension() {
        std::env::remove_var("SIGIL_PROFILE_FORMAT");
        assert_eq!(pick_format("out.txt"), OutputFormat::Folded);
        assert_eq!(pick_format("/tmp/PROFILE.TXT"), OutputFormat::Folded);
    }

    // Env-var override tests are skipped from the suite to keep
    // SIGIL_PROFILE_FORMAT in a known state — the process-wide
    // env-var is shared with other tests and toggling it from one
    // test can race the assertions in another. The format-picker
    // logic is exercised end-to-end by the smoke test in Task 10.
}
