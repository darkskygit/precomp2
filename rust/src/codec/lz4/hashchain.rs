//! Lazy hash-chain parser of liblz4 r131 (`LZ4HC_compress_generic`, which
//! 1.7.3+ renamed `LZ4HC_compress_hashChain`), plus its distance-ring hash
//! table. Byte-exact port of the C output.

use super::{
  LAST_LITERALS, MF_LIMIT,
  common::{MIN_MATCH, Match, common_prefix, hash4, read32},
  writer::{LengthEstimate, Overflow, SequenceWriter},
};

/// Farthest reachable match (64 KiB - 1).
const MAX_OFFSET: usize = 65535;
const EMPTY: u32 = u32::MAX;

/// Hash heads plus a 64K ring of distances to the previous position with the
/// same hash.
struct HashChain {
  heads: Vec<u32>,
  prev_distance: Vec<u16>,
  next_to_insert: usize,
  attempts: usize,
}

impl HashChain {
  fn new(attempts: usize) -> Self {
    HashChain {
      heads: vec![EMPTY; 1 << 15],
      prev_distance: vec![0xFFFF; 1 << 16],
      next_to_insert: 0,
      attempts,
    }
  }

  /// Inserts every position before `pos` that is not yet in the chain.
  #[inline(always)]
  fn insert_until(&mut self, src: &[u8], pos: usize) {
    for p in self.next_to_insert..pos {
      let head = &mut self.heads[hash4(src, p)];
      let distance = if *head == EMPTY {
        MAX_OFFSET
      } else {
        ((p as u32).wrapping_sub(*head) as usize).min(MAX_OFFSET)
      };
      self.prev_distance[p as u16 as usize] = distance as u16;
      *head = p as u32;
    }
    // Assigned even when `pos` is behind (C does the same; the lazy parser
    // never moves back).
    self.next_to_insert = pos;
  }

  /// Up to `attempts` earlier positions sharing the hash of `pos`, nearest
  /// first.
  #[inline(always)]
  fn candidates(&mut self, src: &[u8], pos: usize) -> impl Iterator<Item = usize> + '_ {
    self.insert_until(src, pos);
    let head = self.heads[hash4(src, pos)];
    let mut next = (head != EMPTY).then_some(head as usize);
    let prev_distance = &self.prev_distance;
    std::iter::from_fn(move || {
      // C compares against `pos - (64 KiB - 1)` only, so it would accept
      // indices >= pos too.
      let cand = next.filter(|&c| c + MAX_OFFSET >= pos)?;
      next = cand.checked_sub(prev_distance[cand as u16 as usize] as usize);
      Some(cand)
    })
    .take(self.attempts)
  }

  /// Longest match starting exactly at `pos`; the first one found wins ties.
  #[inline(always)]
  fn longest_match(&mut self, src: &[u8], pos: usize, limit: usize) -> Option<Match> {
    let mut best: Option<Match> = None;
    for cand in self.candidates(src, pos) {
      let best_len = best.map_or(0, |m| m.len);
      // Cheap reject on the byte that would have to extend the best match.
      if src[cand + best_len] != src[pos + best_len] || read32(src, cand) != read32(src, pos) {
        continue;
      }
      let len = MIN_MATCH + common_prefix(src, pos + MIN_MATCH, cand + MIN_MATCH, limit);
      if len > best_len {
        best = Some(Match {
          start: pos,
          source: cand,
          len,
        });
      }
    }
    best
  }

  /// Longest match found at `pos`, extended backwards down to `min_start`, that
  /// is longer than `longest`.
  #[inline(always)]
  fn wider_match(&mut self, src: &[u8], pos: usize, min_start: usize, limit: usize, mut longest: usize) -> Option<Match> {
    let max_back = pos - min_start;
    let mut best = None;
    for cand in self.candidates(src, pos) {
      // The byte just past the current best must match (`longest >= max_back`
      // here). Written as `longest - max_back` first: the invariant
      // guarantees it is non-negative, while `cand - max_back` alone
      // could wrap in debug builds.
      if src[min_start + longest] != src[cand + (longest - max_back)] || read32(src, cand) != read32(src, pos) {
        continue;
      }
      let forward = MIN_MATCH + common_prefix(src, pos + MIN_MATCH, cand + MIN_MATCH, limit);
      let back = (1..=max_back.min(cand)).take_while(|&b| src[pos - b] == src[cand - b]).count();
      if forward + back > longest {
        longest = forward + back;
        best = Some(Match {
          start: pos - back,
          source: cand - back,
          len: longest,
        });
      }
    }
    best
  }
}

/// Longest match that is still encoded with a single token nibble (`15 - 1 +
/// MIN_MATCH`).
const OPTIMAL_ML: usize = 18;

struct Parser<'s, 'w> {
  src: &'s [u8],
  out: &'w mut SequenceWriter<'s>,
  finder: HashChain,
  match_limit: usize,
  mf_limit: usize,
}

impl<'s, 'w> Parser<'s, 'w> {
  fn emit(&mut self, m: Match) -> Result<(), Overflow> {
    self.out.sequence(m.start, m.offset(), m.len)
  }

  /// A longer match overlapping the last `overlap` bytes of `m`, possibly
  /// starting as early as `min_start`.
  fn wider(&mut self, m: Match, overlap: usize, min_start: usize) -> Option<Match> {
    if m.end() >= self.mf_limit {
      return None;
    }
    self
      .finder
      .wider_match(self.src, m.end() - overlap, min_start, self.match_limit, m.len)
  }

  /// Resolves the overlapping matches that follow `first` and emits them.
  /// Returns where scanning resumes.
  fn encode_from(&mut self, first: Match) -> Result<usize, Overflow> {
    let (mut m1, mut fallback) = (first, first);
    'search2: loop {
      // r131 lets m2 start one byte after m1 at the earliest; 1.7.3+ allow
      // m1.start itself, which changes the output.
      let Some(mut m2) = self.wider(m1, 2, m1.start + 1) else {
        self.emit(m1)?;
        return Ok(m1.end());
      };
      // m1 was a replacement that m2 makes pointless: go back to the earlier
      // match.
      if fallback.start < m1.start && m2.start < m1.start + fallback.len {
        m1 = fallback;
      }
      // m2 starts so close to m1 that m1 is not worth a token.
      if m2.start - m1.start < 3 {
        m1 = m2;
        continue 'search2;
      }
      'search3: loop {
        // Keep m1 short enough for a one-nibble length; give the rest to m2.
        if m2.start - m1.start < OPTIMAL_ML {
          let gap = m2.start - m1.start;
          let keep = m1.len.min(OPTIMAL_ML).min(gap + m2.len - MIN_MATCH);
          if keep > gap {
            m2.skip(keep - gap);
          }
        }
        let Some(m3) = self.wider(m2, 3, m2.start) else {
          m1.len = m1.len.min(m2.start - m1.start);
          self.emit(m1)?;
          self.emit(m2)?;
          return Ok(m2.end());
        };
        if m3.start < m1.end() + 3 {
          if m3.start >= m1.end() {
            // m2 is squeezed out by m1 and m3: emit m1, restart from m3 with m2
            // as fallback.
            if m2.start < m1.end() {
              let overlap = m1.end() - m2.start;
              if m2.len < overlap + MIN_MATCH {
                m2 = m3;
              } else {
                m2.skip(overlap);
              }
            }
            self.emit(m1)?;
            (m1, fallback) = (m3, m2);
            continue 'search2;
          }
          m2 = m3;
          continue 'search3;
        }
        // Three ascending matches: m1 can be written now.
        if m2.start < m1.end() {
          let gap = m2.start - m1.start;
          if gap < 15 {
            m1.len = m1.len.min(OPTIMAL_ML).min(gap + m2.len - MIN_MATCH);
            if m1.len > gap {
              m2.skip(m1.len - gap);
            }
          } else {
            m1.len = gap;
          }
        }
        self.emit(m1)?;
        (m1, m2) = (m2, m3);
      }
    }
  }
}

fn parse<'a>(src: &'a [u8], out: &mut SequenceWriter<'a>, attempts: usize) -> Result<(), Overflow> {
  let mut parser = Parser {
    src,
    out,
    finder: HashChain::new(attempts),
    match_limit: src.len().saturating_sub(LAST_LITERALS),
    mf_limit: src.len().saturating_sub(MF_LIMIT),
  };
  let mut pos = 1;
  while pos < parser.mf_limit {
    pos = match parser.finder.longest_match(src, pos, parser.match_limit) {
      Some(m) => parser.encode_from(m)?,
      None => pos + 1,
    };
  }
  Ok(())
}

pub(super) fn compress(src: &[u8], limit: Option<usize>, attempts: usize) -> Option<Vec<u8>> {
  let mut out = SequenceWriter::new(src, limit, LengthEstimate::Shift8);
  parse(src, &mut out, attempts).ok()?;
  out.finish().ok()
}
