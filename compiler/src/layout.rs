//! Plan A3 task 40 — per-type layout descriptors for user-defined
//! nominal types. Each `type` declaration resolves to a `TypeLayout`
//! that codegen (task 41) bakes into allocation sites and match
//! decision trees.
//!
//! # Layout shape
//!
//! Every variant of every user type occupies a heap record whose first
//! 8 bytes are the standard object header (`sigil_header_constants`):
//! tag (8 bits) + payload-word count (6 bits) + pointer bitmap (32
//! bits). Payload words follow:
//!
//! | Word | Content                                               |
//! |------|-------------------------------------------------------|
//! | 0    | 1-byte discriminant (low byte); upper 7 bytes pad     |
//! | 1..N | Fields in declaration order                           |
//!
//! The pointer bitmap reflects payload-word pointerness: bit 0 is
//! always 0 (the discriminant word is not a pointer), bits 1..N are 1
//! for every field whose type is String / Fn / User (all three are
//! GC-managed heap pointers). This agrees with the closure-record
//! convention in `sigil_header_constants` — bit 0 = payload word 0.
//!
//! # Tag assignment
//!
//! A single tag per user type; the variant is discriminated by the
//! byte in payload word 0. Tags start at 0x10 (0x00–0x0F reserved for
//! runtime primitives — `TAG_STRING=0x01`, `TAG_INT64=0x02`,
//! `TAG_CLOSURE=0x03` today; 0xFF is the v2 external-descriptor
//! escape hatch).
//!
//! Iteration order is the BTreeMap key order (alphabetical), which
//! gives every compilation of the same program the same tags —
//! important for reproducibility.
//!
//! # E0130
//!
//! Payload word count is bounded by the header's 6-bit count field
//! (0..=63). A variant whose `1 + field_count > 63` trips the plan's
//! E0130 diagnostic. Plan A3's prompt bank stays well under the limit;
//! the check exists to fail loudly rather than silently truncate.

use crate::ast::{TypeDecl, VariantFields};
use crate::typecheck::{ty_from_type_expr, Ty};
use std::collections::BTreeMap;

/// First type tag available for user types. Reserved block below is
/// runtime primitives (`TAG_STRING=0x01`, `TAG_INT64=0x02`,
/// `TAG_CLOSURE=0x03`) plus headroom for near-future primitives.
pub const USER_TAG_START: u8 = 0x10;

/// Maximum allowed payload word count before E0130 fires. The
/// 6-bit count field maxes at 63.
pub const MAX_PAYLOAD_WORDS: u8 = 63;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeLayout {
    pub type_name: String,
    /// Shared across every variant of this type; the discriminant
    /// distinguishes variants.
    pub type_tag: u8,
    pub variants: Vec<VariantLayout>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariantLayout {
    pub name: String,
    /// 0..variants.len()-1; written to payload word 0 at construction,
    /// read at match dispatch.
    pub discriminant: u8,
    /// Payload word count: 1 (discriminant word) + field count.
    pub payload_words: u8,
    /// Pointer bitmap: bit k = payload word k is a GC-managed pointer.
    /// Bit 0 is always 0 (discriminant). Bits 1..N reflect field
    /// pointer-ness.
    pub pointer_bitmap: u32,
    /// Per-field static type, in declaration order. Used by codegen
    /// to choose the right load/store width when binding pattern vars.
    pub field_tys: Vec<Ty>,
    /// Record field names in declaration order, parallel to `field_tys`.
    /// Empty for Unit / Positional variants.
    pub field_names: Vec<String>,
}

impl VariantLayout {
    /// Field count (excludes the discriminant word).
    pub fn field_count(&self) -> usize {
        self.field_tys.len()
    }

    /// Byte offset past the object header to payload word k.
    /// `k = 0` → discriminant word; `k = i + 1` → field `i`.
    pub fn field_word_offset(k: usize) -> i32 {
        (k as i32) * 8
    }
}

/// Construct layouts for every user-defined type in `types`. Returns
/// errors for any type whose payload exceeds 63 words (E0130). The
/// returned map is keyed by type name and iterates in insertion order
/// (BTreeMap ordering = alphabetical).
///
/// The `reserved_tags` parameter lets the caller account for user types
/// whose tags are fixed by some external source (none in Plan A3; kept
/// as a seam for Plan B stdlib types that may claim specific tags).
pub fn build_layouts(
    types: &BTreeMap<String, TypeDecl>,
) -> Result<BTreeMap<String, TypeLayout>, LayoutError> {
    let mut layouts: BTreeMap<String, TypeLayout> = BTreeMap::new();
    let mut next_tag: u8 = USER_TAG_START;
    for (name, td) in types {
        if next_tag == sigil_header_constants::TAG_EXTERNAL_DESCRIPTOR {
            return Err(LayoutError::TooManyTypes {
                next_tag_attempted: next_tag,
            });
        }
        let type_tag = next_tag;
        next_tag = next_tag.saturating_add(1);
        let mut variants = Vec::new();
        for (idx, v) in td.variants.iter().enumerate() {
            let field_tys: Vec<Ty> = match &v.fields {
                VariantFields::Unit => Vec::new(),
                VariantFields::Positional(ts) => ts
                    .iter()
                    .map(|t| ty_from_type_expr(t, types).unwrap_or(Ty::Unit))
                    .collect(),
                VariantFields::Record(fs) => fs
                    .iter()
                    .map(|f| ty_from_type_expr(&f.ty, types).unwrap_or(Ty::Unit))
                    .collect(),
            };
            let field_names: Vec<String> = match &v.fields {
                VariantFields::Unit | VariantFields::Positional(_) => Vec::new(),
                VariantFields::Record(fs) => fs.iter().map(|f| f.name.clone()).collect(),
            };
            // Payload = 1 (discriminant) + field count.
            let raw = 1usize + field_tys.len();
            if raw > MAX_PAYLOAD_WORDS as usize {
                return Err(LayoutError::PayloadTooLarge {
                    type_name: td.name.clone(),
                    variant_name: v.name.clone(),
                    words: raw,
                });
            }
            let payload_words = raw as u8;
            let pointer_bitmap = compute_pointer_bitmap(&field_tys);
            variants.push(VariantLayout {
                name: v.name.clone(),
                discriminant: idx as u8,
                payload_words,
                pointer_bitmap,
                field_tys,
                field_names,
            });
        }
        layouts.insert(
            name.clone(),
            TypeLayout {
                type_name: td.name.clone(),
                type_tag,
                variants,
            },
        );
    }
    Ok(layouts)
}

/// Compute the pointer bitmap for a positional/record field list.
/// Bit 0 is always 0 (discriminant word); bit `i+1` is set iff field
/// `i` has a GC-pointer type.
fn compute_pointer_bitmap(fields: &[Ty]) -> u32 {
    let mut bm: u32 = 0;
    for (i, ty) in fields.iter().enumerate() {
        if is_gc_pointer_ty(ty) {
            bm |= 1 << (i + 1);
        }
    }
    bm
}

/// Whether a `Ty` is represented at runtime as a GC-managed heap
/// pointer (String, Fn/closure record, or another user type).
pub fn is_gc_pointer_ty(ty: &Ty) -> bool {
    matches!(ty, Ty::String | Ty::Fn(_) | Ty::User(_))
}

/// Build an O(1) constructor-name → (type_name, variant_index) index
/// from the layouts map. Plan A3 enforces unique ctor names across
/// types (E0118 at typecheck), so the map has no duplicate entries
/// by construction.
pub fn build_ctor_index(
    layouts: &BTreeMap<String, TypeLayout>,
) -> BTreeMap<String, (String, usize)> {
    let mut idx = BTreeMap::new();
    for (type_name, layout) in layouts {
        for (i, v) in layout.variants.iter().enumerate() {
            idx.insert(v.name.clone(), (type_name.clone(), i));
        }
    }
    idx
}

/// Errors returned by `build_layouts`. Codegen converts these into the
/// plan's user-facing diagnostics (E0130 etc.).
#[derive(Clone, Debug)]
pub enum LayoutError {
    /// A variant's `1 + field_count` exceeds the header's 6-bit
    /// payload-word field (63 words). Maps to E0130.
    PayloadTooLarge {
        type_name: String,
        variant_name: String,
        words: usize,
    },
    /// The program registers more user types than the 8-bit type tag
    /// space minus reservations admits. Currently unreachable for Plan
    /// A3 surface programs (243 tags between 0x10 and 0xFE); kept as
    /// a defensive error.
    TooManyTypes { next_tag_attempted: u8 },
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::PayloadTooLarge {
                type_name,
                variant_name,
                words,
            } => write!(
                f,
                "user type `{type_name}` variant `{variant_name}` has {words} payload words (limit 63)"
            ),
            LayoutError::TooManyTypes {
                next_tag_attempted,
            } => write!(
                f,
                "ran out of user-type tags at 0x{next_tag_attempted:02x} (reserve 0x10..0xFE)"
            ),
        }
    }
}

/// Render a header word for the given variant using the shared
/// `header_word` formula. Const so codegen consumes the value as an
/// immediate at allocation sites.
pub const fn variant_header_word(type_tag: u8, variant: &VariantLayout) -> u64 {
    // `sigil_header_constants::header_word` is `const fn`; re-exported
    // here so codegen never duplicates the bit layout formula.
    sigil_header_constants::header_word(type_tag, variant.payload_words, variant.pointer_bitmap)
}

// `std::panic` and `unreachable!` are project-wide disallowed macros
// per the discipline greps; the #[cfg(test)] allow is explicit below.
#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::ast::{RecordFieldDecl, TypeDecl, TypeExpr, Variant};
    use crate::errors::Span;

    fn span() -> Span {
        Span::synthetic("t")
    }

    fn unit_variant(name: &str) -> Variant {
        Variant {
            name: name.to_string(),
            name_span: span(),
            fields: VariantFields::Unit,
            span: span(),
        }
    }

    fn pos_variant(name: &str, tys: Vec<&str>) -> Variant {
        Variant {
            name: name.to_string(),
            name_span: span(),
            fields: VariantFields::Positional(
                tys.into_iter()
                    .map(|t| TypeExpr::Named(t.to_string(), span()))
                    .collect(),
            ),
            span: span(),
        }
    }

    fn rec_variant(name: &str, fs: Vec<(&str, &str)>) -> Variant {
        Variant {
            name: name.to_string(),
            name_span: span(),
            fields: VariantFields::Record(
                fs.into_iter()
                    .map(|(n, t)| RecordFieldDecl {
                        name: n.to_string(),
                        ty: TypeExpr::Named(t.to_string(), span()),
                        span: span(),
                    })
                    .collect(),
            ),
            span: span(),
        }
    }

    fn td(name: &str, variants: Vec<Variant>) -> TypeDecl {
        TypeDecl {
            name: name.to_string(),
            name_span: span(),
            variants,
            span: span(),
        }
    }

    #[test]
    fn option_layout_tag_and_variants() {
        let mut types = BTreeMap::new();
        types.insert(
            "Option".to_string(),
            td(
                "Option",
                vec![unit_variant("None"), pos_variant("Some", vec!["Int"])],
            ),
        );
        let layouts = build_layouts(&types).expect("clean layout");
        let opt = &layouts["Option"];
        assert_eq!(opt.type_tag, USER_TAG_START);
        assert_eq!(opt.variants.len(), 2);
        // None: 1 word payload (discriminant only), bitmap 0.
        assert_eq!(opt.variants[0].discriminant, 0);
        assert_eq!(opt.variants[0].payload_words, 1);
        assert_eq!(opt.variants[0].pointer_bitmap, 0);
        assert_eq!(opt.variants[0].field_tys.len(), 0);
        // Some(Int): 2 word payload (discriminant + Int), bitmap 0
        // (Int is not a pointer).
        assert_eq!(opt.variants[1].discriminant, 1);
        assert_eq!(opt.variants[1].payload_words, 2);
        assert_eq!(opt.variants[1].pointer_bitmap, 0);
        assert_eq!(opt.variants[1].field_tys, vec![Ty::Int]);
    }

    #[test]
    fn list_layout_cons_bitmap_has_tail_pointer_bit_set() {
        let mut types = BTreeMap::new();
        types.insert(
            "List".to_string(),
            td(
                "List",
                vec![
                    unit_variant("Nil"),
                    pos_variant("Cons", vec!["Int", "List"]),
                ],
            ),
        );
        let layouts = build_layouts(&types).expect("clean layout");
        let list = &layouts["List"];
        let cons = &list.variants[1];
        // payload words = 1 (discriminant) + 2 (Int, List) = 3.
        assert_eq!(cons.payload_words, 3);
        // bit 1 = Int field (not pointer) = 0
        // bit 2 = List field (pointer) = 1 → bitmap = 0b100 = 4
        assert_eq!(cons.pointer_bitmap, 0b100);
    }

    #[test]
    fn string_field_sets_pointer_bitmap_bit() {
        let mut types = BTreeMap::new();
        types.insert(
            "Result".to_string(),
            td(
                "Result",
                vec![
                    pos_variant("Ok", vec!["Int"]),
                    pos_variant("Err", vec!["String"]),
                ],
            ),
        );
        let layouts = build_layouts(&types).expect("clean layout");
        let err = &layouts["Result"].variants[1];
        // bit 0 = discriminant (0), bit 1 = String (1) → bitmap = 0b10 = 2
        assert_eq!(err.pointer_bitmap, 0b10);
        assert_eq!(err.field_tys, vec![Ty::String]);
    }

    #[test]
    fn record_variant_field_names_preserved() {
        let mut types = BTreeMap::new();
        types.insert(
            "Point".to_string(),
            td(
                "Point",
                vec![rec_variant("Point", vec![("x", "Int"), ("y", "Int")])],
            ),
        );
        let layouts = build_layouts(&types).expect("clean layout");
        let p = &layouts["Point"].variants[0];
        assert_eq!(p.field_tys, vec![Ty::Int, Ty::Int]);
        assert_eq!(p.field_names, vec!["x".to_string(), "y".to_string()]);
        assert_eq!(p.payload_words, 3); // discriminant + x + y
    }

    #[test]
    fn tags_increment_from_user_tag_start_alphabetically() {
        let mut types = BTreeMap::new();
        types.insert("Beta".to_string(), td("Beta", vec![unit_variant("B")]));
        types.insert("Alpha".to_string(), td("Alpha", vec![unit_variant("A")]));
        let layouts = build_layouts(&types).expect("clean layout");
        // BTreeMap iterates alphabetically: Alpha = 0x10, Beta = 0x11.
        assert_eq!(layouts["Alpha"].type_tag, USER_TAG_START);
        assert_eq!(layouts["Beta"].type_tag, USER_TAG_START + 1);
    }

    #[test]
    fn payload_over_63_words_is_e0130_shape_error() {
        // Construct a variant with 63 positional Int fields → 1 + 63 = 64 words.
        let tys: Vec<&str> = std::iter::repeat_n("Int", 63).collect();
        let mut types = BTreeMap::new();
        types.insert("Big".to_string(), td("Big", vec![pos_variant("Big", tys)]));
        let err = build_layouts(&types).expect_err("expected layout error");
        match err {
            LayoutError::PayloadTooLarge { words, .. } => assert_eq!(words, 64),
            other => {
                assert!(
                    matches!(other, LayoutError::PayloadTooLarge { .. }),
                    "wrong error variant: {other:?}"
                );
            }
        }
    }

    #[test]
    fn ctor_index_round_trips_names() {
        let mut types = BTreeMap::new();
        types.insert(
            "Option".to_string(),
            td(
                "Option",
                vec![unit_variant("None"), pos_variant("Some", vec!["Int"])],
            ),
        );
        let layouts = build_layouts(&types).expect("clean layout");
        let idx = build_ctor_index(&layouts);
        assert_eq!(idx["None"], ("Option".to_string(), 0));
        assert_eq!(idx["Some"], ("Option".to_string(), 1));
    }

    #[test]
    fn variant_header_word_composes_correctly() {
        let mut types = BTreeMap::new();
        types.insert(
            "List".to_string(),
            td(
                "List",
                vec![
                    unit_variant("Nil"),
                    pos_variant("Cons", vec!["Int", "List"]),
                ],
            ),
        );
        let layouts = build_layouts(&types).expect("clean layout");
        let list = &layouts["List"];
        let cons_hdr = variant_header_word(list.type_tag, &list.variants[1]);
        // Tag = 0x10 (low byte); count = 3 (bits 8..14); bitmap = 0b100 at bits 14..46
        let expected = sigil_header_constants::header_word(USER_TAG_START, 3, 0b100);
        assert_eq!(cons_hdr, expected);
    }

    #[test]
    fn is_gc_pointer_ty_matches_expected_types() {
        use crate::typecheck::FnSig;
        assert!(is_gc_pointer_ty(&Ty::String));
        assert!(is_gc_pointer_ty(&Ty::User("X".to_string())));
        assert!(is_gc_pointer_ty(&Ty::Fn(Box::new(FnSig {
            params: vec![],
            ret: Ty::Int,
            effects: vec![],
        }))));
        assert!(!is_gc_pointer_ty(&Ty::Int));
        assert!(!is_gc_pointer_ty(&Ty::Bool));
        assert!(!is_gc_pointer_ty(&Ty::Char));
        assert!(!is_gc_pointer_ty(&Ty::Byte));
        assert!(!is_gc_pointer_ty(&Ty::Unit));
    }
}
