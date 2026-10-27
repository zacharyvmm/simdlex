//! Compile-time generation of nibble lookup tables.
//!
//! A byte is classified by intersecting `low[byte & 15]` and
//! `high[byte >> 4]`. This crate turns named byte sets into those two 16-byte
//! tables plus one bit selector per set, so that for every byte value `b` and
//! every class `c`:
//!
//! ```text
//! low[b & 15] & high[b >> 4] & class_bits[c] != 0  <=>  b ∈ classes[c]
//! ```
//!
//! Generation is a `const fn`: a grammar writes its byte sets once, and an
//! unsatisfiable request (more than eight table bits required) fails the
//! build instead of producing a wrong classifier. Every generated table is
//! exhaustively verified against all 256 byte values before it is returned.
//!
//! This module knows nothing about how tables are executed. The
//! [`Indexer`](crate::Indexer) consumes the emitted `low`/`high` arrays.
//!
//! ```
//! use simdlex::tablegen::{ByteSet, generate, unwrap};
//!
//! const TABLES: simdlex::tablegen::Tables<3> = unwrap(generate(&[
//!     ByteSet::from_bytes(b"<>"),
//!     ByteSet::from_bytes(b"="),
//!     ByteSet::from_bytes(b"'\""),
//! ]));
//!
//! assert_ne!(TABLES.classify(b'<') & TABLES.class_bits[0], 0);
//! assert_eq!(TABLES.classify(b'a'), 0);
//! ```
//!
//! Requests that cannot fit fail to compile:
//!
//! ```compile_fail
//! use simdlex::tablegen::{ByteSet, Tables, generate, unwrap};
//!
//! // Nine bytes that share no nibble with each other need nine bits.
//! const DIAGONAL: Tables<1> = unwrap(generate(&[ByteSet::from_bytes(&[
//!     0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
//! ])]));
//! # let _ = DIAGONAL;
//! ```
/// Classes beyond this count cannot be distinguished with an 8-bit table.
pub const MAX_CLASSES: usize = 8;

/// A set of byte values, usable in `const` contexts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ByteSet {
    words: [u64; 4],
}

impl ByteSet {
    pub const EMPTY: Self = Self { words: [0; 4] };

    pub const fn from_bytes(bytes: &[u8]) -> Self {
        let mut set = Self::EMPTY;
        let mut index = 0;
        while index < bytes.len() {
            set = set.with(bytes[index]);
            index += 1;
        }
        set
    }

    /// The inclusive range `first..=last`.
    pub const fn range(first: u8, last: u8) -> Self {
        let mut set = Self::EMPTY;
        let mut byte = first as u16;
        while byte <= last as u16 {
            set = set.with(byte as u8);
            byte += 1;
        }
        set
    }

    pub const fn with(mut self, byte: u8) -> Self {
        self.words[(byte >> 6) as usize] |= 1 << (byte & 63);
        self
    }

    pub const fn union(self, other: Self) -> Self {
        Self {
            words: [
                self.words[0] | other.words[0],
                self.words[1] | other.words[1],
                self.words[2] | other.words[2],
                self.words[3] | other.words[3],
            ],
        }
    }

    pub const fn contains(self, byte: u8) -> bool {
        self.words[(byte >> 6) as usize] & (1 << (byte & 63)) != 0
    }

    pub const fn len(self) -> u32 {
        self.words[0].count_ones()
            + self.words[1].count_ones()
            + self.words[2].count_ones()
            + self.words[3].count_ones()
    }

    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Low nibbles `l` such that `(high << 4) | l` is a member.
    const fn lows_of_high(self, high: u8) -> u16 {
        let mut lows = 0u16;
        let mut low = 0u8;
        while low < 16 {
            if self.contains((high << 4) | low) {
                lows |= 1 << low;
            }
            low += 1;
        }
        lows
    }

    /// High nibbles `h` such that `(h << 4) | low` is a member.
    const fn highs_of_low(self, low: u8) -> u16 {
        let mut highs = 0u16;
        let mut high = 0u8;
        while high < 16 {
            if self.contains((high << 4) | low) {
                highs |= 1 << high;
            }
            high += 1;
        }
        highs
    }
}

/// Generated tables for `N` classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tables<const N: usize> {
    pub low: [u8; 16],
    pub high: [u8; 16],
    /// Bits selecting each class from [`Self::classify`].
    pub class_bits: [u8; N],
    /// Number of the eight table bits that were needed.
    pub bits_used: u32,
}

impl<const N: usize> Tables<N> {
    #[inline]
    pub const fn classify(&self, byte: u8) -> u8 {
        self.low[(byte & 0x0f) as usize] & self.high[(byte >> 4) as usize]
    }

    /// `class_bits` zero-extended to eight entries, the shape backends use.
    pub const fn padded_class_bits(&self) -> [u8; MAX_CLASSES] {
        let mut out = [0u8; MAX_CLASSES];
        let mut index = 0;
        while index < N {
            out[index] = self.class_bits[index];
            index += 1;
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableGenError {
    /// More than [`MAX_CLASSES`] classes were requested.
    TooManyClasses { classes: usize },
    /// A class has no members, so it cannot be assigned a bit.
    EmptyClass { class: usize },
    /// The best decomposition found needs more than eight table bits.
    TooManyBits { required: u32 },
    /// Exhaustive verification failed. This indicates a generator bug.
    Verification { class: usize, byte: u8 },
}

impl TableGenError {
    pub const fn message(&self) -> &'static str {
        match self {
            Self::TooManyClasses { .. } => "nibble tables support at most eight classes",
            Self::EmptyClass { .. } => "a nibble table class has no member bytes",
            Self::TooManyBits { .. } => {
                "the requested byte classes need more than eight nibble table bits"
            }
            Self::Verification { .. } => "generated nibble tables failed exhaustive verification",
        }
    }
}

/// Unwrap a generation result in a `const` item, turning errors into
/// compile-time diagnostics.
pub const fn unwrap<const N: usize>(result: Result<Tables<N>, TableGenError>) -> Tables<N> {
    match result {
        Ok(tables) => tables,
        Err(error) => panic!("{}", error.message()),
    }
}

/// A `low × high` nibble rectangle: one table bit can recognise exactly the
/// bytes in one rectangle.
#[derive(Clone, Copy)]
struct Rect {
    low: u16,
    high: u16,
    /// Classes that select this rectangle's bit.
    owners: u8,
}

const EMPTY_RECT: Rect = Rect {
    low: 0,
    high: 0,
    owners: 0,
};

/// A partition of one class into disjoint rectangles.
#[derive(Clone, Copy)]
struct Partition {
    rects: [Rect; 16],
    len: usize,
}

/// Group high nibbles that share an identical set of low nibbles.
const fn partition_by_high(set: ByteSet) -> Partition {
    let mut partition = Partition {
        rects: [EMPTY_RECT; 16],
        len: 0,
    };
    let mut high = 0u8;
    while high < 16 {
        let lows = set.lows_of_high(high);
        if lows != 0 {
            partition = add_to_partition(partition, lows, 1 << high, true);
        }
        high += 1;
    }
    partition
}

/// Group low nibbles that share an identical set of high nibbles.
const fn partition_by_low(set: ByteSet) -> Partition {
    let mut partition = Partition {
        rects: [EMPTY_RECT; 16],
        len: 0,
    };
    let mut low = 0u8;
    while low < 16 {
        let highs = set.highs_of_low(low);
        if highs != 0 {
            partition = add_to_partition(partition, 1 << low, highs, false);
        }
        low += 1;
    }
    partition
}

const fn add_to_partition(
    mut partition: Partition,
    low: u16,
    high: u16,
    by_high: bool,
) -> Partition {
    let mut index = 0;
    while index < partition.len {
        let rect = &mut partition.rects[index];
        if by_high && rect.low == low {
            rect.high |= high;
            return partition;
        }
        if !by_high && rect.high == high {
            rect.low |= low;
            return partition;
        }
        index += 1;
    }
    partition.rects[partition.len] = Rect {
        low,
        high,
        owners: 0,
    };
    partition.len += 1;
    partition
}

/// Rectangles assigned to table bits. Classes whose partitions contain an
/// identical rectangle share that rectangle's bit.
#[derive(Clone, Copy)]
struct Assignment {
    rects: [Rect; MAX_CLASSES],
    len: usize,
}

/// Merge the chosen partition of every class. `use_low` selects, per class,
/// the low-nibble partition instead of the high-nibble one. Returns `None` as
/// soon as more than eight bits would be needed.
const fn assign(
    by_high: &[Partition; MAX_CLASSES],
    by_low: &[Partition; MAX_CLASSES],
    classes: usize,
    use_low: u32,
) -> Option<Assignment> {
    let mut assignment = Assignment {
        rects: [EMPTY_RECT; MAX_CLASSES],
        len: 0,
    };
    let mut class = 0;
    while class < classes {
        let partition = if use_low & (1 << class) != 0 {
            &by_low[class]
        } else {
            &by_high[class]
        };
        let mut index = 0;
        'rects: while index < partition.len {
            let candidate = partition.rects[index];
            index += 1;
            let mut existing = 0;
            while existing < assignment.len {
                let rect = &mut assignment.rects[existing];
                if rect.low == candidate.low && rect.high == candidate.high {
                    rect.owners |= 1 << class;
                    continue 'rects;
                }
                existing += 1;
            }
            if assignment.len == MAX_CLASSES {
                return None;
            }
            assignment.rects[assignment.len] = Rect {
                owners: 1 << class,
                ..candidate
            };
            assignment.len += 1;
        }
        class += 1;
    }
    Some(assignment)
}

const fn required_bits(
    by_high: &[Partition; MAX_CLASSES],
    by_low: &[Partition; MAX_CLASSES],
    classes: usize,
) -> u32 {
    let mut total = 0;
    let mut class = 0;
    while class < classes {
        let high = by_high[class].len;
        let low = by_low[class].len;
        total += if high < low { high } else { low };
        class += 1;
    }
    total as u32
}

/// Generate tables that recognise every class exactly.
///
/// Each class is partitioned into nibble rectangles, grouping either by high
/// nibble or by low nibble. Each distinct rectangle receives one table bit,
/// and identical rectangles from different classes share their bit. The
/// cheapest per-class choice is tried first; if it does not fit in eight bits,
/// every combination of choices is searched so that cross-class sharing can
/// close the gap. This is a heuristic (minimum biclique cover is NP-hard), so
/// a hand-written table may occasionally fit where generation reports
/// [`TableGenError::TooManyBits`].
pub const fn generate<const N: usize>(classes: &[ByteSet; N]) -> Result<Tables<N>, TableGenError> {
    if N > MAX_CLASSES {
        return Err(TableGenError::TooManyClasses { classes: N });
    }

    let mut by_high = [Partition {
        rects: [EMPTY_RECT; 16],
        len: 0,
    }; MAX_CLASSES];
    let mut by_low = by_high;
    let mut greedy = 0u32;
    let mut class = 0;
    while class < N {
        if classes[class].is_empty() {
            return Err(TableGenError::EmptyClass { class });
        }
        by_high[class] = partition_by_high(classes[class]);
        by_low[class] = partition_by_low(classes[class]);
        if by_low[class].len < by_high[class].len {
            greedy |= 1 << class;
        }
        class += 1;
    }

    let mut best = assign(&by_high, &by_low, N, greedy);
    if best.is_none() {
        let mut choice = 0u32;
        while choice < (1 << N) {
            if let Some(candidate) = assign(&by_high, &by_low, N, choice) {
                let better = match &best {
                    Some(current) => candidate.len < current.len,
                    None => true,
                };
                if better {
                    best = Some(candidate);
                }
            }
            choice += 1;
        }
    }

    let Some(assignment) = best else {
        return Err(TableGenError::TooManyBits {
            required: required_bits(&by_high, &by_low, N),
        });
    };

    let mut tables = Tables {
        low: [0; 16],
        high: [0; 16],
        class_bits: [0; N],
        bits_used: assignment.len as u32,
    };
    let mut bit = 0;
    while bit < assignment.len {
        let rect = assignment.rects[bit];
        let mask = 1u8 << bit;
        let mut nibble = 0;
        while nibble < 16 {
            if rect.low & (1 << nibble) != 0 {
                tables.low[nibble] |= mask;
            }
            if rect.high & (1 << nibble) != 0 {
                tables.high[nibble] |= mask;
            }
            nibble += 1;
        }
        let mut class = 0;
        while class < N {
            if rect.owners & (1 << class) != 0 {
                tables.class_bits[class] |= mask;
            }
            class += 1;
        }
        bit += 1;
    }

    match verify(&tables, classes) {
        Ok(()) => Ok(tables),
        Err(error) => Err(error),
    }
}

/// Check every byte value against every class. Hand-written tables can use
/// this to obtain the same guarantee as generated ones.
pub const fn verify<const N: usize>(
    tables: &Tables<N>,
    classes: &[ByteSet; N],
) -> Result<(), TableGenError> {
    let mut byte = 0u16;
    while byte < 256 {
        let classified = tables.classify(byte as u8);
        let mut class = 0;
        while class < N {
            let selected = classified & tables.class_bits[class] != 0;
            if selected != classes[class].contains(byte as u8) {
                return Err(TableGenError::Verification {
                    class,
                    byte: byte as u8,
                });
            }
            class += 1;
        }
        byte += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: [ByteSet; 5] = [
        ByteSet::from_bytes(b"<"),
        ByteSet::from_bytes(b">"),
        ByteSet::from_bytes(b"="),
        ByteSet::from_bytes(b"'"),
        ByteSet::from_bytes(b"\""),
    ];

    // The JSON classes need cross-class bit sharing to fit: whitespace and
    // controls overlap on '\t', '\n', and '\r'.
    const JSON: [ByteSet; 5] = [
        ByteSet::from_bytes(b"[]{},:"),
        ByteSet::from_bytes(b"\t\n\r "),
        ByteSet::from_bytes(b"\""),
        ByteSet::from_bytes(b"\\"),
        ByteSet::range(0x00, 0x1f),
    ];

    const COMPILE_TIME_HTML: Tables<5> = unwrap(generate(&HTML));
    const COMPILE_TIME_JSON: Tables<5> = unwrap(generate(&JSON));

    #[test]
    fn generates_html_delimiters_one_bit_each() {
        assert_eq!(COMPILE_TIME_HTML.bits_used, 5);
        assert_eq!(verify(&COMPILE_TIME_HTML, &HTML), Ok(()));
    }

    #[test]
    fn generates_overlapping_json_classes_in_eight_bits() {
        assert_eq!(COMPILE_TIME_JSON.bits_used, 8);
        assert_eq!(verify(&COMPILE_TIME_JSON, &JSON), Ok(()));
    }

    #[test]
    fn identical_classes_share_a_bit() {
        let tables = generate(&[ByteSet::from_bytes(b"ab"), ByteSet::from_bytes(b"ab")]).unwrap();
        assert_eq!(tables.bits_used, 1);
        assert_eq!(tables.class_bits[0], tables.class_bits[1]);
    }

    #[test]
    fn rectangular_sets_need_one_bit() {
        // Every byte with high nibble 0x3 or 0x5 and low nibble 0x1..=0x4.
        let mut set = ByteSet::EMPTY;
        for high in [0x30u8, 0x50] {
            set = set.union(ByteSet::range(high | 1, high | 4));
        }
        let tables = generate(&[set]).unwrap();
        assert_eq!(tables.bits_used, 1);
    }

    #[test]
    fn rejects_unsatisfiable_and_malformed_requests() {
        // Nine disjoint single-byte classes cannot share bits.
        let singles: [ByteSet; 9] =
            core::array::from_fn(|i| ByteSet::from_bytes(&[b'a' + i as u8]));
        assert_eq!(
            generate(&singles),
            Err(TableGenError::TooManyClasses { classes: 9 })
        );

        // A diagonal set has no two members sharing a nibble.
        let diagonal: [u8; 9] = core::array::from_fn(|i| (i as u8) * 0x11);
        assert_eq!(
            generate(&[ByteSet::from_bytes(&diagonal)]),
            Err(TableGenError::TooManyBits { required: 9 })
        );

        assert_eq!(
            generate(&[ByteSet::EMPTY]),
            Err(TableGenError::EmptyClass { class: 0 })
        );
    }

    #[test]
    fn verify_rejects_false_positives() {
        let mut tables = generate(&[ByteSet::from_bytes(b"<")]).unwrap();
        tables.low[0] |= tables.class_bits[0];
        assert_eq!(
            verify(&tables, &[ByteSet::from_bytes(b"<")]),
            Err(TableGenError::Verification {
                class: 0,
                byte: 0x30
            })
        );
    }
}
