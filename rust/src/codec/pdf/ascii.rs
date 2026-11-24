//! ASCII metadata uses varint counts/indices and length-prefixed byte blobs:
//! significant length, `z` tuple indices, sparse (index, byte) spelling
//! patches, (index, blob) whitespace gaps, then the original terminator/suffix
//! blob. Gap offsets refer to significant characters, not decoded bytes. This
//! retains case, odd hex nibbles, partial ASCII85 tuples and z/!!!!! choices
//! without storing a second full copy of the original ASCII stream.

use super::{Error, Result, read_blob, read_num, whitespace, write_blob, write_num};

pub(super) fn unpack(input: &[u8], hex: bool) -> Result<(Vec<u8>, Vec<u8>)> {
  let marker = if hex { b">".as_slice() } else { b"~>".as_slice() };
  let end = memchr::memmem::find(input, marker).ok_or(Error::InvalidSegment("ascii terminator"))?;
  let mut significant = Vec::new();
  let mut gaps = Vec::new();
  let mut pos = 0;
  while pos < end {
    if whitespace(input[pos]) {
      let start = pos;
      while pos < end && whitespace(input[pos]) {
        pos += 1;
      }
      gaps.push((significant.len(), &input[start..pos]));
    } else {
      significant.push(input[pos]);
      pos += 1;
    }
  }
  let mut binary = Vec::new();
  let mut zeros = Vec::new();
  if hex {
    for pair in significant.chunks(2) {
      let hi = digit(pair[0])?;
      let lo = if pair.len() == 2 { digit(pair[1])? } else { 0 };
      binary.push(hi * 16 + lo);
    }
  } else {
    let mut pos = 0;
    while pos < significant.len() {
      if significant[pos] == b'z' {
        zeros.push(binary.len() / 4);
        binary.extend_from_slice(&[0; 4]);
        pos += 1;
      } else {
        let count = (significant.len() - pos).min(5);
        if count == 1 {
          return Err(Error::InvalidSegment("ascii85 final group"));
        }
        let mut word = 0u64;
        for i in 0..5 {
          let c = if i < count { significant[pos + i] } else { b'u' };
          if !(b'!'..=b'u').contains(&c) {
            return Err(Error::InvalidSegment("ascii85 digit"));
          }
          word = word * 85 + u64::from(c - b'!');
        }
        let word = u32::try_from(word).map_err(|_| Error::InvalidSegment("ascii85 overflow"))?;
        binary.extend_from_slice(&word.to_be_bytes()[..count - 1]);
        pos += count;
      }
    }
  }
  let canonical = significant_bytes(&binary, hex, significant.len(), &zeros)?;
  let patches: Vec<_> = significant
    .iter()
    .zip(&canonical)
    .enumerate()
    .filter(|(_, (a, b))| a != b)
    .collect();
  let mut meta = Vec::new();
  write_num(significant.len(), &mut meta);
  write_num(zeros.len(), &mut meta);
  for zero in zeros {
    write_num(zero, &mut meta);
  }
  write_num(patches.len(), &mut meta);
  for (index, (&byte, _)) in patches {
    write_num(index, &mut meta);
    meta.push(byte);
  }
  write_num(gaps.len(), &mut meta);
  for (index, bytes) in gaps {
    write_num(index, &mut meta);
    write_blob(bytes, &mut meta);
  }
  write_blob(&input[end..], &mut meta);
  Ok((binary, meta))
}

pub(super) fn repack(binary: &[u8], meta: &[u8], hex: bool, limit: usize) -> Result<Vec<u8>> {
  let mut pos = 0;
  let significant_len = read_num(meta, &mut pos)?;
  if significant_len > limit {
    return Err(Error::InvalidSegment("ascii length"));
  }
  let count = read_num(meta, &mut pos)?;
  if count > meta.len() {
    return Err(Error::InvalidSegment("ascii zero count"));
  }
  let mut zeros = Vec::new();
  for _ in 0..count {
    zeros.push(read_num(meta, &mut pos)?);
  }
  if zeros.windows(2).any(|p| p[0] >= p[1]) || zeros.last().is_some_and(|n| *n >= binary.len() / 4) {
    return Err(Error::InvalidSegment("ascii zero index"));
  }
  let mut significant = significant_bytes(binary, hex, significant_len, &zeros)?;
  let count = read_num(meta, &mut pos)?;
  if count > meta.len() {
    return Err(Error::InvalidSegment("ascii patches"));
  }
  for _ in 0..count {
    let index = read_num(meta, &mut pos)?;
    let byte = *meta.get(pos).ok_or(Error::InvalidSegment("ascii patch"))?;
    pos += 1;
    *significant.get_mut(index).ok_or(Error::InvalidSegment("ascii patch index"))? = byte;
  }
  let count = read_num(meta, &mut pos)?;
  if count > meta.len() {
    return Err(Error::InvalidSegment("ascii gaps"));
  }
  let mut out = Vec::new();
  let mut cursor = 0;
  for _ in 0..count {
    let index = read_num(meta, &mut pos)?;
    if index < cursor || index > significant.len() {
      return Err(Error::InvalidSegment("ascii gap index"));
    }
    let gap = read_blob(meta, &mut pos)?;
    if out.len().saturating_add(index - cursor).saturating_add(gap.len()) > limit {
      return Err(Error::InvalidSegment("ascii output limit"));
    }
    out.extend_from_slice(&significant[cursor..index]);
    out.extend_from_slice(gap);
    cursor = index;
  }
  let suffix = read_blob(meta, &mut pos)?;
  if pos != meta.len() || out.len().saturating_add(significant.len() - cursor).saturating_add(suffix.len()) > limit {
    return Err(Error::InvalidSegment("ascii output size"));
  }
  out.extend_from_slice(&significant[cursor..]);
  out.extend_from_slice(suffix);
  Ok(out)
}

fn digit(b: u8) -> Result<u8> {
  match b {
    b'0'..=b'9' => Ok(b - b'0'),
    b'a'..=b'f' => Ok(b - b'a' + 10),
    b'A'..=b'F' => Ok(b - b'A' + 10),
    _ => Err(Error::InvalidSegment("asciihex digit")),
  }
}

fn significant_bytes(binary: &[u8], hex: bool, len: usize, zeros: &[usize]) -> Result<Vec<u8>> {
  let mut out = Vec::new();
  if hex {
    if !zeros.is_empty() || (len != binary.len().saturating_mul(2) && len.checked_add(1) != binary.len().checked_mul(2)) {
      return Err(Error::InvalidSegment("asciihex length"));
    }
    for b in binary {
      out.push(b"0123456789abcdef"[usize::from(b >> 4)]);
      out.push(b"0123456789abcdef"[usize::from(b & 15)]);
    }
    out.truncate(len);
  } else {
    let mut zero = 0;
    for (index, chunk) in binary.chunks(4).enumerate() {
      if zeros.get(zero) == Some(&index) {
        if chunk != [0; 4] {
          return Err(Error::InvalidSegment("ascii85 zero tuple"));
        }
        out.push(b'z');
        zero += 1;
        continue;
      }
      let mut word = [0u8; 4];
      word[..chunk.len()].copy_from_slice(chunk);
      let mut word = u32::from_be_bytes(word);
      let mut chars = [0; 5];
      for c in chars.iter_mut().rev() {
        *c = (word % 85) as u8 + b'!';
        word /= 85;
      }
      out.extend_from_slice(&chars[..chunk.len() + 1]);
    }
    if out.len() != len {
      return Err(Error::InvalidSegment("ascii85 length"));
    }
  }
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn original_ascii_spelling_roundtrips() {
    for (hex, data) in [
      (true, b"aB cD\r\nF>tail".as_slice()),
      (false, b"z !!!!!\n87cURD]j7BEbo80~>\r\n".as_slice()),
      (false, b" !! ~>".as_slice()),
      (true, b">".as_slice()),
    ] {
      let (binary, meta) = unpack(data, hex).unwrap();
      assert_eq!(repack(&binary, &meta, hex, data.len()).unwrap(), data);
    }
    for data in [b"!~>".as_slice(), b"uuuuu~>", b"!z~>"] {
      assert!(unpack(data, false).is_err());
    }
  }
}
