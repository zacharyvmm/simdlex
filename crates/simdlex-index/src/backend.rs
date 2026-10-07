use std::fmt;

use crate::{ClassifiedBlock, IndexSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Auto,
    Scalar,
    Ssse3,
    Avx2,
    Neon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendError {
    pub requested: BackendKind,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the {:?} backend is unavailable on this CPU",
            self.requested
        )
    }
}

impl std::error::Error for BackendError {}

/// The fastest backend available on this CPU.
pub fn best_backend() -> BackendKind {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            return BackendKind::Avx2;
        }
        if std::is_x86_feature_detected!("ssse3") {
            return BackendKind::Ssse3;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        return BackendKind::Neon;
    }

    #[allow(unreachable_code)]
    BackendKind::Scalar
}

/// Resolve `Auto` and check that `backend` can run on this CPU.
pub fn resolve_backend(backend: BackendKind) -> Result<BackendKind, BackendError> {
    let available = match backend {
        BackendKind::Auto => return Ok(best_backend()),
        BackendKind::Scalar => true,
        #[cfg(target_arch = "x86_64")]
        BackendKind::Ssse3 => std::is_x86_feature_detected!("ssse3"),
        #[cfg(target_arch = "x86_64")]
        BackendKind::Avx2 => std::is_x86_feature_detected!("avx2"),
        // NEON is part of the aarch64 baseline.
        #[cfg(target_arch = "aarch64")]
        BackendKind::Neon => true,
        #[allow(unreachable_patterns)]
        _ => false,
    };
    if available {
        Ok(backend)
    } else {
        Err(BackendError { requested: backend })
    }
}

/// Every backend that can run on this CPU, scalar first.
pub fn available_backends() -> impl Iterator<Item = BackendKind> {
    [
        BackendKind::Scalar,
        BackendKind::Ssse3,
        BackendKind::Avx2,
        BackendKind::Neon,
    ]
    .into_iter()
    .filter(|&backend| resolve_backend(backend).is_ok())
}

/// Classify one block with a specific backend. Intended for tests and
/// debugging; scans use the monomorphized loops in [`crate::Indexer`].
pub fn classify_block<S: IndexSpec>(
    backend: BackendKind,
    block: &[u8; 64],
) -> Result<ClassifiedBlock, BackendError> {
    match resolve_backend(backend)? {
        #[cfg(target_arch = "x86_64")]
        BackendKind::Ssse3 => Ok(unsafe { classify_ssse3::<S>(block) }),
        #[cfg(target_arch = "x86_64")]
        BackendKind::Avx2 => Ok(unsafe { classify_avx2::<S>(block) }),
        #[cfg(target_arch = "aarch64")]
        BackendKind::Neon => Ok(unsafe { classify_neon::<S>(block) }),
        _ => Ok(classify_scalar::<S>(block)),
    }
}

#[inline]
pub(crate) fn classify_scalar<S: IndexSpec>(block: &[u8; 64]) -> ClassifiedBlock {
    let mut out = ClassifiedBlock::default();

    for (lane, &byte) in block.iter().enumerate() {
        let bit = 1u64 << lane;
        let class = S::TABLES.classify(byte);

        for selected in 0..S::CLASS_COUNT {
            if class & S::CLASS_BITS[selected] != 0 {
                out.class[selected] |= bit;
            }
        }
    }

    out
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "ssse3")]
pub(crate) unsafe fn classify_ssse3<S: IndexSpec>(block: &[u8; 64]) -> ClassifiedBlock {
    use std::arch::x86_64::*;

    let mut out = ClassifiedBlock::default();
    let lo_table = unsafe { _mm_loadu_si128(S::TABLES.low.as_ptr().cast()) };
    let hi_table = unsafe { _mm_loadu_si128(S::TABLES.high.as_ptr().cast()) };
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

        for selected in 0..S::CLASS_COUNT {
            let wanted = _mm_set1_epi8(S::CLASS_BITS[selected] as i8);
            let selected_classes = _mm_and_si128(classes, wanted);
            let is_zero = _mm_cmpeq_epi8(selected_classes, zero);
            let class_mask = (!_mm_movemask_epi8(is_zero) as u32) & 0xffff;
            out.class[selected] |= (class_mask as u64) << shift;
        }
    }

    out
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn classify_avx2<S: IndexSpec>(block: &[u8; 64]) -> ClassifiedBlock {
    use std::arch::x86_64::*;

    let mut out = ClassifiedBlock::default();
    let lo128 = unsafe { _mm_loadu_si128(S::TABLES.low.as_ptr().cast()) };
    let hi128 = unsafe { _mm_loadu_si128(S::TABLES.high.as_ptr().cast()) };
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

        for selected in 0..S::CLASS_COUNT {
            let wanted = _mm256_set1_epi8(S::CLASS_BITS[selected] as i8);
            let selected_classes = _mm256_and_si256(classes, wanted);
            let is_zero = _mm256_cmpeq_epi8(selected_classes, zero);
            let class_mask = !_mm256_movemask_epi8(is_zero) as u32;
            out.class[selected] |= (class_mask as u64) << shift;
        }
    }

    out
}

#[cfg(target_arch = "aarch64")]
#[inline]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn classify_neon<S: IndexSpec>(block: &[u8; 64]) -> ClassifiedBlock {
    use std::arch::aarch64::*;

    let mut out = ClassifiedBlock::default();
    let lo_table = unsafe { vld1q_u8(S::TABLES.low.as_ptr()) };
    let hi_table = unsafe { vld1q_u8(S::TABLES.high.as_ptr()) };
    let nibble = vdupq_n_u8(0x0f);

    let mut classes = [vdupq_n_u8(0); 4];
    for (part, classes) in classes.iter_mut().enumerate() {
        let bytes = unsafe { vld1q_u8(block.as_ptr().add(part * 16)) };
        let low = vandq_u8(bytes, nibble);
        let high = vshrq_n_u8::<4>(bytes);
        *classes = vandq_u8(vqtbl1q_u8(lo_table, low), vqtbl1q_u8(hi_table, high));
    }

    for selected in 0..S::CLASS_COUNT {
        let wanted = vdupq_n_u8(S::CLASS_BITS[selected]);
        out.class[selected] = unsafe {
            movemask_64([
                vtstq_u8(classes[0], wanted),
                vtstq_u8(classes[1], wanted),
                vtstq_u8(classes[2], wanted),
                vtstq_u8(classes[3], wanted),
            ])
        };
    }

    out
}

/// Pack four 16-lane all-ones/all-zeros vectors into one 64-bit mask, as
/// simdjson's NEON `to_bitmask` does: weight each lane by its bit, then
/// pairwise-add until each byte holds eight lanes.
#[cfg(target_arch = "aarch64")]
#[inline]
#[target_feature(enable = "neon")]
unsafe fn movemask_64(lanes: [std::arch::aarch64::uint8x16_t; 4]) -> u64 {
    use std::arch::aarch64::*;

    const WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    let weights = unsafe { vld1q_u8(WEIGHTS.as_ptr()) };
    let first = vpaddq_u8(vandq_u8(lanes[0], weights), vandq_u8(lanes[1], weights));
    let second = vpaddq_u8(vandq_u8(lanes[2], weights), vandq_u8(lanes[3], weights));
    let both = vpaddq_u8(first, second);
    let packed = vpaddq_u8(both, both);
    vgetq_lane_u64::<0>(vreinterpretq_u64_u8(packed))
}
