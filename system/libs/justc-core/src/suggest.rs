//! "Did you mean ...?" suggestions.

use crate::prelude::*;
/// Levenshtein distance between two strings, by `char`.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, &cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The candidate closest to `name`, if it is close enough to be a plausible
/// typo. Exact matches are not suggestions.
pub fn best_match<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .into_iter()
        .map(|c| (edit_distance(name, c), c))
        .filter(|&(d, c)| d > 0 && d <= 2.max(c.chars().count() / 3))
        .min()
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_and_matches() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(best_match("cuont", ["count", "x"]), Some("count"));
        assert_eq!(best_match("count", ["count"]), None);
        assert_eq!(best_match("zzzzzz", ["count"]), None);
    }
}
