#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassSpan {
    pub value_start: usize,
    pub value_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassToken<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}
