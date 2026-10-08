//! simdjson-style structural indexing for any byte-level grammar.
//!
//! This crate is stage 1 of a two-stage parser. It classifies input in
//! 64-byte blocks with a scalar, SSSE3, AVX2, or NEON backend, lets an
//! [`IndexSpec`] refine each block (for example, removing bytes inside
//! strings), and records the structural offsets on a tape. Specs may also
//! keep per-block masks for dense classes such as indentation spaces. It
//! never interprets the result: a stage-2 state machine walks the tape with a
//! [`TapeCursor`] and the masks with a [`MaskView`].
//!
//! The index exists to find a grammar's *bounds* (tag edges, line ends,
//! closing quotes) so that the stage-2 state machine can be placed at the
//! start of a region and parse the content inside it. Inside known bounds
//! its hot loop branches regularly and predicts well; the unpredictable
//! decisions happen once per bound instead of once per byte. Index only the
//! bytes stage 2 must search for: a byte it reads in place (such as HTML's
//! `=` or YAML's `:`) only costs stage-1 time and gets skipped. See
//! `docs/stage1-index.md` for the measurements behind this rule.
//!
//! Inputs can be indexed whole ([`Indexer::index`]) or in windows
//! ([`Indexer::index_window`]), so a parser that may stop early (after a
//! first match, say) only pays for the part of the input it reaches.
//!
//! Tables are plain data ([`NibbleTables`]), so this crate does not depend on
//! how they were produced. `simdlex-tablegen` generates them at compile time.

mod backend;
mod index;
mod spec;

use std::ops::Range;

pub use backend::{
    BackendError, BackendKind, available_backends, best_backend, classify_block, resolve_backend,
};
pub use index::{MaskView, SAFETY_PADDING, StructuralIndex, TapeCursor};
pub use spec::{ClassifiedBlock, IndexSpec, NibbleTables};

type RunFn<S> = fn(
    &[u8],
    Range<usize>,
    &mut <S as IndexSpec>::State,
    &mut StructuralIndex,
) -> Result<(), <S as IndexSpec>::Error>;

/// A backend-specialized scanner for one [`IndexSpec`]. CPU detection happens
/// once, at construction.
pub struct Indexer<S: IndexSpec> {
    run: RunFn<S>,
    backend: BackendKind,
}

impl<S: IndexSpec> Clone for Indexer<S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S: IndexSpec> Copy for Indexer<S> {}

impl<S: IndexSpec> Default for Indexer<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: IndexSpec> std::fmt::Debug for Indexer<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Indexer")
            .field("backend", &self.backend)
            .finish()
    }
}

impl<S: IndexSpec> Indexer<S> {
    /// Use the fastest backend available on this CPU.
    pub fn new() -> Self {
        Self::with_backend(BackendKind::Auto)
            .expect("the automatically selected backend must be available")
    }

    pub fn with_backend(backend: BackendKind) -> Result<Self, BackendError> {
        let backend = resolve_backend(backend)?;
        let run: RunFn<S> = match backend {
            #[cfg(target_arch = "x86_64")]
            BackendKind::Ssse3 => ssse3::<S>,
            #[cfg(target_arch = "x86_64")]
            BackendKind::Avx2 => avx2::<S>,
            #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
            BackendKind::Neon => neon::<S>,
            _ => scalar::<S>,
        };
        Ok(Self { run, backend })
    }

    #[inline]
    pub fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Reserve `out` so that indexing windows of at most `max_window` bytes
    /// never allocates.
    pub fn reserve(
        out: &mut StructuralIndex,
        max_window: usize,
    ) -> Result<(), std::collections::TryReserveError> {
        out.try_reserve(S::MASK_CLASSES, max_window)
    }

    /// Index all of `input` into `out`, replacing its contents. `state` is
    /// reset first.
    #[inline]
    pub fn index(
        &self,
        input: &[u8],
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<(), S::Error> {
        *state = S::State::default();
        self.index_window(input, 0..input.len(), state, out)
    }

    /// Index `input[window]` into `out`, replacing its contents. Offsets on
    /// the tape stay absolute.
    ///
    /// Windows let a parser index ahead of itself a bounded amount at a time.
    /// Index consecutive windows with the same `state`, starting from
    /// `S::State::default()`, so that state such as "inside a string"
    /// carries across them. The window must start on a 64-byte boundary and
    /// end on one or at the end of `input`; [`IndexSpec::finish`] runs when it
    /// reaches the end.
    ///
    /// # Panics
    ///
    /// If the window is misaligned or out of bounds, or `input` is
    /// `u32::MAX` bytes or longer.
    #[inline]
    pub fn index_window(
        &self,
        input: &[u8],
        window: Range<usize>,
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<(), S::Error> {
        assert!(
            input.len() < u32::MAX as usize,
            "inputs must be shorter than u32::MAX bytes"
        );
        assert!(
            window.start.is_multiple_of(64)
                && window.start <= window.end
                && window.end <= input.len()
                && (window.end.is_multiple_of(64) || window.end == input.len()),
            "windows must start on a 64-byte boundary and end on one or at the end of input"
        );
        (self.run)(input, window, state, out)
    }
}

fn scalar<S: IndexSpec>(
    input: &[u8],
    window: Range<usize>,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<(), S::Error> {
    run::<S>(input, window, state, out, backend::classify_scalar::<S>)
}

#[cfg(target_arch = "x86_64")]
fn ssse3<S: IndexSpec>(
    input: &[u8],
    window: Range<usize>,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<(), S::Error> {
    #[target_feature(enable = "ssse3")]
    unsafe fn inner<S: IndexSpec>(
        input: &[u8],
        window: Range<usize>,
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<(), S::Error> {
        run::<S>(input, window, state, out, |block| unsafe {
            backend::classify_ssse3::<S>(block)
        })
    }

    // SAFETY: `Indexer::with_backend` only selects this function after
    // detecting SSSE3.
    unsafe { inner::<S>(input, window, state, out) }
}

#[cfg(target_arch = "x86_64")]
fn avx2<S: IndexSpec>(
    input: &[u8],
    window: Range<usize>,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<(), S::Error> {
    #[target_feature(enable = "avx2")]
    unsafe fn inner<S: IndexSpec>(
        input: &[u8],
        window: Range<usize>,
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<(), S::Error> {
        run::<S>(input, window, state, out, |block| unsafe {
            backend::classify_avx2::<S>(block)
        })
    }

    // SAFETY: `Indexer::with_backend` only selects this function after
    // detecting AVX2.
    unsafe { inner::<S>(input, window, state, out) }
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
fn neon<S: IndexSpec>(
    input: &[u8],
    window: Range<usize>,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<(), S::Error> {
    #[target_feature(enable = "neon")]
    unsafe fn inner<S: IndexSpec>(
        input: &[u8],
        window: Range<usize>,
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<(), S::Error> {
        run::<S>(input, window, state, out, |block| unsafe {
            backend::classify_neon::<S>(block)
        })
    }

    // SAFETY: NEON is part of the aarch64 baseline.
    unsafe { inner::<S>(input, window, state, out) }
}

/// The block loop shared by every backend.
#[inline]
fn run<S: IndexSpec>(
    input: &[u8],
    window: Range<usize>,
    state: &mut S::State,
    out: &mut StructuralIndex,
    classify: impl Fn(&[u8; 64]) -> ClassifiedBlock,
) -> Result<(), S::Error> {
    out.prepare(window.clone(), S::MASK_CLASSES);

    let mut emit_block = |block: &[u8; 64],
                          base: u32,
                          valid: u64,
                          masks: &mut ClassifiedBlock|
     -> Result<(), S::Error> {
        let structural = S::scan_block(block, base, valid, masks, state)? & valid;
        if S::MASK_CLASSES > 0 {
            out.push_masks(masks);
        }
        out.flatten(structural, base);
        Ok(())
    };

    let full_end = window.start + (window.end - window.start) / 64 * 64;
    for start in (window.start..full_end).step_by(64) {
        let block: &[u8; 64] = input[start..start + 64].try_into().unwrap();
        let mut masks = classify(block);
        emit_block(block, start as u32, u64::MAX, &mut masks)?;
    }

    if full_end < window.end {
        let tail_len = window.end - full_end;
        let mut padded = [0u8; 64];
        padded[..tail_len].copy_from_slice(&input[full_end..window.end]);
        let valid = (1u64 << tail_len) - 1;
        let mut masks = classify(&padded);
        masks.retain(valid);
        emit_block(&padded, full_end as u32, valid, &mut masks)?;
    }

    if window.end == input.len() {
        S::finish(input.len() as u32, state)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use simdlex_tablegen::{ByteSet, Tables, generate, unwrap};

    const GENERATED: Tables<3> = unwrap(generate(&[
        ByteSet::from_bytes(b" \n"),
        ByteSet::from_bytes(b"<>"),
        ByteSet::from_bytes(b"="),
    ]));

    /// Keeps whitespace as masks and puts `<`, `>`, and `=` on the tape.
    struct Demo;

    impl IndexSpec for Demo {
        type State = ();
        type Error = ();

        const TABLES: NibbleTables = NibbleTables {
            low: GENERATED.low,
            high: GENERATED.high,
        };
        const CLASS_BITS: [u8; 8] = GENERATED.padded_class_bits();
        const CLASS_COUNT: usize = 3;
        const MASK_CLASSES: usize = 1;
    }

    const QUOTES: Tables<2> = unwrap(generate(&[
        ByteSet::from_bytes(b"<"),
        ByteSet::from_bytes(b"\""),
    ]));

    /// `<` outside double quotes, with quote state carried across blocks and
    /// windows, and an error for an unterminated quote.
    struct Quoted;

    impl IndexSpec for Quoted {
        type State = bool;
        type Error = u32;

        const TABLES: NibbleTables = NibbleTables {
            low: QUOTES.low,
            high: QUOTES.high,
        };
        const CLASS_BITS: [u8; 8] = QUOTES.padded_class_bits();
        const CLASS_COUNT: usize = 2;

        fn scan_block(
            _input: &[u8; 64],
            _base: u32,
            _valid: u64,
            masks: &mut ClassifiedBlock,
            in_quote: &mut bool,
        ) -> Result<u64, u32> {
            let mut inside = 0u64;
            let mut quotes = masks.class[1];
            let mut open = *in_quote;
            let mut lane = 0;
            while lane < 64 {
                let next = if quotes == 0 {
                    64
                } else {
                    quotes.trailing_zeros()
                };
                if open {
                    let span = if next == 64 {
                        u64::MAX << lane
                    } else {
                        ((1u64 << next) - 1) & (u64::MAX << lane)
                    };
                    inside |= span;
                }
                if next == 64 {
                    break;
                }
                open = !open;
                quotes &= quotes - 1;
                lane = next + 1;
            }
            *in_quote = open;
            Ok(masks.class[0] & !inside)
        }

        fn finish(input_len: u32, in_quote: &mut bool) -> Result<(), u32> {
            if *in_quote { Err(input_len) } else { Ok(()) }
        }
    }

    fn sample(len: usize, alphabet: &[u8], seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                alphabet[(state % alphabet.len() as u64) as usize]
            })
            .collect()
    }

    #[test]
    fn backends_classify_every_byte_identically() {
        for group in 0..4 {
            let block: [u8; 64] = std::array::from_fn(|lane| (group * 64 + lane) as u8);
            let scalar = classify_block::<Demo>(BackendKind::Scalar, &block).unwrap();
            for backend in available_backends() {
                assert_eq!(classify_block::<Demo>(backend, &block).unwrap(), scalar);
            }
        }
    }

    #[test]
    fn neon_is_available_only_on_little_endian_aarch64() {
        let neon = cfg!(all(target_arch = "aarch64", target_endian = "little"));
        assert_eq!(resolve_backend(BackendKind::Neon).is_ok(), neon);
        assert_eq!(available_backends().any(|b| b == BackendKind::Neon), neon);
    }

    #[test]
    fn tape_and_masks_match_a_byte_scan_for_alignments_and_tails() {
        for len in [0, 1, 63, 64, 65, 127, 128, 129, 300] {
            let input = sample(len, b"<a b=c>\n <", len as u64 + 7);
            let expected_tape: Vec<u32> = (0..len)
                .filter(|&i| matches!(input[i], b'<' | b'>' | b'='))
                .map(|i| i as u32)
                .collect();
            let expected_spaces: Vec<usize> = (0..len)
                .filter(|&i| matches!(input[i], b' ' | b'\n'))
                .collect();

            for backend in available_backends() {
                let indexer = Indexer::<Demo>::with_backend(backend).unwrap();
                let mut index = StructuralIndex::new();
                indexer.index(&input, &mut (), &mut index).unwrap();
                assert_eq!(index.tape(), expected_tape, "{backend:?} len {len}");
                let masks = index.masks();
                assert_eq!(masks.blocks(), 0..len.div_ceil(64));
                assert_eq!(
                    masks.class_positions(0).collect::<Vec<_>>(),
                    expected_spaces
                );
                assert_eq!(
                    index.bytes(),
                    expected_tape.len() * 4 + len.div_ceil(64) * 8
                );
            }
        }
    }

    #[test]
    fn windows_concatenate_to_the_whole_index_and_carry_state() {
        let input = sample(1_000, b"<<\"ab ", 3);
        let indexer = Indexer::<Quoted>::new();
        let mut whole = StructuralIndex::new();
        let mut state = false;
        let whole_result = indexer.index(&input, &mut state, &mut whole);

        for window in [64, 128, 320, 960] {
            let mut tape = Vec::new();
            let mut state = false;
            let mut index = StructuralIndex::new();
            let mut result = Ok(());
            for start in (0..input.len()).step_by(window) {
                let end = (start + window).min(input.len());
                result = indexer.index_window(&input, start..end, &mut state, &mut index);
                assert_eq!(index.window(), start..end);
                assert!(
                    index
                        .tape()
                        .iter()
                        .all(|&o| (start..end).contains(&(o as usize)))
                );
                tape.extend_from_slice(index.tape());
            }
            assert_eq!(tape, whole.tape(), "window {window}");
            assert_eq!(result, whole_result);
        }
    }

    #[test]
    #[should_panic(expected = "64-byte boundary")]
    fn misaligned_windows_panic() {
        let input = [b' '; 200];
        Indexer::<Demo>::new()
            .index_window(&input, 10..100, &mut (), &mut StructuralIndex::new())
            .unwrap();
    }

    #[test]
    fn cursor_seek_and_find_match_linear_search() {
        let input = sample(2_000, b"<a>==  ", 11);
        let mut index = StructuralIndex::new();
        Indexer::<Demo>::new()
            .index(&input, &mut (), &mut index)
            .unwrap();
        let tape = index.tape();

        let mut cursor = index.cursor();
        let mut rng = 5u64;
        for _ in 0..2_000 {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let from = (rng >> 33) as usize % (input.len() + 5);
            let expected_seek = tape
                .iter()
                .position(|&o| o as usize >= from)
                .unwrap_or(tape.len());
            cursor.seek(from);
            assert_eq!(cursor.index(), expected_seek);

            let expected = tape
                .iter()
                .map(|&o| o as usize)
                .find(|&o| o >= from && input[o] == b'>');
            assert_eq!(cursor.find(&input, from, |b| b == b'>'), expected);
            let mut resumed = TapeCursor::at(tape, rng as usize % (tape.len() + 3));
            assert_eq!(resumed.find(&input, from, |b| b == b'>'), expected);
            if let Some(found) = expected {
                assert_eq!(cursor.peek(), Some(found));
                assert_eq!(cursor.find(&input, from, |b| b == b'>'), expected);
            }
        }
    }

    #[test]
    fn mask_searches_match_byte_scans() {
        let mut input = b"<a  b=c>  \n".repeat(20);
        input.extend_from_slice(&[b' '; 130]);
        input.push(b'=');
        let mut index = StructuralIndex::new();
        Indexer::<Demo>::new()
            .index(&input, &mut (), &mut index)
            .unwrap();
        let masks = index.masks();

        for from in 0..=input.len() + 70 {
            let expected_next = input
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, byte)| matches!(byte, b' ' | b'\n'))
                .map(|(offset, _)| offset);
            assert_eq!(masks.next_in(0b1, from), expected_next, "from {from}");

            let expected_run = input
                .iter()
                .skip(from)
                .take_while(|&&byte| matches!(byte, b' ' | b'\n'))
                .count();
            assert_eq!(masks.run_length(0, from), expected_run, "from {from}");
        }
    }

    #[test]
    fn reserved_index_is_reused() {
        let indexer = Indexer::<Demo>::new();
        let mut index = StructuralIndex::new();
        Indexer::<Demo>::reserve(&mut index, 256).unwrap();
        let capacity = index.capacity_bytes();
        indexer.index(&[b'<'; 256], &mut (), &mut index).unwrap();
        indexer.index(&[b' '; 200], &mut (), &mut index).unwrap();
        assert_eq!(index.capacity_bytes(), capacity);
    }
}
