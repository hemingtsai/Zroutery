//! A small TOML reader and writer, scoped to what Codex's config.toml actually contains.

// ---------------------------------------------------------------------------

// Minimal TOML support for Codex `config.toml`
// ---------------------------------------------------------------------------
//
// Codex reads TOML, and this crate has no TOML dependency available to this
// module, so the subset below covers the shapes a Codex config uses: comments,
// `[table]` headers, dotted or quoted keys, basic and literal strings,
// integers, floats, booleans and single-line arrays of those scalars. Anything
// else (arrays of tables, inline tables, multi-line strings, dates) is rejected
// with an explicit error instead of being silently misread, so `read_config`
// fails loudly rather than letting the adapter write a file the client cannot
// parse.

/// Parse the supported TOML subset into a JSON value.
pub(super) fn parse_toml_subset(text: &str) -> Result<serde_json::Value, String> {
    let mut root = serde_json::Value::Object(serde_json::Map::new());
    let mut table_path: Vec<String> = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with("[[") {
            return Err(format!(
                "line {line_number}: arrays of tables are not supported"
            ));
        }

        if let Some(inner) = line.strip_prefix('[') {
            let inner = inner
                .strip_suffix(']')
                .ok_or_else(|| format!("line {line_number}: unterminated table header"))?
                .trim();
            let path = parse_toml_key(inner, line_number)?;
            toml_table_mut(&mut root, &path, line_number)?;
            table_path = path;
            continue;
        }

        let split = find_unquoted(line, '=')
            .ok_or_else(|| format!("line {line_number}: expected `key = value`"))?;
        let mut key = parse_toml_key(&line[..split], line_number)?;
        let value = parse_toml_value(line[split + 1..].trim(), line_number)?;

        let mut path = table_path.clone();
        path.append(&mut key);
        insert_toml_value(&mut root, &path, value, line_number)?;
    }

    Ok(root)
}

/// Remove a `#` comment, ignoring `#` inside quoted strings.
fn strip_toml_comment(line: &str) -> &str {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if in_basic {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_basic = false;
            }
        } else if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
        } else if ch == '"' {
            in_basic = true;
        } else if ch == '\'' {
            in_literal = true;
        } else if ch == '#' {
            return &line[..index];
        }
    }

    line
}

/// Find the first occurrence of `needle` outside of quoted strings.
fn find_unquoted(text: &str, needle: char) -> Option<usize> {
    let mut in_basic = false;
    let mut in_literal = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if in_basic {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_basic = false;
            }
        } else if in_literal {
            if ch == '\'' {
                in_literal = false;
            }
        } else if ch == '"' {
            in_basic = true;
        } else if ch == '\'' {
            in_literal = true;
        } else if ch == needle {
            return Some(index);
        }
    }

    None
}

/// Parse a dotted, optionally quoted TOML key into its segments.
fn parse_toml_key(text: &str, line_number: usize) -> Result<Vec<String>, String> {
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;

    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }

        let segment = if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                if quote == b'"' && bytes[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            if end >= bytes.len() {
                return Err(format!("line {line_number}: unterminated quoted key"));
            }
            index = end + 1;
            text[start..end].to_string()
        } else {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'.'
            {
                index += 1;
            }
            if start == index {
                return Err(format!("line {line_number}: empty key segment"));
            }
            text[start..index].to_string()
        };

        if segment.is_empty() {
            return Err(format!("line {line_number}: empty key segment"));
        }
        parts.push(segment);

        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index] != b'.' {
            return Err(format!("line {line_number}: unexpected character in key"));
        }
        index += 1;
    }

    if parts.is_empty() {
        return Err(format!("line {line_number}: empty key"));
    }

    Ok(parts)
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

/// Parse a single-line TOML value.
fn parse_toml_value(text: &str, line_number: usize) -> Result<serde_json::Value, String> {
    let mut parser = TomlValueParser {
        input: text,
        pos: 0,
    };
    let value = parser.parse_value(line_number)?;
    parser.skip_whitespace();
    if parser.pos != text.len() {
        return Err(format!(
            "line {line_number}: unexpected trailing text `{}`",
            &text[parser.pos..]
        ));
    }
    Ok(value)
}

/// Cursor over the text of one TOML value.
struct TomlValueParser<'a> {
    input: &'a str,
    pos: usize,
}

impl TomlValueParser<'_> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(format!("line {line_number}: expected a value")),
            Some('"') => self
                .parse_basic_string(line_number)
                .map(serde_json::Value::String),
            Some('\'') => self
                .parse_literal_string(line_number)
                .map(serde_json::Value::String),
            Some('[') => self.parse_array(line_number),
            Some('{') => Err(format!(
                "line {line_number}: inline tables are not supported"
            )),
            Some(_) => self.parse_bare_value(line_number),
        }
    }

    fn parse_basic_string(&mut self, line_number: usize) -> Result<String, String> {
        self.bump(); // opening quote
        let mut out = String::new();

        loop {
            match self.bump() {
                None => return Err(format!("line {line_number}: unterminated string")),
                Some('"') => return Ok(out),
                Some('\\') => match self.bump() {
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
                                format!("line {line_number}: invalid unicode escape digit {ch:?}")
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
                },
                Some(ch) => out.push(ch),
            }
        }
    }

    fn parse_literal_string(&mut self, line_number: usize) -> Result<String, String> {
        self.bump(); // opening quote
        let start = self.pos;
        while let Some(ch) = self.bump() {
            if ch == '\'' {
                return Ok(self.input[start..self.pos - 1].to_string());
            }
        }
        Err(format!("line {line_number}: unterminated string"))
    }

    fn parse_array(&mut self, line_number: usize) -> Result<serde_json::Value, String> {
        self.bump(); // '['
        let mut items = Vec::new();

        loop {
            self.skip_whitespace();
            match self.peek() {
                None => return Err(format!("line {line_number}: unterminated array")),
                Some(']') => {
                    self.bump();
                    return Ok(serde_json::Value::Array(items));
                }
                _ => {}
            }

            items.push(self.parse_value(line_number)?);
            self.skip_whitespace();

            match self.bump() {
                Some(',') => continue,
                Some(']') => return Ok(serde_json::Value::Array(items)),
                Some(other) => {
                    return Err(format!(
                        "line {line_number}: expected `,` or `]` in array, found {other:?}"
                    ))
                }
                None => return Err(format!("line {line_number}: unterminated array")),
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
