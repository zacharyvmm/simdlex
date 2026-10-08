use super::{
    YamlBlockScalar, YamlChomping, YamlError, YamlErrorKind, YamlScalar, YamlScalarStyle, error,
};

/// Decode a scalar's value into `scratch`, which is cleared first.
///
/// Applies YAML line folding to plain and quoted scalars, `''` and
/// backslash escapes to quoted scalars, and indentation removal, folding,
/// and chomping to block scalars. Tags are not applied: the result is the
/// scalar's string content.
pub fn decode_yaml_scalar<'s>(
    scalar: &YamlScalar<'_>,
    scratch: &'s mut String,
) -> Result<&'s str, YamlError> {
    let offset = scalar.span.start as usize;
    let raw = std::str::from_utf8(scalar.raw)
        .map_err(|error_| error(YamlErrorKind::Utf8, offset + error_.valid_up_to()))?;
    scratch.clear();
    match scalar.style {
        YamlScalarStyle::Plain => fold_flow(raw, Quoting::Plain, offset, scratch)?,
        YamlScalarStyle::SingleQuoted => fold_flow(raw, Quoting::Single, offset, scratch)?,
        YamlScalarStyle::DoubleQuoted => fold_flow(raw, Quoting::Double, offset, scratch)?,
        YamlScalarStyle::Literal(header) => block(raw, header, false, scratch),
        YamlScalarStyle::Folded(header) => block(raw, header, true, scratch),
    }
    Ok(scratch)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoting {
    Plain,
    Single,
    Double,
}

/// Flow scalar folding: trailing blanks of a line are dropped, leading
/// blanks of continuation lines are dropped, one line break becomes a
/// space, and `n > 1` breaks become `n - 1` newlines.
fn fold_flow(
    raw: &str,
    quoting: Quoting,
    offset: usize,
    out: &mut String,
) -> Result<(), YamlError> {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    // `out` length after the last character that line folding must keep.
    let mut keep = 0;
    let mut at = 0;

    while at < len {
        match bytes[at] {
            b'\n' | b'\r' => {
                out.truncate(keep);
                let mut breaks = 0;
                loop {
                    at += if bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                    breaks += 1;
                    while at < len && matches!(bytes[at], b' ' | b'\t') {
                        at += 1;
                    }
                    if at >= len || !matches!(bytes[at], b'\n' | b'\r') {
                        break;
                    }
                }
                if breaks == 1 {
                    out.push(' ');
                } else {
                    out.extend(std::iter::repeat_n('\n', breaks - 1));
                }
                keep = out.len();
            }
            b'\'' if quoting == Quoting::Single && bytes.get(at + 1) == Some(&b'\'') => {
                out.push('\'');
                at += 2;
                keep = out.len();
            }
            b'\\' if quoting == Quoting::Double => {
                at = escape(raw, at, offset, out)?;
                keep = out.len();
            }
            _ => {
                let start = at;
                let special = |byte: u8| match byte {
                    b'\n' | b'\r' => true,
                    b'\'' => quoting == Quoting::Single,
                    b'\\' => quoting == Quoting::Double,
                    _ => false,
                };
                while at < len && !special(bytes[at]) {
                    at += 1;
                }
                let run = &raw[start..at];
                out.push_str(run);
                let trailing = run.len() - run.trim_end_matches([' ', '\t']).len();
                if trailing < run.len() {
                    keep = out.len() - trailing;
                }
            }
        }
    }
    Ok(())
}

/// Decode the escape at `raw[at] == '\\'`. Returns the offset after it.
fn escape(raw: &str, at: usize, offset: usize, out: &mut String) -> Result<usize, YamlError> {
    let bytes = raw.as_bytes();
    let invalid = || error(YamlErrorKind::InvalidEscape, offset + at);
    let Some(&kind) = bytes.get(at + 1) else {
        return Err(invalid());
    };
    let simple = match kind {
        b'0' => '\0',
        b'a' => '\x07',
        b'b' => '\x08',
        b't' | b'\t' => '\t',
        b'n' => '\n',
        b'v' => '\x0b',
        b'f' => '\x0c',
        b'r' => '\r',
        b'e' => '\x1b',
        b' ' => ' ',
        b'"' => '"',
        b'/' => '/',
        b'\\' => '\\',
        b'N' => '\u{85}',
        b'_' => '\u{a0}',
        b'L' => '\u{2028}',
        b'P' => '\u{2029}',
        b'\n' | b'\r' => {
            // An escaped line break joins lines without a space; empty lines
            // after it are still kept as newlines.
            let mut next = at + 1;
            let mut first = true;
            loop {
                next += if bytes[next] == b'\r' && bytes.get(next + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                if !first {
                    out.push('\n');
                }
                first = false;
                while next < bytes.len() && matches!(bytes[next], b' ' | b'\t') {
                    next += 1;
                }
                if next >= bytes.len() || !matches!(bytes[next], b'\n' | b'\r') {
                    return Ok(next);
                }
            }
        }
        b'x' | b'u' | b'U' => {
            let digits = match kind {
                b'x' => 2,
                b'u' => 4,
                _ => 8,
            };
            let hex = raw.get(at + 2..at + 2 + digits).ok_or_else(invalid)?;
            let value = u32::from_str_radix(hex, 16).map_err(|_| invalid())?;
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid());
            }
            out.push(char::from_u32(value).ok_or_else(invalid)?);
            return Ok(at + 2 + digits);
        }
        _ => return Err(invalid()),
    };
    out.push(simple);
    Ok(at + 2)
}

/// Block scalar content: strip indentation, fold (for `>`), and chomp.
fn block(raw: &str, header: YamlBlockScalar, folded: bool, out: &mut String) {
    let indent = header.indent as usize;
    let body = raw.strip_suffix('\n').unwrap_or(raw);
    let body = body.strip_suffix('\r').unwrap_or(body);
    let ends_with_break = body.len() < raw.len();
    // Lines with indentation removed, split at CRLF, LF, or a lone CR. Stage
    // 2 guarantees that non-empty lines carry at least `indent` spaces, so
    // shorter lines are empty.
    let lines = || {
        body.split('\n')
            .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
            .take(if raw.is_empty() { 0 } else { usize::MAX })
            .map(|line| line.get(indent..).unwrap_or(""))
    };
    let line_count = lines().count();
    let last_content = lines()
        .enumerate()
        .filter(|(_, text)| !text.is_empty())
        .map(|(index, _)| index)
        .last();

    if let Some(last) = last_content {
        let mut previous_normal: Option<bool> = None;
        let mut empty = 0;
        for text in lines().take(last + 1) {
            if text.is_empty() {
                empty += 1;
                continue;
            }
            // Folding only joins adjacent lines that are not more indented.
            let normal = !text.starts_with([' ', '\t']);
            match previous_normal {
                None => out.extend(std::iter::repeat_n('\n', empty)),
                Some(true) if folded && normal => {
                    if empty == 0 {
                        out.push(' ');
                    } else {
                        out.extend(std::iter::repeat_n('\n', empty));
                    }
                }
                Some(_) => out.extend(std::iter::repeat_n('\n', empty + 1)),
            }
            out.push_str(text);
            previous_normal = Some(normal);
            empty = 0;
        }
    }

    let lines_after = match last_content {
        Some(last) => line_count - 1 - last,
        None => line_count.saturating_sub(1),
    };
    let trailing = lines_after + usize::from(ends_with_break);
    match header.chomping {
        YamlChomping::Strip => {}
        YamlChomping::Clip => {
            if last_content.is_some() && trailing > 0 {
                out.push('\n');
            }
        }
        YamlChomping::Keep => out.extend(std::iter::repeat_n('\n', trailing)),
    }
}
