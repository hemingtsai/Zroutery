//! A small TOML reader and writer, scoped to what Codex's config.toml actually contains.

// ---------------------------------------------------------------------------

// Minimal TOML support for Codex `config.toml`
// ---------------------------------------------------------------------------
//
// Codex reads TOML, and this crate has no TOML dependency available to this
// module, so the subset below covers the shapes a Codex config uses: comments,
// `[table]` headers, dotted or quoted keys, basic and literal strings,
// integers, floats, booleans, and arrays of those scalars — single-line or
// spanning lines, because `args = [` with one item per line is how a real
// config lists a command's arguments, and rejecting it would refuse half the
// configs Codex itself accepts. Anything else (arrays of tables, inline
// tables, multi-line strings, dates) is rejected with an explicit error
// instead of being silently misread, so `read_config` fails loudly rather than
// letting the adapter write a file the client cannot parse.
//
// The reader is cursor-based rather than line-based for exactly that reason:
// whether an array is finished is a property of the value, not of the line it
// started on.

/// Parse the supported TOML subset into a JSON value.
pub(super) fn parse_toml_subset(text: &str) -> Result<serde_json::Value, String> {
    let mut root = serde_json::Value::Object(serde_json::Map::new());
    let mut parser = TomlValueParser {
        input: text,
        pos: 0,
        line: 1,
    };
    let mut table_path: Vec<String> = Vec::new();

    loop {
        parser.skip_trivia();
        if parser.pos >= parser.input.len() {
            break;
        }

        // A `[table]` header, or the array-of-tables form this subset refuses.
        if parser.peek() == Some('[') {
            let line_number = parser.line;
            parser.bump();
            if parser.peek() == Some('[') {
                return Err(format!(
                    "line {line_number}: arrays of tables are not supported"
                ));
            }
            parser.skip_spaces();
            let path = parser.parse_key_path()?;
            parser.skip_spaces();
            match parser.bump() {
                Some(']') => {}
                other => {
                    return Err(format!(
                        "line {line_number}: unterminated table header, found {other:?}"
                    ))
                }
            }
            toml_table_mut(&mut root, &path, line_number)?;
            table_path = path;
            continue;
        }

        // `key = value`, where the value may be an array spanning lines.
        let line_number = parser.line;
        let mut key = parser.parse_key_path()?;
        parser.skip_spaces();
        match parser.bump() {
            Some('=') => {}
            other => return Err(format!("line {line_number}: expected `=`, found {other:?}")),
        }
        let value = parser.parse_value(line_number)?;
        // The line ends the value: only trailing spaces, an optional comment,
        // then the newline. Anything else on the line is not a document this
        // subset represents, and reading it as two pairs would invent config.
        parser.skip_spaces();
        match parser.peek() {
            None | Some('\n') | Some('\r') | Some('#') => {}
            Some(other) => {
                return Err(format!(
                    "line {}: unexpected trailing text `{}`",
                    parser.line, other
                ))
            }
        }

        let mut path = table_path.clone();
        path.append(&mut key);
        insert_toml_value(&mut root, &path, value, parser.line)?;
    }

    Ok(root)
}

/// Whether `ch` can appear in a bare (unquoted) TOML key.
fn is_bare_key_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

/// Navigate to a table, creating missing tables and rejecting conflicts.
fn toml_table_mut<'a>(
    root: &'a mut serde_json::Value,
    path: &[String],
    line_number: usize,
) -> Result<&'a mut serde_json::Value, String> {
    let mut current = root;

    for part in path {
        if !current.is_object() {
            return Err(format!("line {line_number}: `{part}` is not a table"));
        }
        let object = current.as_object_mut().unwrap();
        current = object
            .entry(part.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !current.is_object() {
            return Err(format!(
                "line {line_number}: `{part}` is a value, not a table"
            ));
        }
    }

    Ok(current)
}

/// Insert a key/value pair at `path`, rejecting duplicate keys.
fn insert_toml_value(
    root: &mut serde_json::Value,
    path: &[String],
    value: serde_json::Value,
    line_number: usize,
) -> Result<(), String> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| format!("line {line_number}: empty key"))?;
    let table = toml_table_mut(root, parents, line_number)?;
    let object = table.as_object_mut().unwrap();
    if object.contains_key(last) {
        return Err(format!("line {line_number}: duplicate key `{last}`"));
    }
    object.insert(last.clone(), value);
    Ok(())
}

/// Cursor over the whole document, tracking the line for error messages.
struct TomlValueParser<'a> {
    input: &'a str,
    pos: usize,
    line: usize,
}

impl TomlValueParser<'_> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
        }
        Some(ch)
    }

    /// Spaces, tabs and carriage returns on the current line.
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.pos += 1;
        }
    }

    /// Whitespace, newlines and comments — the gaps TOML allows between
    /// items, and inside a multi-line array.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => {
                    self.pos += 1;
                }
                Some('\n') => {
                    self.bump();
                }
                Some('#') => {
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.pos += ch.len_utf8();
                    }
                }
                _ => break,
            }
        }
    }

    /// Parse a dotted, optionally quoted key path at the cursor.
    fn parse_key_path(&mut self) -> Result<Vec<String>, String> {
        let line = self.line;
        let mut parts = Vec::new();

        loop {
            self.skip_spaces();
            let segment = match self.peek() {
                Some('"') => {
                    self.bump();
                    self.parse_basic_string(line)?
                }
                Some('\'') => {
                    self.bump();
                    self.parse_literal_string(line)?
                }
                Some(ch) if is_bare_key_char(ch) => {
                    let start = self.pos;
                    while matches!(self.peek(), Some(c) if is_bare_key_char(c)) {
                        self.bump();
                    }
                    self.input[start..self.pos].to_string()
                }
                other => {
                    return Err(format!(
                        "line {line}: expected a key segment, found {other:?}"
                    ))
                }
            };
            if segment.is_empty() {
                return Err(format!("line {line}: empty key segment"));
            }
            parts.push(segment);

            self.skip_spaces();
            if self.peek() == Some('.') {
                self.bump();
                continue;
            }
            break;
        }

        if parts.is_empty() {
            return Err(format!("line {line}: empty key"));
        }

        Ok(parts)
    }

    /// Parse one TOML value at the cursor. Arrays may span lines and carry
    /// comments; strings may not span lines.
    fn parse_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.skip_spaces();
        match self.peek() {
            None => Err(format!("line {line_number}: expected a value")),
            Some('"') => {
                self.bump();
                self.parse_basic_string(line_number)
                    .map(serde_json::Value::String)
            }
            Some('\'') => {
                self.bump();
                self.parse_literal_string(line_number)
                    .map(serde_json::Value::String)
            }
            Some('[') => self.parse_array(),
            Some('{') => Err(format!(
                "line {line_number}: inline tables are not supported"
            )),
            Some(_) => self.parse_bare_value(line_number),
        }
    }

    fn parse_basic_string(&mut self, line_number: usize) -> Result<String, String> {
        // The opening quote was consumed by the caller.
        let mut out = String::new();

        loop {
            match self.peek() {
                None => return Err(format!("line {line_number}: unterminated string")),
                Some('\n') | Some('\r') => {
                    return Err(format!(
                        "line {line_number}: unterminated string (a basic string cannot span \
                         lines)"
                    ))
                }
                Some('"') => {
                    self.bump();
                    return Ok(out);
                }
                Some('\\') => {
                    self.bump();
                    match self.bump() {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('r') => out.push('\r'),
                        Some('"') => out.push('"'),
                        Some('\\') => out.push('\\'),
                        Some('b') => out.push('\u{8}'),
                        Some('f') => out.push('\u{c}'),
                        Some(marker @ ('u' | 'U')) => {
                            let digits = if marker == 'u' { 4 } else { 8 };
                            let mut code = 0u32;
                            for _ in 0..digits {
                                let ch = self.bump().ok_or_else(|| {
                                    format!("line {line_number}: unterminated unicode escape")
                                })?;
                                let digit = ch.to_digit(16).ok_or_else(|| {
                                    format!(
                                        "line {line_number}: invalid unicode escape digit {ch:?}"
                                    )
                                })?;
                                code = code * 16 + digit;
                            }
                            let ch = char::from_u32(code).ok_or_else(|| {
                                format!("line {line_number}: invalid unicode scalar {code:#x}")
                            })?;
                            out.push(ch);
                        }
                        Some(other) => {
                            return Err(format!(
                                "line {line_number}: unsupported escape `\\{other}`"
                            ))
                        }
                        None => return Err(format!("line {line_number}: unterminated string")),
                    }
                }
                Some(ch) => {
                    self.bump();
                    out.push(ch);
                }
            }
        }
    }

    fn parse_literal_string(&mut self, line_number: usize) -> Result<String, String> {
        // The opening quote was consumed by the caller.
        let start = self.pos;
        loop {
            match self.peek() {
                None | Some('\n') | Some('\r') => {
                    return Err(format!("line {line_number}: unterminated string"))
                }
                Some('\'') => {
                    let text = self.input[start..self.pos].to_string();
                    self.bump();
                    return Ok(text);
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }

    fn parse_array(&mut self) -> Result<serde_json::Value, String> {
        let open_line = self.line;
        self.bump(); // '['
        let mut items = Vec::new();

        loop {
            // Inside the brackets the grammar allows newlines and comments,
            // which is the whole reason this parser follows the value rather
            // than the line.
            self.skip_trivia();
            match self.peek() {
                None => return Err(format!("line {open_line}: unterminated array")),
                Some(']') => {
                    self.bump();
                    return Ok(serde_json::Value::Array(items));
                }
                _ => {}
            }

            items.push(self.parse_value(self.line)?);
            self.skip_trivia();

            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {
                    self.bump();
                    return Ok(serde_json::Value::Array(items));
                }
                other => {
                    return Err(format!(
                        "line {}: expected `,` or `]` in array, found {other:?}",
                        self.line
                    ))
                }
            }
        }
    }

    fn parse_bare_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '_' | '.') {
                self.bump();
            } else {
                break;
            }
        }

        let token = &self.input[start..self.pos];
        match token {
            "true" => return Ok(serde_json::Value::Bool(true)),
            "false" => return Ok(serde_json::Value::Bool(false)),
            "" => {
                return Err(format!(
                    "line {line_number}: expected a value, found {:?}",
                    self.peek()
                ))
            }
            _ => {}
        }

        let cleaned = token.replace('_', "");
        if let Ok(integer) = cleaned.parse::<i64>() {
            return Ok(serde_json::Value::Number(integer.into()));
        }
        if let Ok(float) = cleaned.parse::<f64>() {
            if let Some(number) = serde_json::Number::from_f64(float) {
                return Ok(serde_json::Value::Number(number));
            }
        }

        Err(format!(
            "line {line_number}: unsupported TOML value `{token}`"
        ))
    }
}

/// Serialize a JSON object as the supported TOML subset.
pub(super) fn serialize_toml_subset(value: &serde_json::Value) -> Result<String, String> {
    let table = value
        .as_object()
        .ok_or_else(|| "TOML config must be a table".to_string())?;
    let mut out = String::new();
    write_toml_table(&mut out, &[], table)?;
    Ok(out)
}

/// Write one table: its scalar keys first, then its child sections.
fn write_toml_table(
    out: &mut String,
    path: &[String],
    table: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if !path.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push('[');
        out.push_str(
            &path
                .iter()
                .map(|segment| quote_toml_key(segment))
                .collect::<Vec<_>>()
                .join("."),
        );
        out.push_str("]\n");
    }

    let mut nested = Vec::new();
    for (key, value) in table {
        if value.is_object() {
            nested.push((key, value));
            continue;
        }
        out.push_str(&quote_toml_key(key));
        out.push_str(" = ");
        out.push_str(&format_toml_value(value)?);
        out.push('\n');
    }

    for (key, value) in nested {
        let mut child = path.to_vec();
        child.push(key.clone());
        write_toml_table(out, &child, value.as_object().unwrap())?;
    }

    Ok(())
}

fn format_toml_value(value: &serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::Null => Err("TOML has no null value".to_string()),
        serde_json::Value::Bool(flag) => Ok(flag.to_string()),
        serde_json::Value::Number(number) => Ok(number.to_string()),
        serde_json::Value::String(text) => Ok(quote_toml_string(text)),
        serde_json::Value::Array(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                if item.is_object() {
                    return Err("arrays of tables are not supported".to_string());
                }
                rendered.push(format_toml_value(item)?);
            }
            Ok(format!("[{}]", rendered.join(", ")))
        }
        serde_json::Value::Object(_) => {
            Err("nested tables must be written as sections".to_string())
        }
    }
}

/// Quote a key unless it is a bare key.
fn quote_toml_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if bare {
        key.to_string()
    } else {
        quote_toml_string(key)
    }
}

/// Quote a string as a TOML basic string.
fn quote_toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 || ch == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
