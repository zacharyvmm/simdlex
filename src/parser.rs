use std::fmt;

use simdlex_index::{BackendError, BackendKind, IndexMode, IndexView, Indexer, StructuralIndex};

use crate::grammar::Grammar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityError {
    InputTooLarge,
    AllocationFailed { requested: usize },
}

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge => f.write_str("inputs must be shorter than u32::MAX bytes"),
            Self::AllocationFailed { requested } => {
                write!(f, "could not reserve parser capacity for {requested} bytes")
            }
        }
    }
}

impl std::error::Error for CapacityError {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stage1Stats {
    pub input_bytes: usize,
    pub mode: IndexMode,
    pub structural_count: usize,
    /// Logical size of the tape or block masks.
    pub index_bytes: usize,
    pub index_capacity_bytes: usize,
    pub blocks: usize,
    pub density: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParseStats {
    pub stage1: Stage1Stats,
    pub events: usize,
}

pub struct Parser<G: Grammar> {
    config: G::Config,
    index: StructuralIndex,
    stage1_state: G::State,
    stage2_state: G::Stage2State,
    indexer: Indexer<G>,
    mode: IndexMode,
    max_input: Option<usize>,
}

impl<G: Grammar> Parser<G> {
    pub fn new(config: G::Config) -> Self {
        Self::with_backend(config, BackendKind::Auto)
            .expect("the automatically selected parser backend must be available")
    }

    pub fn with_capacity(config: G::Config, max_input: usize) -> Result<Self, CapacityError> {
        if max_input >= u32::MAX as usize {
            return Err(CapacityError::InputTooLarge);
        }

        let mut parser = Self::build(config, Indexer::new(), true)?;
        parser.max_input = Some(max_input);
        parser.reserve_index()?;
        Ok(parser)
    }

    pub fn with_backend(config: G::Config, backend: BackendKind) -> Result<Self, BackendError> {
        let indexer = Indexer::with_backend(backend)?;
        Ok(
            Self::build(config, indexer, false)
                .expect("infallible parser state reservation failed"),
        )
    }

    /// Record stage 1 as a tape or as block masks instead of the grammar's
    /// default [`IndexMode`]. Bounded parsers re-reserve for the new mode.
    pub fn with_index_mode(mut self, mode: IndexMode) -> Result<Self, CapacityError> {
        self.mode = mode;
        if self.max_input.is_some() {
            self.reserve_index()?;
        }
        Ok(self)
    }

    fn build(
        config: G::Config,
        indexer: Indexer<G>,
        fallible: bool,
    ) -> Result<Self, CapacityError> {
        let mut stage2_state = G::Stage2State::default();
        G::reserve_stage2(&config, &mut stage2_state, fallible)?;
        Ok(Self {
            config,
            index: StructuralIndex::new(),
            stage1_state: G::State::default(),
            stage2_state,
            indexer,
            mode: G::MODE,
            max_input: None,
        })
    }

    fn reserve_index(&mut self) -> Result<(), CapacityError> {
        let max_input = self.max_input.unwrap_or(0);
        self.index
            .try_reserve(self.mode, Indexer::<G>::layout(), max_input)
            .map_err(|_| CapacityError::AllocationFailed {
                requested: max_input,
            })
    }

    #[inline]
    pub fn backend(&self) -> BackendKind {
        self.indexer.backend()
    }

    #[inline]
    pub fn index_mode(&self) -> IndexMode {
        self.mode
    }

    #[inline]
    pub fn config(&self) -> &G::Config {
        &self.config
    }

    pub fn stage1(&mut self, input: &[u8]) -> Result<Stage1Stats, G::Error> {
        if input.len() >= u32::MAX as usize {
            return Err(G::input_too_large_error());
        }
        if let Some(max_input) = self.max_input
            && input.len() > max_input
        {
            return Err(G::capacity_error(max_input as u32));
        }

        let metrics =
            self.indexer
                .index(input, self.mode, &mut self.stage1_state, &mut self.index)?;
        let view = self.index.view();
        let structural_count = view.structural_count();
        Ok(Stage1Stats {
            input_bytes: input.len(),
            mode: self.mode,
            structural_count,
            index_bytes: view.bytes(),
            index_capacity_bytes: self.index.capacity_bytes(),
            blocks: metrics.blocks,
            density: if input.is_empty() {
                0.0
            } else {
                structural_count as f64 / input.len() as f64
            },
        })
    }

    /// The index built by the most recent [`Self::stage1`].
    #[inline]
    pub fn index(&self) -> IndexView<'_> {
        self.index.view()
    }

    #[inline]
    pub fn index_capacity_bytes(&self) -> usize {
        self.index.capacity_bytes()
    }

    pub fn parse_with<'a>(
        &mut self,
        input: &'a [u8],
        sink: impl FnMut(G::Event<'a>),
    ) -> Result<ParseStats, G::Error>
    where
        G: 'a,
    {
        let stage1 = self.stage1(input)?;
        let events = self.stage2_with(input, sink)?;
        Ok(ParseStats { stage1, events })
    }

    /// Run stage 2 over the index produced by the most recent [`Self::stage1`].
    ///
    /// This is primarily useful for isolating stage-2 cost in benchmarks. The
    /// caller must pass the same input bytes used to build the current index.
    pub fn stage2_with<'a>(
        &mut self,
        input: &'a [u8],
        mut sink: impl FnMut(G::Event<'a>),
    ) -> Result<usize, G::Error>
    where
        G: 'a,
    {
        let mut events = 0usize;
        G::parse(
            input,
            self.index.view(),
            &self.config,
            &mut self.stage2_state,
            |event| {
                events += 1;
                sink(event);
            },
        )?;
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HtmlConfig, HtmlEvent, HtmlGrammar, JsonConfig, JsonError, JsonEvent, JsonGrammar,
    };

    fn available_backends() -> Vec<BackendKind> {
        [BackendKind::Scalar, BackendKind::Ssse3, BackendKind::Avx2]
            .into_iter()
            .filter(|&backend| simdlex_index::resolve_backend(backend).is_ok())
            .collect()
    }

    fn positions<G: Grammar>(parser: &Parser<G>) -> Vec<u32> {
        parser.index().positions().collect()
    }

    #[test]
    fn json_offsets_errors_and_events_match_every_backend() {
        let documents: &[&[u8]] = &[
            br#"{"a":"escaped\\\"quote","b":[true,false,null,-1.2e3]}"#,
            r#"["cross boundary padding",{"unicode":"é雪"}]"#.as_bytes(),
            br#"{"bad":"\uD800"}"#,
            b"[1,]",
            b"\"\xf0\x28\x8c\x28\"",
        ];

        for &document in documents {
            let mut expected_parser =
                Parser::<JsonGrammar>::with_backend(JsonConfig::default(), BackendKind::Scalar)
                    .unwrap();
            let mut expected_events = Vec::new();
            let expected_result =
                expected_parser.parse_with(document, |event| expected_events.push(event));
            let expected_offsets = positions(&expected_parser);
            let expected_error: Option<JsonError> = expected_result.err();

            for backend in available_backends() {
                let mut parser =
                    Parser::<JsonGrammar>::with_backend(JsonConfig::default(), backend).unwrap();
                let mut events: Vec<JsonEvent<'_>> = Vec::new();
                let result = parser.parse_with(document, |event| events.push(event));
                assert_eq!(positions(&parser), expected_offsets);
                assert_eq!(result.err(), expected_error);
                assert_eq!(events, expected_events);
            }
        }
    }

    #[test]
    fn html_offsets_and_events_match_for_alignments_and_tails() {
        let template = br#"<div title="> quoted" class="a b"><x className=c></x><!-- > --></div>"#;

        for prefix in 0..32 {
            for tail in 0..64 {
                let mut storage = vec![b'x'; prefix];
                storage.extend_from_slice(template);
                storage.extend(std::iter::repeat_n(b' ', tail));
                let document = &storage[prefix..];

                let mut scalar =
                    Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), BackendKind::Scalar)
                        .unwrap();
                let mut expected = Vec::new();
                scalar
                    .parse_with(document, |event| expected.push(event))
                    .unwrap();
                let offsets = positions(&scalar);

                for backend in available_backends() {
                    for mode in [IndexMode::Tape, IndexMode::Masks] {
                        let mut parser =
                            Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), backend)
                                .unwrap()
                                .with_index_mode(mode)
                                .unwrap();
                        let mut events: Vec<HtmlEvent<'_>> = Vec::new();
                        parser
                            .parse_with(document, |event| events.push(event))
                            .unwrap();
                        assert_eq!(parser.index().mode(), mode);
                        assert_eq!(positions(&parser), offsets);
                        assert_eq!(events, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_mask_mode_reuses_capacity() {
        let html = br#"<a class="x">"#.repeat(16);
        let mut parser = Parser::<HtmlGrammar>::with_capacity(HtmlConfig::default(), 256)
            .unwrap()
            .with_index_mode(IndexMode::Masks)
            .unwrap();
        let capacity = parser.index_capacity_bytes();
        let stats = parser.stage1(&html).unwrap();
        assert_eq!(stats.mode, IndexMode::Masks);
        assert_eq!(parser.index_capacity_bytes(), capacity);
        // Four 64-byte blocks: one structural mask plus four class masks each.
        assert_eq!(stats.index_bytes, 4 * 5 * size_of::<u64>());
    }

    #[test]
    fn bounded_parser_rejects_oversize_and_reuses_capacity() {
        let mut parser = Parser::<JsonGrammar>::with_capacity(JsonConfig::default(), 128).unwrap();
        parser.parse_with(b"[1,2,3]", |_| {}).unwrap();
        let capacity = parser.index_capacity_bytes();
        parser.parse_with(br#"{"a":true}"#, |_| {}).unwrap();
        assert_eq!(parser.index_capacity_bytes(), capacity);
        assert_eq!(
            parser.stage1(&[b' '; 129]).unwrap_err().kind,
            crate::JsonErrorKind::Capacity
        );
    }
}
