//! Grammar-neutral execution of nibble tables.
//!
//! This crate is stage 1 of a simdjson-style pipeline: it classifies input in
//! 64-byte blocks with a scalar, SSSE3, or AVX2 backend, lets an
//! [`IndexSpec`] refine each block's masks, and records the result either as
//! a global offset tape or as per-block masks (see [`IndexMode`]). It never
//! interprets the result; stage-2 state machines consume an [`IndexView`].
//!
//! Tables are plain data ([`NibbleTables`]), so this crate does not depend on
//! how they were produced. `simdlex-tablegen` generates them at compile time.

mod backend;
mod index;
mod spec;

pub use backend::{BackendError, BackendKind, best_backend, classify_block, resolve_backend};
pub use index::{
    IndexView, MaskLanes, MaskLayout, MaskView, Positions, SAFETY_PADDING, StructuralIndex,
};
pub use spec::{ClassifiedBlock, IndexMode, IndexSpec, NibbleTables, Utf8Policy};

/// Work counters for one scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexMetrics {
    pub blocks: usize,
}

type RunFn<S> = fn(
    &[u8],
    IndexMode,
    &mut <S as IndexSpec>::State,
    &mut StructuralIndex,
) -> Result<IndexMetrics, <S as IndexSpec>::Error>;

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
            _ => scalar::<S>,
        };
        Ok(Self { run, backend })
    }

    #[inline]
    pub fn backend(&self) -> BackendKind {
        self.backend
    }

    /// The mask layout `S` produces in [`IndexMode::Masks`].
    #[inline]
    pub const fn layout() -> MaskLayout {
        MaskLayout {
            classes: S::CLASS_COUNT,
            exacts: S::EXACT_COUNT,
        }
    }

    /// Scan `input` into `out`, replacing its previous contents. `state` is
    /// reset first. Does not allocate when `out` was reserved for at least
    /// `input.len()` bytes in `mode`.
    #[inline]
    pub fn index(
        &self,
        input: &[u8],
        mode: IndexMode,
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<IndexMetrics, S::Error> {
        assert!(
            input.len() < u32::MAX as usize,
            "inputs must be shorter than u32::MAX bytes"
        );
        (self.run)(input, mode, state, out)
    }
}

fn scalar<S: IndexSpec>(
    input: &[u8],
    mode: IndexMode,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<IndexMetrics, S::Error> {
    match mode {
        IndexMode::Tape => run::<S, false>(input, state, out, backend::classify_scalar::<S>),
        IndexMode::Masks => run::<S, true>(input, state, out, backend::classify_scalar::<S>),
    }
}

#[cfg(target_arch = "x86_64")]
fn ssse3<S: IndexSpec>(
    input: &[u8],
    mode: IndexMode,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<IndexMetrics, S::Error> {
    #[target_feature(enable = "ssse3")]
    unsafe fn inner<S: IndexSpec, const MASKS: bool>(
        input: &[u8],
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<IndexMetrics, S::Error> {
        run::<S, MASKS>(input, state, out, |block| unsafe {
            backend::classify_ssse3::<S>(block)
        })
    }

    // SAFETY: `Indexer::with_backend` only selects this function after
    // detecting SSSE3.
    unsafe {
        match mode {
            IndexMode::Tape => inner::<S, false>(input, state, out),
            IndexMode::Masks => inner::<S, true>(input, state, out),
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn avx2<S: IndexSpec>(
    input: &[u8],
    mode: IndexMode,
    state: &mut S::State,
    out: &mut StructuralIndex,
) -> Result<IndexMetrics, S::Error> {
    #[target_feature(enable = "avx2")]
    unsafe fn inner<S: IndexSpec, const MASKS: bool>(
        input: &[u8],
        state: &mut S::State,
        out: &mut StructuralIndex,
    ) -> Result<IndexMetrics, S::Error> {
        run::<S, MASKS>(input, state, out, |block| unsafe {
            backend::classify_avx2::<S>(block)
        })
    }

    // SAFETY: `Indexer::with_backend` only selects this function after
    // detecting AVX2.
    unsafe {
        match mode {
            IndexMode::Tape => inner::<S, false>(input, state, out),
            IndexMode::Masks => inner::<S, true>(input, state, out),
        }
    }
}

/// The block loop shared by every backend. `MASKS` selects the output
/// representation at compile time, so neither mode pays for the other.
#[inline]
fn run<S: IndexSpec, const MASKS: bool>(
    input: &[u8],
    state: &mut S::State,
    out: &mut StructuralIndex,
    classify: impl Fn(&[u8; 64]) -> ClassifiedBlock,
) -> Result<IndexMetrics, S::Error> {
    *state = S::State::default();
    let mode = if MASKS {
        IndexMode::Masks
    } else {
        IndexMode::Tape
    };
    out.prepare(mode, Indexer::<S>::layout(), input.len());

    if S::UTF8_POLICY == Utf8Policy::Validate
        && let Err(error) = std::str::from_utf8(input)
    {
        return Err(S::utf8_error(error.valid_up_to() as u32));
    }

    let mut emit_block = |block: &[u8; 64],
                          base: u32,
                          valid: u64,
                          masks: &mut ClassifiedBlock|
     -> Result<(), S::Error> {
        let structural = S::scan_block(block, base, valid, masks, state)? & valid;
        if MASKS {
            out.push_block(structural, masks);
        } else {
            out.flatten(structural, base);
        }
        Ok(())
    };

    let full_blocks = input.len() / 64;
    for block_index in 0..full_blocks {
        let start = block_index * 64;
        let block: &[u8; 64] = input[start..start + 64].try_into().unwrap();
        let mut masks = classify(block);
        emit_block(block, start as u32, u64::MAX, &mut masks)?;
    }

    let tail_start = full_blocks * 64;
    if tail_start < input.len() {
        let tail_len = input.len() - tail_start;
        let mut padded = [0u8; 64];
        padded[..tail_len].copy_from_slice(&input[tail_start..]);
        let valid = (1u64 << tail_len) - 1;
        let mut masks = classify(&padded);
        masks.retain(valid);
        emit_block(&padded, tail_start as u32, valid, &mut masks)?;
    }

    S::finish(input.len() as u32, state)?;
    Ok(IndexMetrics {
        blocks: full_blocks + usize::from(tail_start < input.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use simdlex_tablegen::{ByteSet, Tables, generate, unwrap};

    const GENERATED: Tables<3> = unwrap(generate(&[
        ByteSet::from_bytes(b"<>"),
        ByteSet::from_bytes(b"="),
        ByteSet::from_bytes(b" \n"),
    ]));

    /// Indexes `<`, `>`, and `=`; whitespace is classified but not
    /// structural, so it only appears in mask mode.
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
        const EXACT_BYTES: [u8; 4] = [b'x', 0, 0, 0];
        const EXACT_COUNT: usize = 1;
        const UTF8_POLICY: Utf8Policy = Utf8Policy::ByteOriented;
        const MODE: IndexMode = IndexMode::Tape;

        fn scan_block(
            _input: &[u8; 64],
            _base: u32,
            _valid: u64,
            masks: &mut ClassifiedBlock,
            _state: &mut (),
        ) -> Result<u64, ()> {
            Ok(masks.class[0] | masks.class[1])
        }

        fn finish(_input_len: u32, _state: &mut ()) -> Result<(), ()> {
            Ok(())
        }

        fn utf8_error(_offset: u32) {}
    }

    fn backends() -> Vec<BackendKind> {
        [BackendKind::Scalar, BackendKind::Ssse3, BackendKind::Avx2]
            .into_iter()
            .filter(|&backend| resolve_backend(backend).is_ok())
            .collect()
    }

    #[test]
    fn backends_classify_every_byte_identically() {
        for group in 0..4 {
            let block: [u8; 64] = std::array::from_fn(|lane| (group * 64 + lane) as u8);
            let scalar = classify_block::<Demo>(BackendKind::Scalar, &block).unwrap();
            for backend in backends() {
                assert_eq!(classify_block::<Demo>(backend, &block).unwrap(), scalar);
            }
        }
    }

    #[test]
    fn tape_and_masks_agree_for_alignments_and_tails() {
        let template = b"<a b=c>\n<d e = 'f'> x <g/>";
        for repeat in [1, 3, 9] {
            let input = template.repeat(repeat);
            for start in 0..input.len().min(70) {
                let input = &input[start..];
                let expected: Vec<u32> = input
                    .iter()
                    .enumerate()
                    .filter(|(_, byte)| matches!(byte, b'<' | b'>' | b'='))
                    .map(|(offset, _)| offset as u32)
                    .collect();

                for backend in backends() {
                    let indexer = Indexer::<Demo>::with_backend(backend).unwrap();
                    let mut index = StructuralIndex::new();

                    indexer
                        .index(input, IndexMode::Tape, &mut (), &mut index)
                        .unwrap();
                    assert_eq!(index.view().tape().unwrap(), expected);

                    indexer
                        .index(input, IndexMode::Masks, &mut (), &mut index)
                        .unwrap();
                    let view = index.view();
                    assert_eq!(view.positions().collect::<Vec<_>>(), expected);
                    assert_eq!(view.structural_count(), expected.len());

                    let masks = view.masks().unwrap();
                    assert_eq!(masks.blocks(), input.len().div_ceil(64));
                    let whitespace: Vec<u32> = masks.class_positions(2).collect();
                    let expected_whitespace: Vec<u32> = input
                        .iter()
                        .enumerate()
                        .filter(|(_, byte)| matches!(byte, b' ' | b'\n'))
                        .map(|(offset, _)| offset as u32)
                        .collect();
                    assert_eq!(whitespace, expected_whitespace);
                    let xs: u32 = (0..masks.blocks())
                        .map(|block| masks.exact(block, 0).count_ones())
                        .sum();
                    assert_eq!(xs as usize, input.iter().filter(|&&b| b == b'x').count());
                }
            }
        }
    }

    #[test]
    fn mask_searches_match_byte_scans() {
        let mut input = b"<a  b=c>  \n".repeat(20);
        input.extend_from_slice(&[b' '; 130]);
        input.push(b'=');
        let indexer = Indexer::<Demo>::new();
        let mut index = StructuralIndex::new();
        indexer
            .index(&input, IndexMode::Masks, &mut (), &mut index)
            .unwrap();
        let masks = index.view().masks().unwrap();

        for from in 0..=input.len() + 70 {
            let expected_next = input
                .iter()
                .enumerate()
                .skip(from)
                .find(|(_, byte)| matches!(byte, b'=' | b' ' | b'\n'))
                .map(|(offset, _)| offset);
            assert_eq!(masks.next_in(0b110, from), expected_next, "from {from}");

            let expected_run = input
                .iter()
                .skip(from)
                .take_while(|&&byte| matches!(byte, b' ' | b'\n'))
                .count();
            assert_eq!(masks.run_length(2, from), expected_run, "from {from}");
        }
    }

    #[test]
    fn reserved_index_is_reused() {
        let indexer = Indexer::<Demo>::new();
        let mut index = StructuralIndex::new();
        index
            .try_reserve(IndexMode::Masks, Indexer::<Demo>::layout(), 256)
            .unwrap();
        let capacity = index.capacity_bytes();
        indexer
            .index(&[b'<'; 256], IndexMode::Masks, &mut (), &mut index)
            .unwrap();
        assert_eq!(index.capacity_bytes(), capacity);
        assert_eq!(index.view().bytes(), 4 * 5 * 8);
    }
}
