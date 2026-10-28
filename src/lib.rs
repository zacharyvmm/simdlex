//! simdjson-style structural indexing as a primitive.
//!
//! A two-stage parser first finds the bounds of its grammar's regions with
//! SIMD (stage 1), then runs a state machine that jumps between those bounds
//! (stage 2). Inside known bounds the state machine's hot loop branches
//! regularly and predicts well; the unpredictable decisions happen once per
//! bound instead of once per byte. simdlex provides stage 1 for any grammar
//! whose bounds are single bytes:
//!
//! ```text
//! tablegen   [ByteSet; N]   ──► NibbleTables                     (compile time)
//! Indexer    input + IndexSpec ──► StructuralIndex { tape, masks }
//! stage 2    TapeCursor over the tape, MaskView over the masks    (yours)
//! ```
//!
//! - [`tablegen`] turns named byte sets into nibble lookup tables at compile
//!   time, failing the build if they cannot be classified exactly.
//! - [`Indexer`] runs those tables over 64-byte blocks with a scalar, SSSE3,
//!   AVX2, or NEON backend, lets the [`IndexSpec`] refine each block (for
//!   example, dropping bytes inside strings), and records a tape of
//!   structural offsets plus per-block masks for dense classes. Inputs can be
//!   indexed whole ([`Indexer::index`]) or in windows
//!   ([`Indexer::index_window`]), so a parser that may stop early only pays
//!   for the part of the input it reaches.
//! - [`TapeCursor`] and [`MaskView`] are what a stage-2 state machine walks.
//!
//! Index only the bytes stage 2 must search for: a byte it reads in place
//! (such as HTML's `=` or YAML's `:`) only costs stage-1 time and gets
//! skipped. See `docs/stage1-index.md` for the measurements behind this rule.
//!
//! ```
//! use simdlex::tablegen::{ByteSet, generate, unwrap};
//! use simdlex::{IndexSpec, Indexer, NibbleTables, StructuralIndex};
//!
//! /// Tag delimiters and quotes, the bounds an HTML tokenizer searches for.
//! struct Html;
//!
//! impl IndexSpec for Html {
//!     type State = ();
//!     type Error = std::convert::Infallible;
//!     const TABLES: NibbleTables = unwrap(generate(&[
//!         ByteSet::from_bytes(b"<>"),
//!         ByteSet::from_bytes(b"'\""),
//!     ]));
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

mod backend;
mod index;
mod indexer;
mod spec;
pub mod tablegen;

pub use backend::{
    BackendError, BackendKind, available_backends, best_backend, classify_block, resolve_backend,
};
pub use index::{MaskView, SAFETY_PADDING, StructuralIndex, TapeCursor};
pub use indexer::Indexer;
pub use spec::{ClassifiedBlock, IndexSpec, NibbleTables};

/// Compiles the README's example as a doctest.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
