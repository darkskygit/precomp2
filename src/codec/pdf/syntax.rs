use std::{collections::BTreeMap, ops::Range};

#[derive(Clone, Debug)]
pub(super) enum Value {
  Int(u64),
  Name(Vec<u8>),
  Array(Vec<Value>),
  Dict(BTreeMap<Vec<u8>, Value>),
  Ref(u64, u64),
  Bool(bool),
  Null,
  Other,
}

impl Value {
  pub(super) fn get(&self, key: &[u8]) -> Option<&Self> {
    match self {
      Self::Dict(items) => items.get(key),
      _ => None,
    }
  }

  pub(super) fn int(&self) -> Option<u64> {
    match self {
      Self::Int(n) => Some(*n),
      _ => None,
    }
  }

  pub(super) fn name(&self) -> Option<&[u8]> {
    match self {
      Self::Name(n) => Some(n),
      _ => None,
    }
  }
}

pub(super) struct Stream {
  pub range: Range<usize>,
  pub dict: Value,
}

pub(super) fn whitespace(b: u8) -> bool {
  matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn delimiter(b: u8) -> bool {
  whitespace(b) || b"()<>[]{}/%".contains(&b)
}

struct Parser<'a> {
  data: &'a [u8],
  pos: usize,
}

impl Parser<'_> {
  fn skip(&mut self) {
    loop {
      while self.data.get(self.pos).is_some_and(|b| whitespace(*b)) {
        self.pos += 1;
      }
      if self.data.get(self.pos) != Some(&b'%') {
        break;
      }
      while self.data.get(self.pos).is_some_and(|b| !matches!(b, b'\n' | b'\r')) {
        self.pos += 1;
      }
    }
  }

  fn word(&mut self, word: &[u8]) -> bool {
    self.skip();
    let end = self.pos + word.len();
    if self.data.get(self.pos..end) == Some(word) && self.data.get(end).is_none_or(|b| delimiter(*b)) {
      self.pos = end;
      true
    } else {
      false
    }
  }

  fn number(&mut self) -> Option<u64> {
    self.skip();
    let start = self.pos;
    while self.data.get(self.pos).is_some_and(u8::is_ascii_digit) {
      self.pos += 1;
    }
    if start == self.pos || self.data.get(self.pos).is_some_and(|b| !delimiter(*b)) {
      return None;
    }
    std::str::from_utf8(&self.data[start..self.pos]).ok()?.parse().ok()
  }

  fn value(&mut self, depth: u32) -> Option<Value> {
    if depth > 32 {
      return None;
    }
    self.skip();
    let b = *self.data.get(self.pos)?;
    match b {
      b'/' => {
        self.pos += 1;
        let mut name = Vec::new();
        while let Some(&b) = self.data.get(self.pos) {
          if delimiter(b) {
            break;
          }
          if b == b'#' {
            let h = self.data.get(self.pos + 1..self.pos + 3)?;
            name.push(u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?);
            self.pos += 3;
          } else {
            name.push(b);
            self.pos += 1;
          }
        }
        Some(Value::Name(name))
      }
      b'[' => {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
          self.skip();
          if self.data.get(self.pos) == Some(&b']') {
            self.pos += 1;
            break;
          }
          items.push(self.value(depth + 1)?);
        }
        Some(Value::Array(items))
      }
      b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
        self.pos += 2;
        let mut items = BTreeMap::new();
        loop {
          self.skip();
          if self.data.get(self.pos..self.pos + 2) == Some(b">>") {
            self.pos += 2;
            break;
          }
          let Value::Name(key) = self.value(depth + 1)? else {
            return None;
          };
          if items.insert(key, self.value(depth + 1)?).is_some() {
            return None;
          }
        }
        Some(Value::Dict(items))
      }
      b'(' => {
        self.pos += 1;
        let mut nesting = 1usize;
        while nesting > 0 {
          let b = *self.data.get(self.pos)?;
          self.pos += 1;
          match b {
            b'\\' => {
              self.data.get(self.pos)?;
              self.pos += 1;
            }
            b'(' => nesting += 1,
            b')' => nesting -= 1,
            _ => (),
          }
        }
        Some(Value::Other)
      }
      b'<' => {
        self.pos += 1;
        while *self.data.get(self.pos)? != b'>' {
          self.pos += 1;
        }
        self.pos += 1;
        Some(Value::Other)
      }
      b'0'..=b'9' => {
        let start = self.pos;
        let Some(n) = self.number() else {
          self.pos = start;
          while self.data.get(self.pos).is_some_and(|b| !delimiter(*b)) {
            self.pos += 1;
          }
          return Some(Value::Other);
        };
        let end = self.pos;
        if let Some(generation) = self.number()
          && self.word(b"R")
        {
          return Some(Value::Ref(n, generation));
        }
        self.pos = end;
        Some(Value::Int(n))
      }
      _ => {
        if self.word(b"true") {
          return Some(Value::Bool(true));
        }
        if self.word(b"false") {
          return Some(Value::Bool(false));
        }
        if self.word(b"null") {
          return Some(Value::Null);
        }
        let start = self.pos;
        while self.data.get(self.pos).is_some_and(|b| !delimiter(*b)) {
          self.pos += 1;
        }
        if start == self.pos {
          self.pos += 1;
        }
        Some(Value::Other)
      }
    }
  }
}

fn stream_end(data: &[u8], end: usize) -> Option<usize> {
  let mut parser = Parser { data, pos: end };
  if parser.word(b"endstream") && parser.word(b"endobj") {
    Some(parser.pos)
  } else {
    None
  }
}

fn unresolved_end(data: &[u8], start: usize) -> Option<usize> {
  let mut pos = start;
  while let Some(offset) = memchr::memmem::find(&data[pos..], b"endstream") {
    pos += offset;
    if pos > start
      && whitespace(data[pos - 1])
      && let Some(end) = stream_end(data, pos)
    {
      return Some(end);
    }
    pos += 9;
  }
  None
}

// This is a physical-range parser, not an object-graph serializer. Accept only
// nonoverlapping ranges whose direct/resolved Length agrees with endstream;
// ambiguous references and inline images remain in the caller's literal spans.
pub(super) fn streams(data: &[u8]) -> Vec<Stream> {
  let mut parser = Parser { data, pos: 0 };
  let mut objects = BTreeMap::new();
  let mut pending = Vec::new();
  while parser.pos < data.len() {
    parser.skip();
    let start = parser.pos;
    let header = (|| {
      let id = parser.number()?;
      let generation = parser.number()?;
      parser.word(b"obj").then_some((id, generation))
    })();
    let Some(id) = header else {
      parser.pos = start;
      if parser.value(0).is_none() {
        parser.pos = start.saturating_add(1);
      }
      continue;
    };
    let Some(value) = parser.value(0) else {
      parser.pos = start + 1;
      continue;
    };
    if matches!(value, Value::Dict(_)) && parser.word(b"stream") {
      let start = match data.get(parser.pos..) {
        Some(rest) if rest.starts_with(b"\r\n") => parser.pos + 2,
        Some(rest) if rest.starts_with(b"\n") || rest.starts_with(b"\r") => parser.pos + 1,
        _ => break,
      };
      let length = value.get(b"Length").and_then(Value::int).and_then(|n| usize::try_from(n).ok());
      let end = length.and_then(|n| start.checked_add(n)).filter(|end| *end <= data.len());
      let after = end.and_then(|end| stream_end(data, end)).or_else(|| unresolved_end(data, start));
      pending.push((start, value));
      if let Some(after) = after {
        parser.pos = after;
      } else {
        break;
      }
    } else if parser.word(b"endobj") {
      objects.entry(id).and_modify(|value| *value = Value::Other).or_insert(value);
    }
  }
  let mut result = Vec::new();
  let mut previous_end = 0;
  for (start, dict) in pending {
    let length = dict.get(b"Length").and_then(|v| resolve(v, &objects, 0)).and_then(Value::int);
    let Some(end) = length.and_then(|n| usize::try_from(n).ok()).and_then(|n| start.checked_add(n)) else {
      continue;
    };
    if end > data.len() || start < previous_end || stream_end(data, end).is_none() {
      continue;
    }
    let dict = resolve_dict(dict, &objects);
    result.push(Stream { range: start..end, dict });
    previous_end = end;
  }
  result
}

fn resolve<'a>(value: &'a Value, objects: &'a BTreeMap<(u64, u64), Value>, depth: usize) -> Option<&'a Value> {
  if depth > 16 {
    return None;
  }
  match value {
    Value::Ref(id, generation) => resolve(objects.get(&(*id, *generation))?, objects, depth + 1),
    _ => Some(value),
  }
}

fn resolve_dict(mut dict: Value, objects: &BTreeMap<(u64, u64), Value>) -> Value {
  if let Value::Dict(items) = &mut dict {
    for key in [
      b"Filter".as_slice(),
      b"DecodeParms",
      b"Width",
      b"Height",
      b"BitsPerComponent",
      b"ColorSpace",
    ] {
      if let Some(value) = items.get_mut(key)
        && let Some(resolved) = resolve(value, objects, 0)
      {
        *value = resolved.clone();
      }
    }
    if let Some(Value::Array(params)) = items.get_mut(b"DecodeParms".as_slice()) {
      for value in params {
        if let Some(resolved) = resolve(value, objects, 0) {
          *value = resolved.clone();
        }
      }
    }
  }
  dict
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn length_references_and_lexical_boundaries() {
    let payload = b"literal endstream endobj 17 0 obj << /Length 3 >> stream\nfoo";
    for indirect in [false, true] {
      let length = if indirect { "2 0 R".to_owned() } else { payload.len().to_string() };
      let mut data =
        format!("%PDF-1.7\n1 0 obj << /Length {length} /BBox [0 0 1.5 -2.3] /Name /Fl#61teDecode /Label (stream \\( fake) >> stream\r\n")
          .into_bytes();
      data.extend_from_slice(payload);
      data.extend_from_slice(format!("\r\nendstream\nendobj\n2 0 obj {} endobj\n", payload.len()).as_bytes());
      let streams = streams(&data);
      assert_eq!(streams.len(), 1);
      assert_eq!(&data[streams[0].range.clone()], payload);
      assert_eq!(streams[0].dict.get(b"Name").and_then(Value::name), Some(b"FlateDecode".as_slice()));
    }
  }

  #[test]
  fn unresolved_or_conflicting_lengths_are_not_transformed() {
    for length in ["-1", "1.5", "999999", "2 0 R", "18446744073709551616"] {
      let data = format!("%PDF\n1 0 obj << /Length {length} >> stream\nabc\nendstream\nendobj\n");
      assert!(streams(data.as_bytes()).is_empty());
    }
    let data = b"%PDF\n1 0 obj << /Length 2 0 R >> stream\nabc\nendstream\nendobj\n2 0 obj 3 endobj\n2 0 obj 4 endobj\n";
    assert!(streams(data).is_empty());
  }

  #[test]
  fn resolves_filter_and_parameter_arrays_without_losing_nulls() {
    let data = b"%PDF\n1 0 obj << /Length 2 /Filter 2 0 R /DecodeParms [null 3 0 R] >> stream\n00\nendstream\nendobj\n2 0 obj [/AHx /Fl] endobj\n3 0 obj << /Predictor 15 /Columns 7 >> endobj\n";
    let streams = streams(data);
    assert_eq!(streams.len(), 1);
    let Some(Value::Array(params)) = streams[0].dict.get(b"DecodeParms") else {
      panic!()
    };
    assert!(matches!(params[0], Value::Null));
    assert_eq!(params[1].get(b"Predictor").and_then(Value::int), Some(15));
  }
}
