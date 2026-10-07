//! simdjson-style structural indexing as a primitive.
//!
//! A two-stage parser first finds the bounds of its grammar's regions with
//! SIMD (stage 1), then runs a state machine that jumps between those bounds
//! (stage 2). simdlex provides stage 1 for any grammar whose bounds are
//! single bytes:
//!
//! - [`tablegen`] turns named byte sets into nibble lookup tables at compile
//!   time, failing the build if they cannot be classified exactly.
//! - [`Indexer`] runs those tables over 64-byte blocks with a scalar, SSSE3,
//!   AVX2, or NEON backend and records a tape of structural offsets, plus
//!   per-block masks for dense classes, whole or in windows.
//! - [`TapeCursor`] and [`MaskView`] are what a stage-2 state machine walks.
//!
//! ```
//! use simdlex::tablegen::{ByteSet, Tables, generate, unwrap};
//! use simdlex::{IndexSpec, Indexer, NibbleTables, StructuralIndex};
//!
//! const TABLES: Tables<2> = unwrap(generate(&[
//!     ByteSet::from_bytes(b"<>"),
//!     ByteSet::from_bytes(b"'\""),
//! ]));
//!
//! /// Tag delimiters and quotes, the bounds an HTML tokenizer searches for.
//! struct Html;
//!
//! impl IndexSpec for Html {
//!     type State = ();
//!     type Error = std::convert::Infallible;
//!     const TABLES: NibbleTables = NibbleTables { low: TABLES.low, high: TABLES.high };
//!     const CLASS_BITS: [u8; 8] = TABLES.padded_class_bits();
//!     const CLASS_COUNT: usize = 2;
//! }
//!
//! let input = br#"<a href="x">hi</a>"#;
//! let mut index = StructuralIndex::new();
//! Indexer::<Html>::new().index(input, &mut (), &mut index).unwrap();
//! assert_eq!(index.tape(), [0, 8, 10, 11, 14, 17]);
//!
//! let mut cursor = index.cursor();
//! assert_eq!(cursor.find(input, 1, |byte| byte == b'>'), Some(11));
//! ```
//!
//! The `examples/` directory holds complete grammars built on this crate:
//! an HTML element stream, a JSON parser compared against simdjson, and a
//! YAML event stream.

pub use simdlex_index::*;

/// Compile-time nibble table generation.
pub mod tablegen {
    pub use simdlex_tablegen::*;
}

/// Compiles the README's example as a doctest.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
