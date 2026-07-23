//! A faithful port of the parts of Python's `difflib` that the tool system
//! depends on: `SequenceMatcher` (matching blocks, opcodes, grouped opcodes,
//! `ratio()` — including the autojunk heuristic) and `unified_diff`.
//!
//! `ratio()` is load-bearing for the fuzzy matcher's `block_anchor` /
//! `context_aware` strategies and must return the exact 2*M/T value CPython
//! computes, so this is a line-for-line port rather than an approximation.

use std::collections::HashMap;
use std::hash::Hash;

/// An opcode tag, mirroring difflib's string tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

pub type Opcode = (Tag, usize, usize, usize, usize);

pub struct SequenceMatcher<'a, T: Eq + Hash> {
    a: &'a [T],
    b: &'a [T],
    b2j: HashMap<&'a T, Vec<usize>>,
    bpopular: std::collections::HashSet<&'a T>,
}

impl<'a, T: Eq + Hash> SequenceMatcher<'a, T> {
    /// Equivalent of `SequenceMatcher(None, a, b)` (autojunk=True).
    pub fn new(a: &'a [T], b: &'a [T]) -> Self {
        let mut b2j: HashMap<&'a T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt).or_default().push(i);
        }
        // autojunk: elements appearing in more than 1% of positions in b are
        // treated as popular ("junk") when b is at least 200 elements long.
        let mut bpopular = std::collections::HashSet::new();
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            for (elt, idxs) in b2j.iter() {
                if idxs.len() > ntest {
                    bpopular.insert(*elt);
                }
            }
            for elt in &bpopular {
                b2j.remove(elt);
            }
        }
        Self { a, b, b2j, bpopular }
    }

    fn is_bjunk(&self, elt: &T) -> bool {
        self.bpopular.contains(elt)
    }

    /// Port of `find_longest_match`.
    pub fn find_longest_match(
        &self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(indices) = self.b2j.get(&self.a[i]) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = if j == 0 { 1 } else { j2len.get(&(j - 1)).copied().unwrap_or(0) + 1 };
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        // Extend the best by non-junk elements on each end.
        while besti > alo
            && bestj > blo
            && !self.is_bjunk(&self.b[bestj - 1])
            && self.a[besti - 1] == self.b[bestj - 1]
        {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && !self.is_bjunk(&self.b[bestj + bestsize])
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }
        // Then extend by junk elements on each end.
        while besti > alo
            && bestj > blo
            && self.is_bjunk(&self.b[bestj - 1])
            && self.a[besti - 1] == self.b[bestj - 1]
        {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && self.is_bjunk(&self.b[bestj + bestsize])
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    /// Port of `get_matching_blocks` (non-recursive queue version).
    pub fn get_matching_blocks(&self) -> Vec<(usize, usize, usize)> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0usize, la, 0usize, lb)];
        let mut matching_blocks: Vec<(usize, usize, usize)> = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                matching_blocks.push((i, j, k));
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        matching_blocks.sort_unstable();
        // Merge adjacent blocks.
        let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
        let mut non_adjacent = Vec::new();
        for (i2, j2, k2) in matching_blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    non_adjacent.push((i1, j1, k1));
                }
                i1 = i2;
                j1 = j2;
                k1 = k2;
            }
        }
        if k1 > 0 {
            non_adjacent.push((i1, j1, k1));
        }
        non_adjacent.push((la, lb, 0));
        non_adjacent
    }

    /// Port of `get_opcodes`.
    pub fn get_opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0usize, 0usize);
        let mut answer = Vec::new();
        for (ai, bj, size) in self.get_matching_blocks() {
            let tag = if i < ai && j < bj {
                Some(Tag::Replace)
            } else if i < ai {
                Some(Tag::Delete)
            } else if j < bj {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(t) = tag {
                answer.push((t, i, ai, j, bj));
            }
            i = ai + size;
            j = bj + size;
            if size > 0 {
                answer.push((Tag::Equal, ai, i, bj, j));
            }
        }
        answer
    }

    /// Port of `get_grouped_opcodes(n)`.
    pub fn get_grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.get_opcodes();
        if codes.is_empty() {
            codes.push((Tag::Equal, 0, 1, 0, 1));
        }
        if codes[0].0 == Tag::Equal {
            let (tag, i1, i2, j1, j2) = codes[0];
            codes[0] = (tag, i1.max(i2.saturating_sub(n)), i2, j1.max(j2.saturating_sub(n)), j2);
        }
        let last = codes.len() - 1;
        if codes[last].0 == Tag::Equal {
            let (tag, i1, i2, j1, j2) = codes[last];
            codes[last] = (tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n));
        }
        let nn = n + n;
        let mut groups = Vec::new();
        let mut group: Vec<Opcode> = Vec::new();
        for (tag, mut i1, i2, mut j1, j2) in codes {
            if tag == Tag::Equal && i2 - i1 > nn {
                group.push((tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n)));
                groups.push(std::mem::take(&mut group));
                i1 = i1.max(i2.saturating_sub(n));
                j1 = j1.max(j2.saturating_sub(n));
            }
            group.push((tag, i1, i2, j1, j2));
        }
        if !(group.is_empty() || (group.len() == 1 && group[0].0 == Tag::Equal)) {
            groups.push(group);
        }
        groups
    }

    /// Port of `ratio()` — 2.0*M / T.
    pub fn ratio(&self) -> f64 {
        let matches: usize = self.get_matching_blocks().iter().map(|t| t.2).sum();
        let length = self.a.len() + self.b.len();
        if length == 0 {
            1.0
        } else {
            2.0 * matches as f64 / length as f64
        }
    }
}

/// `SequenceMatcher(None, a, b).ratio()` over the characters of two strings.
pub fn ratio_chars(a: &str, b: &str) -> f64 {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    SequenceMatcher::new(&av, &bv).ratio()
}

/// Port of `difflib._format_range_unified`.
fn format_range_unified(start: usize, stop: usize) -> String {
    let mut beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return format!("{}", beginning);
    }
    if length == 0 {
        beginning -= 1;
    }
    format!("{},{}", beginning, length)
}

/// Split into lines keeping line endings — Python's `str.splitlines(keepends=True)`
/// restricted to `\n` / `\r\n` / `\r` (the forms that occur in files we edit).
pub fn split_lines_keepends(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..=i]);
                start = i + 1;
                i += 1;
            }
            b'\r' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    out.push(&text[start..i + 2]);
                    start = i + 2;
                    i += 2;
                } else {
                    out.push(&text[start..=i]);
                    start = i + 1;
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    }
    out
}

/// Port of `difflib.unified_diff` joined with `''.join(...)`, as used by
/// `ShellFileOperations._unified_diff` (n=3, lineterm=`\n`, keepends inputs).
pub fn unified_diff(old_content: &str, new_content: &str, fromfile: &str, tofile: &str) -> String {
    let a = split_lines_keepends(old_content);
    let b = split_lines_keepends(new_content);
    let sm = SequenceMatcher::new(&a, &b);
    let mut out = String::new();
    let mut started = false;
    for group in sm.get_grouped_opcodes(3) {
        if !started {
            started = true;
            out.push_str(&format!("--- {}\n", fromfile));
            out.push_str(&format!("+++ {}\n", tofile));
        }
        let first = group[0];
        let last = group[group.len() - 1];
        let file1_range = format_range_unified(first.1, last.2);
        let file2_range = format_range_unified(first.3, last.4);
        out.push_str(&format!("@@ -{} +{} @@\n", file1_range, file2_range));
        for (tag, i1, i2, j1, j2) in group {
            match tag {
                Tag::Equal => {
                    for line in &a[i1..i2] {
                        out.push(' ');
                        out.push_str(line);
                    }
                }
                Tag::Replace => {
                    for line in &a[i1..i2] {
                        out.push('-');
                        out.push_str(line);
                    }
                    for line in &b[j1..j2] {
                        out.push('+');
                        out.push_str(line);
                    }
                }
                Tag::Delete => {
                    for line in &a[i1..i2] {
                        out.push('-');
                        out.push_str(line);
                    }
                }
                Tag::Insert => {
                    for line in &b[j1..j2] {
                        out.push('+');
                        out.push_str(line);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_matches_cpython_vectors() {
        // Vectors computed with CPython 3.12 difflib.
        assert!((ratio_chars("abcd", "bcde") - 0.75).abs() < 1e-12);
        assert!((ratio_chars("", "") - 1.0).abs() < 1e-12);
        assert!((ratio_chars("abc", "") - 0.0).abs() < 1e-12);
        // SequenceMatcher(None, "private_thing", "private_stuff").ratio() == 0.6923076923076923
        assert!((ratio_chars("private_thing", "private_stuff") - 0.6923076923076923).abs() < 1e-12);
        // "qabxcd" vs "abycdf" == 0.6666666666666666
        assert!((ratio_chars("qabxcd", "abycdf") - 0.6666666666666666).abs() < 1e-12);
    }

    #[test]
    fn opcodes_match_cpython() {
        let a: Vec<char> = "qabxcd".chars().collect();
        let b: Vec<char> = "abycdf".chars().collect();
        let sm = SequenceMatcher::new(&a, &b);
        let ops = sm.get_opcodes();
        // CPython: delete(0,1,0,0) equal(1,3,0,2) replace(3,4,2,3) equal(4,6,3,5) insert(6,6,5,6)
        assert_eq!(
            ops,
            vec![
                (Tag::Delete, 0, 1, 0, 0),
                (Tag::Equal, 1, 3, 0, 2),
                (Tag::Replace, 3, 4, 2, 3),
                (Tag::Equal, 4, 6, 3, 5),
                (Tag::Insert, 6, 6, 5, 6),
            ]
        );
    }

    #[test]
    fn unified_diff_shape() {
        let old = "one\ntwo\nthree\n";
        let new = "one\n2\nthree\n";
        let d = unified_diff(old, new, "a/f.txt", "b/f.txt");
        assert!(d.starts_with("--- a/f.txt\n+++ b/f.txt\n@@ -1,3 +1,3 @@\n"));
        assert!(d.contains(" one\n-two\n+2\n three\n"));
    }

    #[test]
    fn split_keepends() {
        assert_eq!(split_lines_keepends("a\nb"), vec!["a\n", "b"]);
        assert_eq!(split_lines_keepends("a\r\nb\n"), vec!["a\r\n", "b\n"]);
        assert_eq!(split_lines_keepends(""), Vec::<&str>::new());
    }
}
