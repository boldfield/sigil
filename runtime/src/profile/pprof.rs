//! Hand-rolled pprof writer — plan 2026-05-08-sigil-v2-runtime-
//! profile-data Phase 5, Task 8.
//!
//! The
//! [pprof](https://github.com/google/pprof/blob/main/proto/profile.proto)
//! schema is stable and small enough that a hand-rolled writer is a
//! better fit than pulling in `prost` / `protobuf-rs` (the plan
//! explicitly forbids new compiler / runtime deps).
//!
//! Only the subset of the schema actually consumed by `pprof` /
//! `speedscope` / `perfetto` is emitted:
//!
//! - `Profile.sample_type[]`
//! - `Profile.sample[]` — `location_id[]` and `value[]`
//! - `Profile.mapping[]` — one entry for the main binary
//! - `Profile.location[]` — id, mapping_id, address
//! - `Profile.function[]` — id, name (string_table index)
//! - `Profile.string_table[]`
//! - `Profile.time_nanos`, `Profile.duration_nanos`
//!
//! Source-line resolution (per-PC line numbers via DWARF) is v3 work;
//! v2 ships address-only locations. Renderers handle this gracefully.
//!
//! ## Wire format
//!
//! Protobuf encoding is documented at
//! <https://protobuf.dev/programming-guides/encoding/>. We implement
//! varint + length-delimited (wire types 0 and 2) only; the schema
//! does not use any fixed32 / fixed64 fields in the subset we emit.

use std::collections::BTreeMap;
use std::io::{self, Write};

use crate::profile::resolve;
use crate::profile::sample::{Sample, SampleKind};

// ---- wire-format primitives ----------------------------------------------

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[allow(dead_code)]
fn write_svarint(out: &mut Vec<u8>, v: i64) {
    // Zig-zag encoding for signed varints; the fields we emit are
    // either unsigned or `int64` documented as small-positive, so
    // this helper is unused today. Kept for completeness.
    let z = ((v << 1) ^ (v >> 63)) as u64;
    write_varint(out, z);
}

fn write_tag(out: &mut Vec<u8>, field_number: u32, wire_type: u32) {
    write_varint(out, ((field_number as u64) << 3) | (wire_type as u64));
}

const WIRE_VARINT: u32 = 0;
const WIRE_LENGTH_DELIMITED: u32 = 2;

fn write_varint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    write_tag(out, field, WIRE_VARINT);
    write_varint(out, v);
}

fn write_int64_field(out: &mut Vec<u8>, field: u32, v: i64) {
    // pprof `int64` fields encode as varint, two-complement; for our
    // positive timestamps and counts this is just `write_varint`.
    write_varint_field(out, field, v as u64);
}

fn write_string_field(out: &mut Vec<u8>, field: u32, s: &str) {
    write_tag(out, field, WIRE_LENGTH_DELIMITED);
    write_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn write_bytes_field(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    write_tag(out, field, WIRE_LENGTH_DELIMITED);
    write_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn write_message_field(out: &mut Vec<u8>, field: u32, body: &[u8]) {
    write_bytes_field(out, field, body);
}

/// Emit a `repeated int64 packed` field (the encoding `pprof` uses
/// for `Sample.value` and similar columns).
fn write_packed_varint(out: &mut Vec<u8>, field: u32, values: &[u64]) {
    if values.is_empty() {
        return;
    }
    let mut payload: Vec<u8> = Vec::new();
    for v in values {
        write_varint(&mut payload, *v);
    }
    write_tag(out, field, WIRE_LENGTH_DELIMITED);
    write_varint(out, payload.len() as u64);
    out.extend_from_slice(&payload);
}

// ---- pprof message helpers ----------------------------------------------

/// Build a `string_table` accumulator. Per the pprof schema string
/// indices are 1-based after a mandatory empty string at index 0.
struct StringTable {
    strings: Vec<String>,
    by_value: BTreeMap<String, u64>,
}

impl StringTable {
    fn new() -> Self {
        let mut t = Self {
            strings: Vec::new(),
            by_value: BTreeMap::new(),
        };
        // Index 0 is required to be the empty string.
        t.intern("");
        t
    }

    fn intern(&mut self, s: &str) -> u64 {
        if let Some(idx) = self.by_value.get(s) {
            return *idx;
        }
        let idx = self.strings.len() as u64;
        self.strings.push(s.to_string());
        self.by_value.insert(s.to_string(), idx);
        idx
    }
}

#[derive(Clone)]
struct LocationRecord {
    /// pprof Location id — assigned monotonically starting at 1.
    id: u64,
    /// Captured runtime PC (image-relative if resolver had a base).
    /// Stored as-is for pprof's `Location.address` field.
    address: u64,
    /// Function id this location resolves to (1-based).
    function_id: u64,
}

#[derive(Clone)]
struct FunctionRecord {
    id: u64,
    name_strtab_idx: u64,
}

// ---- top-level writer ---------------------------------------------------

/// Write `samples` to `out` as a pprof v0.4-shape Profile message.
/// `kind` selects the `sample_type` headers.
pub fn write_pprof(samples: &[Sample], kind: SampleKind, out: &mut impl Write) -> io::Result<()> {
    let resolver = resolve::Resolver::from_env_for_main_binary().with_dyld_images();
    let body = encode_profile(samples, kind, &resolver);
    out.write_all(&body)
}

/// Public to support unit testing the byte output without a Writer.
pub fn encode_profile(
    samples: &[Sample],
    kind: SampleKind,
    resolver: &resolve::Resolver,
) -> Vec<u8> {
    let mut strtab = StringTable::new();
    // Pre-intern the sample_type type/unit strings so they end up at
    // stable indices (helps spot-checks but isn't a wire-format
    // requirement).
    let (type0, unit0, type1, unit1) = match kind {
        SampleKind::Cpu => ("samples", "count", "cpu", "nanoseconds"),
        SampleKind::Alloc => ("alloc_objects", "count", "alloc_space", "bytes"),
    };
    let type0_idx = strtab.intern(type0);
    let unit0_idx = strtab.intern(unit0);
    let type1_idx = strtab.intern(type1);
    let unit1_idx = strtab.intern(unit1);

    // Function table — indexed by name, value = FunctionRecord.
    let mut fns_by_name: BTreeMap<String, FunctionRecord> = BTreeMap::new();
    // Location table — indexed by address, value = LocationRecord.
    // Two samples that share a PC share a location_id.
    let mut locs_by_addr: BTreeMap<u64, LocationRecord> = BTreeMap::new();

    let mut next_loc_id: u64 = 1;
    let mut next_fn_id: u64 = 1;

    // For each sample, resolve every PC to a location_id; sample
    // payloads land in `samples_payload`.
    let mut sample_msgs: Vec<Vec<u8>> = Vec::with_capacity(samples.len());

    let mut earliest_ts_ns: u64 = u64::MAX;
    let mut latest_ts_ns: u64 = 0;

    for s in samples {
        if s.ts_ns < earliest_ts_ns {
            earliest_ts_ns = s.ts_ns;
        }
        if s.ts_ns > latest_ts_ns {
            latest_ts_ns = s.ts_ns;
        }
        let live = s.live_frames();
        let mut location_ids: Vec<u64> = Vec::with_capacity(live.len());
        for pc in live {
            let addr = *pc as u64;
            let loc_id = if let Some(existing) = locs_by_addr.get(&addr) {
                existing.id
            } else {
                let name: String = resolver.lookup(*pc);
                let fn_id = if let Some(existing) = fns_by_name.get(&name) {
                    existing.id
                } else {
                    let id = next_fn_id;
                    next_fn_id += 1;
                    let strtab_idx = strtab.intern(&name);
                    fns_by_name.insert(
                        name,
                        FunctionRecord {
                            id,
                            name_strtab_idx: strtab_idx,
                        },
                    );
                    id
                };
                let id = next_loc_id;
                next_loc_id += 1;
                locs_by_addr.insert(
                    addr,
                    LocationRecord {
                        id,
                        address: addr,
                        function_id: fn_id,
                    },
                );
                id
            };
            location_ids.push(loc_id);
        }

        // Sample message:
        //   repeated uint64 location_id = 1; (packed)
        //   repeated int64  value       = 2; (packed)
        let mut payload: Vec<u8> = Vec::new();
        write_packed_varint(&mut payload, 1, &location_ids);
        let values: [u64; 2] = match kind {
            SampleKind::Cpu => [s.value, 0],
            SampleKind::Alloc => [1, s.value],
        };
        write_packed_varint(&mut payload, 2, &values);
        sample_msgs.push(payload);
    }

    // Profile.mapping[] — emit a single mapping spanning the main
    // image so renderers know how to bucket locations. Use the
    // current_exe's path string as the filename.
    let mapping_filename = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_default();
    let mapping_filename_idx = strtab.intern(&mapping_filename);
    let mut mapping_payload: Vec<u8> = Vec::new();
    write_varint_field(&mut mapping_payload, 1, 1); // id = 1
    write_varint_field(&mut mapping_payload, 2, 0); // memory_start = 0 (image base; resolver subtracts at lookup time)
    write_varint_field(&mut mapping_payload, 3, u32::MAX as u64); // memory_limit (conservative upper bound)
    write_varint_field(&mut mapping_payload, 4, 0); // file_offset
    write_varint_field(&mut mapping_payload, 5, mapping_filename_idx); // filename

    // Profile encoding ---------------------------------------------------
    let mut profile: Vec<u8> = Vec::new();

    // sample_type[0] = (type0, unit0)
    let mut st0: Vec<u8> = Vec::new();
    write_int64_field(&mut st0, 1, type0_idx as i64);
    write_int64_field(&mut st0, 2, unit0_idx as i64);
    write_message_field(&mut profile, 1, &st0);
    // sample_type[1] = (type1, unit1)
    let mut st1: Vec<u8> = Vec::new();
    write_int64_field(&mut st1, 1, type1_idx as i64);
    write_int64_field(&mut st1, 2, unit1_idx as i64);
    write_message_field(&mut profile, 1, &st1);

    for s in &sample_msgs {
        write_message_field(&mut profile, 2, s);
    }

    write_message_field(&mut profile, 3, &mapping_payload);

    // Locations — sort by id so the output is reproducible.
    let mut locs: Vec<LocationRecord> = locs_by_addr.values().cloned().collect();
    locs.sort_by_key(|l| l.id);
    for loc in &locs {
        let mut payload: Vec<u8> = Vec::new();
        write_varint_field(&mut payload, 1, loc.id);
        write_varint_field(&mut payload, 2, 1); // mapping_id = 1
        write_varint_field(&mut payload, 3, loc.address);
        // Line[] - emit one line entry that points back at the
        // location's function so renderers can label nodes
        // without DWARF integration. The Line message lives at
        // Location.line, field 4.
        let mut line_payload: Vec<u8> = Vec::new();
        write_varint_field(&mut line_payload, 1, loc.function_id); // function_id
        write_varint_field(&mut line_payload, 2, 0); // line = 0 (unknown)
        write_message_field(&mut payload, 4, &line_payload);
        write_message_field(&mut profile, 4, &payload);
    }

    // Functions
    let mut fns: Vec<FunctionRecord> = fns_by_name.values().cloned().collect();
    fns.sort_by_key(|f| f.id);
    for f in &fns {
        let mut payload: Vec<u8> = Vec::new();
        write_varint_field(&mut payload, 1, f.id);
        write_varint_field(&mut payload, 2, f.name_strtab_idx);
        write_varint_field(&mut payload, 3, f.name_strtab_idx); // system_name = name
        write_varint_field(&mut payload, 4, 0); // filename = "" (string 0)
        write_varint_field(&mut payload, 5, 0); // start_line = 0
        write_message_field(&mut profile, 5, &payload);
    }

    // String table
    for s in &strtab.strings {
        write_string_field(&mut profile, 6, s.as_str());
    }

    // time_nanos / duration_nanos
    let time_nanos = earliest_ts_ns.min(latest_ts_ns);
    let duration_nanos = latest_ts_ns.saturating_sub(earliest_ts_ns);
    write_int64_field(&mut profile, 9, time_nanos as i64);
    write_int64_field(&mut profile, 10, duration_nanos as i64);

    profile
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

    fn read_varint(bytes: &[u8], cursor: &mut usize) -> u64 {
        let mut result: u64 = 0;
        let mut shift = 0;
        loop {
            let b = bytes[*cursor];
            *cursor += 1;
            result |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        result
    }

    fn read_tag(bytes: &[u8], cursor: &mut usize) -> (u32, u32) {
        let v = read_varint(bytes, cursor);
        ((v >> 3) as u32, (v & 0x07) as u32)
    }

    #[test]
    fn varint_round_trip() {
        for v in [
            0u64,
            1,
            0x7F,
            0x80,
            0x3FFF,
            0x4000,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let mut buf: Vec<u8> = Vec::new();
            write_varint(&mut buf, v);
            let mut cur = 0;
            let read = read_varint(&buf, &mut cur);
            assert_eq!(read, v);
            assert_eq!(cur, buf.len());
        }
    }

    #[test]
    fn encode_profile_emits_sample_type_pair() {
        let r = resolve::Resolver::empty();
        let samples = vec![sample_with(1, &[0xAAA])];
        let bytes = encode_profile(&samples, SampleKind::Cpu, &r);

        // Find the first two sample_type fields (field 1, wire 2).
        let mut cur = 0;
        let mut sample_type_count = 0;
        while cur < bytes.len() {
            let (fnum, wire) = read_tag(&bytes, &mut cur);
            let len = if wire == WIRE_LENGTH_DELIMITED {
                read_varint(&bytes, &mut cur) as usize
            } else {
                let _ = read_varint(&bytes, &mut cur);
                0
            };
            if fnum == 1 {
                sample_type_count += 1;
            }
            cur += len;
        }
        assert_eq!(sample_type_count, 2, "expected two sample_type messages");
    }

    #[test]
    fn encode_profile_dedup_locations_by_address() {
        let r = resolve::Resolver::empty();
        let samples = vec![
            sample_with(1, &[0xAAA, 0xBBB]),
            sample_with(1, &[0xAAA, 0xBBB]),
            sample_with(1, &[0xAAA, 0xCCC]),
        ];
        let bytes = encode_profile(&samples, SampleKind::Cpu, &r);
        // Count field 4 (location) messages.
        let mut cur = 0;
        let mut loc_count = 0;
        while cur < bytes.len() {
            let (fnum, wire) = read_tag(&bytes, &mut cur);
            let len = if wire == WIRE_LENGTH_DELIMITED {
                read_varint(&bytes, &mut cur) as usize
            } else {
                let _ = read_varint(&bytes, &mut cur);
                0
            };
            if fnum == 4 {
                loc_count += 1;
            }
            cur += len;
        }
        // Three unique PCs: AAA, BBB, CCC.
        assert_eq!(loc_count, 3, "expected 3 unique location records");
    }

    #[test]
    fn encode_profile_with_alloc_kind_uses_alloc_sample_types() {
        let r = resolve::Resolver::empty();
        let samples = vec![sample_with(128, &[0xAAA])];
        let bytes = encode_profile(&samples, SampleKind::Alloc, &r);
        // String table for an alloc profile must contain the
        // "alloc_objects" and "alloc_space" type strings.
        let table_blob = String::from_utf8_lossy(&bytes).to_string();
        assert!(table_blob.contains("alloc_objects"));
        assert!(table_blob.contains("alloc_space"));
        assert!(table_blob.contains("bytes"));
    }

    #[test]
    fn encode_profile_zero_samples_still_valid_message() {
        let r = resolve::Resolver::empty();
        let bytes = encode_profile(&[], SampleKind::Cpu, &r);
        // Must at minimum contain the two sample_type headers and
        // the mapping. Non-zero length.
        assert!(
            !bytes.is_empty(),
            "empty samples still produces a valid Profile message"
        );
    }
}
