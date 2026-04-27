use crate::spans::{ClassCandidate, ExtractOptions, ExtractResult};
use crate::stage1::StructuralIndex;

pub mod html;

pub trait CandidateParser {
    fn parse(
        source: &[u8],
        index: &StructuralIndex,
        options: &ExtractOptions,
    ) -> ExtractResult<ClassCandidate>;
}
