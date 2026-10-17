//! Match finder of the optimal parser (`LZ4HC_Insert` +
//! `LZ4HC_InsertAndGetWiderMatch`), 1.8.2+ release classes.

use super::common::{MIN_MATCH, Match, common_prefix, hash, read16, read32, run_backward, run_forward};

const MAX_DISTANCE: u32 = 65_535;
/// Table index of `src[0]`. liblz4 starts a fresh context at 64 KiB, so an
/// empty hash slot (index 0) is always outside the window. All window and
/// chain arithmetic below is on these indices.
const BASE: u32 = 1 << 16;

/// Behaviour switches between release classes; each constant is named after
/// the release its tests compare against.
#[derive(Clone, Copy)]
pub(super) struct Quirks {
  /// 1.9.2+: chain-swap scan skips ahead (`kTrigger` acceleration).
  pub accelerated_chain_swap: bool,
  /// 1.9.2+: repeat candidate must lie inside the 64 KiB window (1.9.1-: only
  /// `>= dictLimit`).
  pub repeat_candidate_in_window: bool,
}

impl Quirks {
  /// 1.8.2–1.9.1.
  pub const V1_9_1: Quirks = Quirks {
    accelerated_chain_swap: false,
    repeat_candidate_in_window: false,
  };
  /// 1.9.2–1.10.0.
  pub const V1_10_0: Quirks = Quirks {
    accelerated_chain_swap: true,
    repeat_candidate_in_window: true,
  };
}

pub(super) struct MatchFinder<'a> {
  src: &'a [u8],
  /// hash of 4 bytes -> newest index
  head: Vec<u32>,
  /// `index as u16` -> distance to the previous index with the same hash
  /// (saturates at 65535)
  prev: Vec<u16>,
  next_to_update: u32,
  quirks: Quirks,
}

impl<'a> MatchFinder<'a> {
  pub fn new(src: &'a [u8], quirks: Quirks) -> Self {
    MatchFinder {
      src,
      head: vec![0; 1 << 15],
      prev: vec![u16::MAX; 1 << 16],
      next_to_update: BASE,
      quirks,
    }
  }

  pub fn len(&self) -> usize {
    self.src.len()
  }

  fn pos(idx: u32) -> usize {
    (idx - BASE) as usize
  }

  fn delta(&self, idx: u32) -> u32 {
    self.prev[idx as u16 as usize] as u32
  }

  /// Insert every position before `pos`.
  fn insert_up_to(&mut self, pos: usize) {
    let target = pos as u32 + BASE;
    for idx in self.next_to_update..target {
      let h = hash(read32(self.src, Self::pos(idx)));
      self.prev[idx as u16 as usize] = idx.wrapping_sub(self.head[h]).min(MAX_DISTANCE) as u16;
      self.head[h] = idx;
    }
    self.next_to_update = target;
  }

  /// `LZ4HC_FindLongerMatch`: longest match at `ip` (`ip <= limit - 4`), no
  /// backward extension. Returns a match only if it is longer than `longest`.
  pub fn longer_match(&mut self, ip: usize, limit: usize, mut longest: usize, mut attempts: u32) -> Option<Match> {
    self.insert_up_to(ip);
    let s = self.src;
    let ip_idx = ip as u32 + BASE;
    let lowest = ip_idx.saturating_sub(MAX_DISTANCE).max(BASE);
    let pattern = read32(s, ip);
    // Some(len of the byte run starting at ip) once tested positive
    let mut run: Option<Option<usize>> = None;
    let mut chain_shift = 0u32;
    let mut best = None;

    let mut idx = self.head[hash(pattern)];
    while idx >= lowest && attempts > 0 {
      attempts -= 1;
      let cand = Self::pos(idx);
      let mut len = 0;
      // cheap reject: the byte pair that would make the match longer must agree
      if read16(s, ip + longest - 1) == read16(s, cand + longest - 1) && read32(s, cand) == pattern {
        len = MIN_MATCH + common_prefix(s, ip + MIN_MATCH, cand + MIN_MATCH, limit);
        if len > longest {
          longest = len;
          best = Some(Match {
            start: ip,
            source: cand,
            len,
          });
        }
      }

      if len == longest
        && idx + longest as u32 <= ip_idx
        && let Some((dist, shift)) = self.farthest_chain(idx, longest)
      {
        chain_shift = shift;
        idx -= dist;
        continue;
      }

      if chain_shift == 0 && self.delta(idx) == 1 {
        let run_len = *run.get_or_insert_with(|| {
          let b = pattern as u8;
          (pattern == u32::from_ne_bytes([b; 4])).then(|| MIN_MATCH + run_forward(s, ip + MIN_MATCH, limit, b))
        });
        let cand_idx = idx - 1;
        let window_floor = if self.quirks.repeat_candidate_in_window { lowest } else { BASE };
        if let Some(src_run) = run_len.filter(|_| cand_idx >= window_floor) {
          let cand = Self::pos(cand_idx);
          if read32(s, cand) == pattern {
            let byte = pattern as u8;
            let fwd = MIN_MATCH + run_forward(s, cand + MIN_MATCH, limit, byte);
            let back = run_backward(s, cand, Self::pos(lowest), byte);
            let segment = back + fwd;
            if segment >= src_run && fwd <= src_run {
              // align the run ends; a full-length match follows
              idx = cand_idx + fwd as u32 - src_run as u32;
            } else {
              // farthest position of this run
              idx = cand_idx - back as u32;
              let ml = segment.min(src_run);
              if longest < ml {
                // 1.8.3+: repeat-pattern match farther than 65535 is rejected
                // (all optimal-parser classes).
                if ip_idx - idx > MAX_DISTANCE {
                  break;
                }
                longest = ml;
                best = Some(Match {
                  start: ip,
                  source: Self::pos(idx),
                  len: ml,
                });
              }
              idx -= self.delta(idx);
            }
            continue;
          }
        }
      }

      idx -= self.delta(idx + chain_shift);
    }
    best
  }

  /// Chain swap: among positions `idx..=idx+longest-4`, the one whose
  /// predecessor is farthest (> 1). 1.9.2+ samples sparser after 16 misses.
  fn farthest_chain(&self, idx: u32, longest: usize) -> Option<(u32, u32)> {
    let span = (longest - MIN_MATCH + 1) as u32;
    let mut best: Option<(u32, u32)> = None;
    let mut best_dist = 1;
    let mut pos = 0;
    let mut misses = 0u32;
    while pos < span {
      // C: `step = accel++ >> 4`, taken before an improvement resets accel to
      // 16
      let step = if self.quirks.accelerated_chain_swap {
        (16 + misses) >> 4
      } else {
        1
      };
      let dist = self.delta(idx + pos);
      if dist > best_dist {
        best_dist = dist;
        best = Some((dist, pos));
        misses = 0;
      } else {
        misses += 1;
      }
      pos += step;
    }
    best
  }
}
