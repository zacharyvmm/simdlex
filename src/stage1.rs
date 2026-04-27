use crate::classifier::{ByteClassifier, ScalarClassifier};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StructuralIndex {
    pub items: Vec<Structural>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Structural {
    pub offset: u32,
    pub kind: StructuralKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralKind {
    Lt,
    Gt,
    Slash,
    Eq,
    QuoteSingle,
    QuoteDouble,
    Backtick,
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Dot,
    Colon,
    Comma,
    Semicolon,
    Whitespace,
}

pub fn build_structural_index(input: &[u8]) -> StructuralIndex {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("ssse3") {
            return build_structural_index_with::<crate::classifier::Ssse3Classifier>(input);
        }
    }

    build_structural_index_with::<ScalarClassifier>(input)
}

pub fn build_structural_index_with<C: ByteClassifier>(input: &[u8]) -> StructuralIndex {
    let mut items = Vec::with_capacity(input.len() / 8);
    let mut offset = 0;

    while offset + C::LANES <= input.len() {
        let classes = C::classify_block(input, offset);
        push_block_structurals(
            input,
            offset,
            classes.structural | classes.whitespace,
            &mut items,
        );
        offset += C::LANES;
    }

    push_tail_structurals(input, offset, &mut items);

    StructuralIndex { items }
}

#[inline]
fn push_block_structurals(input: &[u8], offset: usize, mut mask: u64, items: &mut Vec<Structural>) {
    while mask != 0 {
        let lane = mask.trailing_zeros() as usize;
        let pos = offset + lane;

        if let Some(kind) = StructuralKind::from_byte(input[pos]) {
            items.push(Structural {
                offset: structural_offset(pos),
                kind,
            });
        }

        mask &= mask - 1;
    }
}

#[inline]
fn push_tail_structurals(input: &[u8], mut offset: usize, items: &mut Vec<Structural>) {
    while offset < input.len() {
        if let Some(kind) = StructuralKind::from_byte(input[offset]) {
            items.push(Structural {
                offset: structural_offset(offset),
                kind,
            });
        }

        offset += 1;
    }
}

#[inline]
fn structural_offset(offset: usize) -> u32 {
    u32::try_from(offset).expect("structural index offsets require inputs shorter than 4 GiB")
}

impl StructuralKind {
    #[inline]
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'<' => Some(Self::Lt),
            b'>' => Some(Self::Gt),
            b'/' => Some(Self::Slash),
            b'=' => Some(Self::Eq),
            b'\'' => Some(Self::QuoteSingle),
            b'"' => Some(Self::QuoteDouble),
            b'`' => Some(Self::Backtick),
            b'(' => Some(Self::OpenParen),
            b')' => Some(Self::CloseParen),
            b'{' => Some(Self::OpenBrace),
            b'}' => Some(Self::CloseBrace),
            b'[' => Some(Self::OpenBracket),
            b']' => Some(Self::CloseBracket),
            b'.' => Some(Self::Dot),
            b':' => Some(Self::Colon),
            b',' => Some(Self::Comma),
            b';' => Some(Self::Semicolon),
            b'\t' | b'\n' | b'\r' | b' ' => Some(Self::Whitespace),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_representative_structural_positions() {
        let input = br#"<div class="px-4" :class="{ active: ok }">`tw`</div>"#;
        let index = build_structural_index_with::<ScalarClassifier>(input);

        assert_eq!(
            index.items,
            vec![
                item(0, StructuralKind::Lt),
                item(4, StructuralKind::Whitespace),
                item(10, StructuralKind::Eq),
                item(11, StructuralKind::QuoteDouble),
                item(16, StructuralKind::QuoteDouble),
                item(17, StructuralKind::Whitespace),
                item(18, StructuralKind::Colon),
                item(24, StructuralKind::Eq),
                item(25, StructuralKind::QuoteDouble),
                item(26, StructuralKind::OpenBrace),
                item(27, StructuralKind::Whitespace),
                item(34, StructuralKind::Colon),
                item(35, StructuralKind::Whitespace),
                item(38, StructuralKind::Whitespace),
                item(39, StructuralKind::CloseBrace),
                item(40, StructuralKind::QuoteDouble),
                item(41, StructuralKind::Gt),
                item(42, StructuralKind::Backtick),
                item(45, StructuralKind::Backtick),
                item(46, StructuralKind::Lt),
                item(47, StructuralKind::Slash),
                item(51, StructuralKind::Gt),
            ]
        );
    }

    #[test]
    fn handles_tails_shorter_than_lane_width() {
        let input = b"abc <x>";
        let index = build_structural_index_with::<ScalarClassifier>(input);

        assert_eq!(
            index.items,
            vec![
                item(3, StructuralKind::Whitespace),
                item(4, StructuralKind::Lt),
                item(6, StructuralKind::Gt),
            ]
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn raw_simd_index_matches_scalar() {
        let input = br#"<div class="a b" data-x='1'>fn(clsx("p-4", cond && `m-2`));</div>"#;

        assert_eq!(
            build_structural_index_with::<crate::classifier::RawSimdClassifier>(input),
            build_structural_index_with::<ScalarClassifier>(input)
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn ssse3_index_matches_scalar_when_available() {
        if !std::is_x86_feature_detected!("ssse3") {
            return;
        }

        let input = br#"<div class="a b" data-x='1'>fn(clsx("p-4", cond && `m-2`));</div>"#;

        assert_eq!(
            build_structural_index_with::<crate::classifier::Ssse3Classifier>(input),
            build_structural_index_with::<ScalarClassifier>(input)
        );
    }

    fn item(offset: u32, kind: StructuralKind) -> Structural {
        Structural { offset, kind }
    }
}
