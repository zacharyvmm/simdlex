use crate::grammar::Grammar;

pub(crate) const SAFETY_PADDING: usize = 8;

/// Grammar-selected class and exact-byte masks for one logical 64-byte block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClassifiedBlock {
    pub class: [u64; 8],
    pub exact: [u64; 4],
}

pub(crate) type Stage1Fn<G> = fn(
    &[u8],
    &mut <G as Grammar>::Stage1State,
    &mut Vec<u32>,
) -> Result<Stage1Metrics, <G as Grammar>::Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Stage1Metrics {
    pub blocks: usize,
}

pub(crate) fn scalar<G: Grammar>(
    input: &[u8],
    state: &mut G::Stage1State,
    tape: &mut Vec<u32>,
) -> Result<Stage1Metrics, G::Error> {
    run::<G>(input, state, tape, classify_scalar::<G>)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn ssse3<G: Grammar>(
    input: &[u8],
    state: &mut G::Stage1State,
    tape: &mut Vec<u32>,
) -> Result<Stage1Metrics, G::Error> {
    unsafe { ssse3_inner::<G>(input, state, tape) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn ssse3_inner<G: Grammar>(
    input: &[u8],
    state: &mut G::Stage1State,
    tape: &mut Vec<u32>,
) -> Result<Stage1Metrics, G::Error> {
    run::<G>(input, state, tape, |block| unsafe {
        classify_ssse3::<G>(block)
    })
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn avx2<G: Grammar>(
    input: &[u8],
    state: &mut G::Stage1State,
    tape: &mut Vec<u32>,
) -> Result<Stage1Metrics, G::Error> {
    unsafe { avx2_inner::<G>(input, state, tape) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn avx2_inner<G: Grammar>(
    input: &[u8],
    state: &mut G::Stage1State,
    tape: &mut Vec<u32>,
) -> Result<Stage1Metrics, G::Error> {
    run::<G>(input, state, tape, |block| unsafe {
        classify_avx2::<G>(block)
    })
}

#[inline]
fn run<G: Grammar>(
    input: &[u8],
    state: &mut G::Stage1State,
    tape: &mut Vec<u32>,
    classify: impl Fn(&[u8; 64]) -> ClassifiedBlock,
) -> Result<Stage1Metrics, G::Error> {
    *state = G::Stage1State::default();

    if G::UTF8_POLICY == crate::grammar::Utf8Policy::Validate
        && let Err(error) = std::str::from_utf8(input)
    {
        return Err(G::utf8_error(error.valid_up_to() as u32));
    }

    let full_blocks = input.len() / 64;
    for block_index in 0..full_blocks {
        let start = block_index * 64;
        let block: &[u8; 64] = input[start..start + 64].try_into().unwrap();
        let masks = classify(block);
        G::scan_block(block, start as u32, u64::MAX, &masks, state, tape)?;
    }

    let tail_start = full_blocks * 64;
    if tail_start < input.len() {
        let tail_len = input.len() - tail_start;
        let mut padded = [0u8; 64];
        padded[..tail_len].copy_from_slice(&input[tail_start..]);
        let valid = (1u64 << tail_len) - 1;
        let masks = classify(&padded);
        G::scan_block(&padded, tail_start as u32, valid, &masks, state, tape)?;
    }

    G::finish_stage1(input.len() as u32, state)?;
    Ok(Stage1Metrics {
        blocks: full_blocks + usize::from(tail_start < input.len()),
    })
}

#[inline]
fn classify_scalar<G: Grammar>(block: &[u8; 64]) -> ClassifiedBlock {
    let mut out = ClassifiedBlock::default();

    for (lane, &byte) in block.iter().enumerate() {
        let bit = 1u64 << lane;
        let class = G::TABLES.classify(byte);

        for selected in 0..G::CLASS_COUNT {
            if class & G::CLASS_BITS[selected] != 0 {
                out.class[selected] |= bit;
            }
        }
        for selected in 0..G::EXACT_COUNT {
            if byte == G::EXACT_BYTES[selected] {
                out.exact[selected] |= bit;
            }
        }
    }

    out
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "ssse3")]
unsafe fn classify_ssse3<G: Grammar>(block: &[u8; 64]) -> ClassifiedBlock {
    use std::arch::x86_64::*;

    let mut out = ClassifiedBlock::default();
    let lo_table = unsafe { _mm_loadu_si128(G::TABLES.low.as_ptr().cast()) };
    let hi_table = unsafe { _mm_loadu_si128(G::TABLES.high.as_ptr().cast()) };
    let nibble = _mm_set1_epi8(0x0f);
    let zero = _mm_setzero_si128();

    for part in 0..4 {
        let bytes = unsafe { _mm_loadu_si128(block.as_ptr().add(part * 16).cast()) };
        let low = _mm_and_si128(bytes, nibble);
        let high = _mm_and_si128(_mm_srli_epi16(bytes, 4), nibble);
        let classes = _mm_and_si128(
            _mm_shuffle_epi8(lo_table, low),
            _mm_shuffle_epi8(hi_table, high),
        );
        let shift = part * 16;

        for selected in 0..G::CLASS_COUNT {
            let wanted = _mm_set1_epi8(G::CLASS_BITS[selected] as i8);
            let selected_classes = _mm_and_si128(classes, wanted);
            let is_zero = _mm_cmpeq_epi8(selected_classes, zero);
            let class_mask = (!_mm_movemask_epi8(is_zero) as u32) & 0xffff;
            out.class[selected] |= (class_mask as u64) << shift;
        }
        for selected in 0..G::EXACT_COUNT {
            let exact = _mm_set1_epi8(G::EXACT_BYTES[selected] as i8);
            let exact_mask = (_mm_movemask_epi8(_mm_cmpeq_epi8(bytes, exact)) as u32) & 0xffff;
            out.exact[selected] |= (exact_mask as u64) << shift;
        }
    }

    out
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn classify_avx2<G: Grammar>(block: &[u8; 64]) -> ClassifiedBlock {
    use std::arch::x86_64::*;

    let mut out = ClassifiedBlock::default();
    let lo128 = unsafe { _mm_loadu_si128(G::TABLES.low.as_ptr().cast()) };
    let hi128 = unsafe { _mm_loadu_si128(G::TABLES.high.as_ptr().cast()) };
    let lo_table = _mm256_broadcastsi128_si256(lo128);
    let hi_table = _mm256_broadcastsi128_si256(hi128);
    let nibble = _mm256_set1_epi8(0x0f);
    let zero = _mm256_setzero_si256();

    for part in 0..2 {
        let bytes = unsafe { _mm256_loadu_si256(block.as_ptr().add(part * 32).cast()) };
        let low = _mm256_and_si256(bytes, nibble);
        let high = _mm256_and_si256(_mm256_srli_epi16(bytes, 4), nibble);
        let classes = _mm256_and_si256(
            _mm256_shuffle_epi8(lo_table, low),
            _mm256_shuffle_epi8(hi_table, high),
        );
        let shift = part * 32;

        for selected in 0..G::CLASS_COUNT {
            let wanted = _mm256_set1_epi8(G::CLASS_BITS[selected] as i8);
            let selected_classes = _mm256_and_si256(classes, wanted);
            let is_zero = _mm256_cmpeq_epi8(selected_classes, zero);
            let class_mask = !_mm256_movemask_epi8(is_zero) as u32;
            out.class[selected] |= (class_mask as u64) << shift;
        }
        for selected in 0..G::EXACT_COUNT {
            let exact = _mm256_set1_epi8(G::EXACT_BYTES[selected] as i8);
            let exact_mask = _mm256_movemask_epi8(_mm256_cmpeq_epi8(bytes, exact)) as u32;
            out.exact[selected] |= (exact_mask as u64) << shift;
        }
    }

    out
}

#[cfg(test)]
pub(crate) fn classify_for_test<G: Grammar>(
    backend: crate::BackendKind,
    block: &[u8; 64],
) -> ClassifiedBlock {
    match backend {
        crate::BackendKind::Auto | crate::BackendKind::Scalar => classify_scalar::<G>(block),
        #[cfg(target_arch = "x86_64")]
        crate::BackendKind::Ssse3 => unsafe { classify_ssse3::<G>(block) },
        #[cfg(target_arch = "x86_64")]
        crate::BackendKind::Avx2 => unsafe { classify_avx2::<G>(block) },
        #[cfg(not(target_arch = "x86_64"))]
        _ => classify_scalar::<G>(block),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendKind, HtmlGrammar, JsonGrammar};

    #[test]
    fn simd_classification_matches_scalar_for_every_byte() {
        let mut block = [0u8; 64];
        for group in 0..4 {
            for (lane, byte) in block.iter_mut().enumerate() {
                *byte = (group * 64 + lane) as u8;
            }
            compare_available::<HtmlGrammar>(&block);
            compare_available::<JsonGrammar>(&block);
        }
    }

    fn compare_available<G: Grammar>(block: &[u8; 64]) {
        let scalar = classify_for_test::<G>(BackendKind::Scalar, block);
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("ssse3") {
                assert_eq!(classify_for_test::<G>(BackendKind::Ssse3, block), scalar);
            }
            if std::is_x86_feature_detected!("avx2") {
                assert_eq!(classify_for_test::<G>(BackendKind::Avx2, block), scalar);
            }
        }
    }
}
