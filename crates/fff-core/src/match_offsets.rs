use smallvec::SmallVec;

pub(crate) fn char_indices_to_byte_offsets(
    line: &str,
    char_indices: &[u32],
) -> SmallVec<[(u32, u32); 4]> {
    debug_assert!(char_indices.windows(2).all(|pair| pair[0] <= pair[1]));
    let mut result: SmallVec<[(u32, u32); 4]> = SmallVec::new();
    let mut chars = line.char_indices().enumerate().peekable();

    for &index in char_indices {
        while chars.peek().is_some_and(|&(i, _)| i < index as usize) {
            chars.next();
        }
        let Some(&(_, (start, ch))) = chars.peek() else {
            break;
        };
        let end = (start + ch.len_utf8()) as u32;
        if let Some(last) = result.last_mut()
            && last.1 == start as u32
        {
            last.1 = end;
        } else {
            result.push((start as u32, end));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_character_ranges() {
        for line in ["", "abcdefghij", "aé文🦀e\u{301}z", "é🦀a文bc"] {
            let ranges: Vec<_> = line
                .char_indices()
                .map(|(start, ch)| (start as u32, (start + ch.len_utf8()) as u32))
                .collect();
            for mask in 0..1 << (ranges.len() + 1) {
                let indices: Vec<_> = (0..=ranges.len())
                    .filter(|i| mask & (1 << i) != 0)
                    .map(|i| i as u32)
                    .collect();
                let actual = char_indices_to_byte_offsets(line, &indices);
                let mut expected: Vec<(u32, u32)> = Vec::new();
                for &index in &indices {
                    let Some(&(start, end)) = ranges.get(index as usize) else {
                        continue;
                    };
                    if let Some(last) = expected.last_mut()
                        && last.1 == start
                    {
                        last.1 = end;
                    } else {
                        expected.push((start, end));
                    }
                }
                assert_eq!(actual.as_slice(), expected, "{line:?}, {indices:?}");
            }
        }
    }

    #[test]
    fn contiguous_matches_stay_inline() {
        let indices: Vec<_> = (0..100).collect();
        let ranges = char_indices_to_byte_offsets(&"x".repeat(100), &indices);
        assert_eq!(ranges.as_slice(), &[(0, 100)]);
        assert!(!ranges.spilled());
    }

    #[test]
    fn duplicate_and_out_of_bounds_indices() {
        assert_eq!(
            char_indices_to_byte_offsets("aé🦀", &[0, 0, 1, 2, 3, u32::MAX]).as_slice(),
            &[(0, 1), (0, 7)]
        );
    }
}
