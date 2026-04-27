#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteClasses {
    pub structural: u64,
    pub whitespace: u64,
    pub quote_double: u64,
    pub quote_single: u64,
    pub lt: u64,
    pub gt: u64,
    pub eq: u64,
}

pub trait ByteClassifier {
    const LANES: usize;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses;

    #[inline]
    fn lt_block_mask(input: &[u8], offset: usize) -> u64 {
        Self::classify_block(input, offset).lt
    }

    #[inline]
    fn gt_block_mask(input: &[u8], offset: usize) -> u64 {
        Self::classify_block(input, offset).gt
    }

    #[inline]
    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64 {
        let classes = Self::classify_block(input, offset);
        classes.quote_double | classes.quote_single | classes.gt
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScalarClassifier;

impl ByteClassifier for ScalarClassifier {
    const LANES: usize = 16;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses {
        debug_assert!(offset + Self::LANES <= input.len());

        let mut out = ByteClasses {
            structural: 0,
            whitespace: 0,
            quote_double: 0,
            quote_single: 0,
            lt: 0,
            gt: 0,
            eq: 0,
        };

        for lane in 0..Self::LANES {
            let bit = 1u64 << lane;

            match input[offset + lane] {
                b'"' => {
                    out.structural |= bit;
                    out.quote_double |= bit;
                }
                b'\'' => {
                    out.structural |= bit;
                    out.quote_single |= bit;
                }
                b'<' => {
                    out.structural |= bit;
                    out.lt |= bit;
                }
                b'>' => {
                    out.structural |= bit;
                    out.gt |= bit;
                }
                b'=' => {
                    out.structural |= bit;
                    out.eq |= bit;
                }
                b'\t' | b'\n' | b'\r' | b' ' => {
                    out.whitespace |= bit;
                }
                _ => {}
            }
        }

        out
    }

    #[inline]
    fn lt_block_mask(input: &[u8], offset: usize) -> u64 {
        scalar_byte_mask(input, offset, Self::LANES, b'<')
    }

    #[inline]
    fn gt_block_mask(input: &[u8], offset: usize) -> u64 {
        scalar_byte_mask(input, offset, Self::LANES, b'>')
    }

    #[inline]
    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64 {
        let mut mask = 0;

        for lane in 0..Self::LANES {
            if matches!(input[offset + lane], b'"' | b'\'' | b'>') {
                mask |= 1u64 << lane;
            }
        }

        mask
    }
}

#[inline]
fn scalar_byte_mask(input: &[u8], offset: usize, lanes: usize, byte: u8) -> u64 {
    let mut mask = 0;

    for lane in 0..lanes {
        if input[offset + lane] == byte {
            mask |= 1u64 << lane;
        }
    }

    mask
}

#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, Copy)]
pub struct RawSimdClassifier;

#[cfg(target_arch = "x86_64")]
impl ByteClassifier for RawSimdClassifier {
    const LANES: usize = 16;

    fn classify_block(input: &[u8], offset: usize) -> ByteClasses {
        debug_assert!(offset + Self::LANES <= input.len());

        if std::is_x86_feature_detected!("ssse3") {
            unsafe { classify_block_ssse3(input, offset) }
        } else {
            ScalarClassifier::classify_block(input, offset)
        }
    }

    #[inline]
    fn lt_block_mask(input: &[u8], offset: usize) -> u64 {
        if std::is_x86_feature_detected!("ssse3") {
            unsafe { byte_eq_block_mask_ssse3(input, offset, b'<') }
        } else {
            ScalarClassifier::lt_block_mask(input, offset)
        }
    }

    #[inline]
    fn gt_block_mask(input: &[u8], offset: usize) -> u64 {
        if std::is_x86_feature_detected!("ssse3") {
            unsafe { byte_eq_block_mask_ssse3(input, offset, b'>') }
        } else {
            ScalarClassifier::gt_block_mask(input, offset)
        }
    }

    #[inline]
    fn tag_end_block_mask(input: &[u8], offset: usize) -> u64 {
        if std::is_x86_feature_detected!("ssse3") {
            unsafe { tag_end_block_mask_ssse3(input, offset) }
        } else {
            ScalarClassifier::tag_end_block_mask(input, offset)
        }
    }
}

#[cfg(target_arch = "x86_64")]
const TLO: [u8; 16] = [8, 0, 1, 0, 0, 0, 0, 1, 0, 4, 4, 0, 2, 6, 2, 0];

#[cfg(target_arch = "x86_64")]
const THI: [u8; 16] = [4, 0, 9, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[cfg(target_arch = "x86_64")]
const STRUCTURAL_BITS: u8 = 1 | 2;

#[cfg(target_arch = "x86_64")]
const WHITESPACE_BITS: u8 = 4 | 8;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn classify_block_ssse3(input: &[u8], offset: usize) -> ByteClasses {
    use std::arch::x86_64::*;

    let bytes = unsafe { _mm_loadu_si128(input.as_ptr().add(offset).cast::<__m128i>()) };

    let lo_mask = _mm_set1_epi8(0x0f);
    let lo = _mm_and_si128(bytes, lo_mask);
    let hi = _mm_and_si128(_mm_srli_epi16(bytes, 4), lo_mask);

    let tlo = unsafe { _mm_loadu_si128(TLO.as_ptr().cast::<__m128i>()) };
    let thi = unsafe { _mm_loadu_si128(THI.as_ptr().cast::<__m128i>()) };

    let lo_class = _mm_shuffle_epi8(tlo, lo);
    let hi_class = _mm_shuffle_epi8(thi, hi);
    let class = _mm_and_si128(lo_class, hi_class);

    let structural = nonzero_bitmask(_mm_and_si128(class, _mm_set1_epi8(STRUCTURAL_BITS as i8)));
    let whitespace = nonzero_bitmask(_mm_and_si128(class, _mm_set1_epi8(WHITESPACE_BITS as i8)));

    ByteClasses {
        structural,
        whitespace,
        quote_double: eq_bitmask(bytes, b'"'),
        quote_single: eq_bitmask(bytes, b'\''),
        lt: eq_bitmask(bytes, b'<'),
        gt: eq_bitmask(bytes, b'>'),
        eq: eq_bitmask(bytes, b'='),
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn byte_eq_block_mask_ssse3(input: &[u8], offset: usize, byte: u8) -> u64 {
    use std::arch::x86_64::*;

    let bytes = unsafe { _mm_loadu_si128(input.as_ptr().add(offset).cast::<__m128i>()) };
    eq_bitmask(bytes, byte)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn tag_end_block_mask_ssse3(input: &[u8], offset: usize) -> u64 {
    use std::arch::x86_64::*;

    let bytes = unsafe { _mm_loadu_si128(input.as_ptr().add(offset).cast::<__m128i>()) };
    eq_bitmask(bytes, b'"') | eq_bitmask(bytes, b'\'') | eq_bitmask(bytes, b'>')
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
fn eq_bitmask(bytes: std::arch::x86_64::__m128i, byte: u8) -> u64 {
    use std::arch::x86_64::*;

    let wanted = _mm_set1_epi8(byte as i8);
    let cmp = _mm_cmpeq_epi8(bytes, wanted);
    (_mm_movemask_epi8(cmp) as u32 & 0xffff) as u64
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
fn nonzero_bitmask(bytes: std::arch::x86_64::__m128i) -> u64 {
    use std::arch::x86_64::*;

    let zero = _mm_setzero_si128();
    let cmp_zero = _mm_cmpeq_epi8(bytes, zero);
    ((!_mm_movemask_epi8(cmp_zero) as u32) & 0xffff) as u64
}

#[cfg(target_arch = "x86_64")]
pub type DefaultClassifier = RawSimdClassifier;

#[cfg(not(target_arch = "x86_64"))]
pub type DefaultClassifier = ScalarClassifier;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_classifier_finds_expected_masks() {
        assert_classifier::<ScalarClassifier>();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn raw_simd_classifier_matches_scalar() {
        assert_classifier::<RawSimdClassifier>();

        let input = b"a<'\"=> \t\n\rxyz---";
        let scalar = ScalarClassifier::classify_block(input, 0);
        let simd = RawSimdClassifier::classify_block(input, 0);

        assert_eq!(simd, scalar);
    }

    fn assert_classifier<C: ByteClassifier>() {
        let input = b"a<'\"=> \t\n\rxyz---";
        let classes = C::classify_block(input, 0);

        assert_eq!(classes.lt, 1 << 1);
        assert_eq!(classes.quote_single, 1 << 2);
        assert_eq!(classes.quote_double, 1 << 3);
        assert_eq!(classes.eq, 1 << 4);
        assert_eq!(classes.gt, 1 << 5);
        assert_eq!(
            classes.whitespace,
            (1 << 6) | (1 << 7) | (1 << 8) | (1 << 9)
        );
        assert_eq!(
            classes.structural,
            (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5)
        );
    }
}
