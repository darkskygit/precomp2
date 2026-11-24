//! Optimal parser (`LZ4HC_compress_optimal`) of the 1.8.2+ release classes;
//! version differences live in the match finder's [`Quirks`].
//!
//! For each block starting at `ip`, `opt[k]` is the cheapest known way to
//! reach `ip + k` (price in output bytes) and the last step taken to get
//! there. The block ends at the farthest match end; the cheapest path is
//! then traced back and emitted.

use super::{
  common::{LAST_LITERALS, MF_LIMIT, MIN_MATCH, Match},
  finder::{MatchFinder, Quirks},
  writer::{LengthEstimate, Overflow, SequenceWriter},
};

/// Size of the price table (`LZ4_OPT_NUM`); also caps `sufficient_len`.
const OPT_NUM: usize = 1 << 12;
/// Literal-only positions priced past the last match end.
const TRAILING_LITERALS: usize = 3;

#[derive(Clone, Copy, Default)]
struct Node {
  price: i32,
  /// Step that ends here: a match of `mlen >= 4` bytes at `off`, or a literal
  /// (`mlen == 1`).
  mlen: usize,
  off: usize,
  /// Literals before this position (for literal steps), or literals before the
  /// match.
  litlen: usize,
}

impl Node {
  fn literal(price: i32, litlen: usize) -> Self {
    Node {
      price,
      mlen: 1,
      off: 0,
      litlen,
    }
  }
}

/// Bytes for `litlen` literals including the extra length bytes.
fn literals_price(litlen: usize) -> i32 {
  let extra = if litlen >= 15 { 1 + (litlen - 15) / 255 } else { 0 };
  (litlen + extra) as i32
}

/// Bytes for a whole sequence: token, literals, offset, extra match-length
/// bytes.
fn sequence_price(litlen: usize, mlen: usize) -> i32 {
  let ml_extra = if mlen >= 15 + MIN_MATCH {
    1 + (mlen - 15 - MIN_MATCH) / 255
  } else {
    0
  };
  3 + literals_price(litlen) + ml_extra as i32
}

/// Row of liblz4's `clTable` for an optimal-parser level.
#[derive(Clone, Copy)]
pub(super) struct Level {
  attempts: u32,
  /// Matches longer than this are taken without pricing alternatives.
  sufficient_len: usize,
  /// `fullUpdate`: also search where a short match could still pay off, and
  /// accept any match length there.
  full_update: bool,
}

/// `LZ4HC_CLEVEL_MAX`.
pub(super) const LEVEL_12: Level = Level {
  attempts: 16384,
  sufficient_len: OPT_NUM,
  full_update: true,
};

pub(super) fn compress(src: &[u8], limit: Option<usize>, quirks: Quirks, level: Level) -> Option<Vec<u8>> {
  let mut finder = MatchFinder::new(src, quirks);
  let mut out = SequenceWriter::new(src, limit, LengthEstimate::Div255);
  parse(&mut finder, &mut out, level).ok()?;
  out.finish().ok()
}

fn parse(finder: &mut MatchFinder, out: &mut SequenceWriter, level: Level) -> Result<(), Overflow> {
  let len = finder.len();
  if len < MF_LIMIT {
    return Ok(());
  }
  let mf_limit = len - MF_LIMIT;
  let limit = len - LAST_LITERALS;
  let Level {
    attempts,
    sufficient_len,
    full_update,
  } = level;
  let sufficient_len = sufficient_len.min(OPT_NUM - 1);
  let mut opt = vec![Node::default(); OPT_NUM + TRAILING_LITERALS];
  let mut path = Vec::new();

  let mut ip = 0;
  while ip <= mf_limit {
    let Some(first) = finder.longer_match(ip, limit, MIN_MATCH - 1, attempts) else {
      ip += 1;
      continue;
    };
    if first.len > sufficient_len {
      out.sequence(ip, first.offset(), first.len)?;
      ip = first.end();
      continue;
    }

    let llen = ip - out.anchor();
    for (k, node) in opt[..MIN_MATCH].iter_mut().enumerate() {
      *node = Node::literal(literals_price(llen + k), llen + k);
    }
    for (ml, node) in opt.iter_mut().enumerate().take(first.len + 1).skip(MIN_MATCH) {
      *node = Node {
        price: sequence_price(llen, ml),
        mlen: ml,
        off: first.offset(),
        litlen: llen,
      };
    }
    let mut last = first.len;
    pad_literals(&mut opt, last);

    // Final step of the block: (position it starts at, match).
    let mut forced: Option<(usize, Match)> = None;
    for cur in 1.. {
      if cur >= last || ip + cur > mf_limit {
        break;
      }
      let here = opt[cur].price;
      let next_not_worse = opt[cur + 1].price <= here;
      // full update still searches when a short match could pay off just after
      if next_not_worse && (!full_update || opt[cur + MIN_MATCH].price < here + 3) {
        continue;
      }
      let min_len = if full_update { MIN_MATCH - 1 } else { last - cur };
      let Some(m) = finder.longer_match(ip + cur, limit, min_len, attempts) else {
        continue;
      };
      if m.len > sufficient_len || cur + m.len >= OPT_NUM {
        forced = Some((cur, m));
        break;
      }
      relax_literals(&mut opt, cur);
      relax_match(&mut opt, cur, &m, &mut last);
      pad_literals(&mut opt, last);
    }

    let (start, step) = match forced {
      Some((cur, m)) => (cur, (m.len, m.offset())),
      None => (last - opt[last].mlen, (opt[last].mlen, opt[last].off)),
    };
    trace_back(&opt, start, step, &mut path);
    for &(mlen, off) in path.iter().rev() {
      if mlen == 1 {
        ip += 1;
      } else {
        out.sequence(ip, off, mlen)?;
        ip += mlen;
      }
    }
  }
  Ok(())
}

/// Price the literal steps right after the last match end.
fn pad_literals(opt: &mut [Node], last: usize) {
  for k in 1..=TRAILING_LITERALS {
    opt[last + k] = Node::literal(opt[last].price + literals_price(k), k);
  }
}

/// Extend the literal run ending at `cur` by 1..=3 bytes.
fn relax_literals(opt: &mut [Node], cur: usize) {
  let base = opt[cur];
  for k in 1..MIN_MATCH {
    let price = base.price - literals_price(base.litlen) + literals_price(base.litlen + k);
    if price < opt[cur + k].price {
      opt[cur + k] = Node::literal(price, base.litlen + k);
    }
  }
}

/// Price every prefix (>= 4) of match `m` starting at `cur`.
fn relax_match(opt: &mut [Node], cur: usize, m: &Match, last: &mut usize) {
  // a match after literals is priced together with them as one sequence
  let (litlen, base_price) = if opt[cur].mlen == 1 {
    let ll = opt[cur].litlen;
    (ll, if cur > ll { opt[cur - ll].price } else { 0 })
  } else {
    (0, opt[cur].price)
  };
  let horizon = *last + TRAILING_LITERALS;
  for ml in MIN_MATCH..=m.len {
    let pos = cur + ml;
    let price = base_price + sequence_price(litlen, ml);
    // positions past the horizon hold stale data and are always overwritten
    if pos > horizon || price <= opt[pos].price {
      if ml == m.len && pos > *last {
        *last = pos;
      }
      opt[pos] = Node {
        price,
        mlen: ml,
        off: m.offset(),
        litlen,
      };
    }
  }
}

/// Collect the steps of the cheapest path to `start`, plus `final_step`, in
/// reverse order.
fn trace_back(opt: &[Node], mut pos: usize, final_step: (usize, usize), path: &mut Vec<(usize, usize)>) {
  path.clear();
  path.push(final_step);
  while pos > 0 {
    let n = opt[pos];
    path.push((n.mlen, n.off));
    pos -= n.mlen;
  }
}
