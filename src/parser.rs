use std::fmt;

use crate::backend::{self, SAFETY_PADDING, Stage1Fn};
use crate::grammar::Grammar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Auto,
    Scalar,
    Ssse3,
    Avx2,
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
    pub structural_count: usize,
    pub logical_tape_bytes: usize,
    pub tape_capacity_bytes: usize,
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
    tape: Vec<u32>,
    stage1_state: G::Stage1State,
    stage2_state: G::Stage2State,
    stage1_fn: Stage1Fn<G>,
    backend: BackendKind,
    max_input: Option<usize>,
}

impl<G: Grammar> Parser<G> {
    pub fn new(config: G::Config) -> Self {
        let backend = best_backend();
        let stage1_fn = stage1_function::<G>(backend)
            .expect("the automatically selected parser backend must be available");
        let mut stage2_state = G::Stage2State::default();
        G::reserve_stage2(&config, &mut stage2_state, false)
            .expect("infallible parser state reservation failed");

        Self {
            config,
            tape: Vec::new(),
            stage1_state: G::Stage1State::default(),
            stage2_state,
            stage1_fn,
            backend,
            max_input: None,
        }
    }

    pub fn with_capacity(config: G::Config, max_input: usize) -> Result<Self, CapacityError> {
        if max_input >= u32::MAX as usize {
            return Err(CapacityError::InputTooLarge);
        }

        let backend = best_backend();
        let stage1_fn = stage1_function::<G>(backend)
            .expect("the automatically selected parser backend must be available");
        let requested = max_input
            .checked_add(SAFETY_PADDING)
            .ok_or(CapacityError::InputTooLarge)?;
        let mut tape = Vec::new();
        tape.try_reserve_exact(requested)
            .map_err(|_| CapacityError::AllocationFailed { requested })?;
        let mut stage2_state = G::Stage2State::default();
        G::reserve_stage2(&config, &mut stage2_state, true)?;

        Ok(Self {
            config,
            tape,
            stage1_state: G::Stage1State::default(),
            stage2_state,
            stage1_fn,
            backend,
            max_input: Some(max_input),
        })
    }

    pub fn with_backend(config: G::Config, backend: BackendKind) -> Result<Self, BackendError> {
        let selected = if backend == BackendKind::Auto {
            best_backend()
        } else {
            backend
        };
        let stage1_fn = stage1_function::<G>(selected)?;
        let mut stage2_state = G::Stage2State::default();
        G::reserve_stage2(&config, &mut stage2_state, false)
            .expect("infallible parser state reservation failed");

        Ok(Self {
            config,
            tape: Vec::new(),
            stage1_state: G::Stage1State::default(),
            stage2_state,
            stage1_fn,
            backend: selected,
            max_input: None,
        })
    }

    #[inline]
    pub fn backend(&self) -> BackendKind {
        self.backend
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

        self.tape.clear();
        if self.max_input.is_none() {
            let needed = input.len().saturating_add(SAFETY_PADDING);
            if self.tape.capacity() < needed {
                self.tape.reserve(needed - self.tape.capacity());
            }
        }

        let metrics = (self.stage1_fn)(input, &mut self.stage1_state, &mut self.tape)?;
        let structural_count = self.tape.len();
        Ok(Stage1Stats {
            input_bytes: input.len(),
            structural_count,
            logical_tape_bytes: structural_count * size_of::<u32>(),
            tape_capacity_bytes: self.tape.capacity() * size_of::<u32>(),
            blocks: metrics.blocks,
            density: if input.is_empty() {
                0.0
            } else {
                structural_count as f64 / input.len() as f64
            },
        })
    }

    #[inline]
    pub fn structural_offsets(&self) -> &[u32] {
        &self.tape
    }

    #[inline]
    pub fn tape_capacity_bytes(&self) -> usize {
        self.tape.capacity() * size_of::<u32>()
    }

    pub fn parse_with<'a>(
        &mut self,
        input: &'a [u8],
        mut sink: impl FnMut(G::Event<'a>),
    ) -> Result<ParseStats, G::Error>
    where
        G: 'a,
    {
        let stage1 = self.stage1(input)?;
        let mut events = 0usize;
        G::parse(
            input,
            &self.tape,
            &self.config,
            &mut self.stage2_state,
            |event| {
                events += 1;
                sink(event);
            },
        )?;
        Ok(ParseStats { stage1, events })
    }

    /// Run stage 2 over the tape produced by the most recent [`Self::stage1`].
    ///
    /// This is primarily useful for isolating stage-2 cost in benchmarks. The
    /// caller must pass the same input bytes used to build the current tape.
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
            &self.tape,
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

fn best_backend() -> BackendKind {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            return BackendKind::Avx2;
        }
        if std::is_x86_feature_detected!("ssse3") {
            return BackendKind::Ssse3;
        }
    }

    BackendKind::Scalar
}

fn stage1_function<G: Grammar>(backend: BackendKind) -> Result<Stage1Fn<G>, BackendError> {
    match backend {
        BackendKind::Auto => stage1_function::<G>(best_backend()),
        BackendKind::Scalar => Ok(backend::scalar::<G>),
        BackendKind::Ssse3 => {
            #[cfg(target_arch = "x86_64")]
            if std::is_x86_feature_detected!("ssse3") {
                return Ok(backend::ssse3::<G>);
            }
            Err(BackendError { requested: backend })
        }
        BackendKind::Avx2 => {
            #[cfg(target_arch = "x86_64")]
            if std::is_x86_feature_detected!("avx2") {
                return Ok(backend::avx2::<G>);
            }
            Err(BackendError { requested: backend })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HtmlConfig, HtmlEvent, HtmlGrammar, JsonConfig, JsonError, JsonEvent, JsonGrammar,
    };

    fn available_backends() -> Vec<BackendKind> {
        let mut backends = vec![BackendKind::Scalar];
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("ssse3") {
                backends.push(BackendKind::Ssse3);
            }
            if std::is_x86_feature_detected!("avx2") {
                backends.push(BackendKind::Avx2);
            }
        }
        backends
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
            let expected_offsets = expected_parser.structural_offsets().to_vec();
            let expected_error: Option<JsonError> = expected_result.err();

            for backend in available_backends() {
                let mut parser =
                    Parser::<JsonGrammar>::with_backend(JsonConfig::default(), backend).unwrap();
                let mut events: Vec<JsonEvent<'_>> = Vec::new();
                let result = parser.parse_with(document, |event| events.push(event));
                assert_eq!(parser.structural_offsets(), expected_offsets);
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
                let offsets = scalar.structural_offsets().to_vec();

                for backend in available_backends() {
                    let mut parser =
                        Parser::<HtmlGrammar>::with_backend(HtmlConfig::default(), backend)
                            .unwrap();
                    let mut events: Vec<HtmlEvent<'_>> = Vec::new();
                    parser
                        .parse_with(document, |event| events.push(event))
                        .unwrap();
                    assert_eq!(parser.structural_offsets(), offsets);
                    assert_eq!(events, expected);
                }
            }
        }
    }

    #[test]
    fn bounded_parser_rejects_oversize_and_reuses_capacity() {
        let mut parser = Parser::<JsonGrammar>::with_capacity(JsonConfig::default(), 128).unwrap();
        parser.parse_with(b"[1,2,3]", |_| {}).unwrap();
        let capacity = parser.tape.capacity();
        parser.parse_with(br#"{"a":true}"#, |_| {}).unwrap();
        assert_eq!(parser.tape.capacity(), capacity);
        assert_eq!(
            parser.stage1(&[b' '; 129]).unwrap_err().kind,
            crate::JsonErrorKind::Capacity
        );
    }
}
