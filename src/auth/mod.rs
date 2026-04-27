pub mod jwt;
pub mod password;

/// Constant-time byte comparison that does not short-circuit on length mismatch,
/// preventing timing-based token length disclosure.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max_len  = a.len().max(b.len());
    let len_diff = a.len() ^ b.len();
    (0..max_len).fold(len_diff, |acc, i| {
        acc | (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize
    }) == 0
}
