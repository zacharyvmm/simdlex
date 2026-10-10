//! Compile-time grammar contracts used by [`crate::Parser`].

mod html;
mod html_stream;
mod yaml;

#[cfg(any(feature = "json-test-grammar", test))]
mod json;

pub use html::{
    HtmlConfig, HtmlEmit, HtmlError, HtmlErrorKind, HtmlEvent, HtmlGrammar, HtmlStage1State,
};
pub use html_stream::{
    HtmlAttribute, HtmlAttributeValue, HtmlAttributes, HtmlReader, HtmlStartElement,
    HtmlStreamEvent, HtmlStreamGrammar,
};

pub use yaml::{
    YamlBlockScalar, YamlChomping, YamlCollectionStyle, YamlConfig, YamlError, YamlErrorKind,
    YamlEvent, YamlGrammar, YamlProperties, YamlScalar, YamlScalarStyle, YamlStage2State,
    decode_yaml_scalar,
};

#[cfg(any(feature = "json-test-grammar", test))]
pub use json::{
    JsonConfig, JsonError, JsonErrorKind, JsonEvent, JsonGrammar, RawJsonString, decode_json_string,
};

use simdlex_index::{IndexSpec, IndexView};

use crate::{CapacityError, Span};

/// A grammar: a stage-1 [`IndexSpec`] plus a stage-2 parser over its index.
///
/// Stage 1 (tables, lexical state, and the per-block hook) runs inside the
/// grammar-neutral `simdlex-index` backends. Stage 2 receives the index in
/// whichever [`simdlex_index::IndexMode`] the parser was configured with.
pub trait Grammar: IndexSpec {
    type Config;
    type Event<'a>: Copy
    where
        Self: 'a;
    type Stage2State: Default;

    fn parse<'a, F>(
        input: &'a [u8],
        index: IndexView<'_>,
        config: &Self::Config,
        state: &mut Self::Stage2State,
        sink: F,
    ) -> Result<(), Self::Error>
    where
        F: FnMut(Self::Event<'a>),
        Self: 'a;

    fn input_too_large_error() -> Self::Error;
    fn capacity_error(offset: u32) -> Self::Error;

    fn reserve_stage2(
        config: &Self::Config,
        state: &mut Self::Stage2State,
        fallible: bool,
    ) -> Result<(), CapacityError>;
}

#[inline]
pub(crate) fn span(start: usize, end: usize) -> Span {
    Span {
        start: start as u32,
        end: end as u32,
    }
}

#[cfg(test)]
mod tests {
    use simdlex_index::{BackendKind, IndexSpec, classify_block};

    use super::{HtmlGrammar, JsonGrammar};

    #[test]
    fn simd_classification_matches_scalar_for_every_byte() {
        for group in 0..4 {
            let block: [u8; 64] = std::array::from_fn(|lane| (group * 64 + lane) as u8);
            compare_available::<HtmlGrammar>(&block);
            compare_available::<JsonGrammar>(&block);
        }
    }

    fn compare_available<G: IndexSpec>(block: &[u8; 64]) {
        let scalar = classify_block::<G>(BackendKind::Scalar, block).unwrap();
        for backend in [BackendKind::Ssse3, BackendKind::Avx2] {
            if let Ok(classified) = classify_block::<G>(backend, block) {
                assert_eq!(classified, scalar);
            }
        }
    }
}
