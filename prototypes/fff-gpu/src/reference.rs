use neo_frizbee::Scoring;

const LANES: usize = 16;

// Scalar twin of score.wgsl; used to validate the shader independently of frizbee.
pub fn score(needle: &[u8], hay: &[u8], s: &Scoring) -> u32 {
    let n = needle.len().min(32);
    let needle = &needle[..n];
    let Some((win_start, win_end)) = prefilter_window(needle, hay) else {
        return 0;
    };

    let match_score = (s.match_score + s.mismatch_penalty) as i32;
    let mismatch = s.mismatch_penalty as i32;
    let gap_open = (s.gap_open_penalty - s.gap_extend_penalty) as i32;
    let gap_extend = s.gap_extend_penalty as i32;
    let mcb = s.matching_case_bonus as i32;

    let mut adj = vec![0i32; (n + 1) * LANES];
    let mut adj_mm = vec![false; (n + 1) * LANES];
    let mut best = 0;
    let mut prev_is_lower = false;
    let mut prev_is_delim = false;
    let mut exact_whole = hay.len() == n;

    for base in (win_start..win_end).step_by(LANES) {
        let mut chars = [0u8; LANES];
        let mut bonus = [0i32; LANES];
        for l in 0..LANES {
            let c = hay
                .get(base + l)
                .copied()
                .filter(|_| base + l < win_end)
                .unwrap_or(0);
            chars[l] = c;
            let is_delim = !(c.is_ascii_alphanumeric() || c > 127);
            let mut b = match_score;
            if c.is_ascii_uppercase() && prev_is_lower {
                b += s.capitalization_bonus as i32;
            }
            if prev_is_delim && !is_delim {
                b += s.delimiter_bonus as i32;
            }
            if base + l == 0 {
                b += s.prefix_bonus as i32;
            }
            bonus[l] = b;
            prev_is_lower = c.is_ascii_lowercase();
            prev_is_delim = is_delim;
            if exact_whole
                && base + l < hay.len()
                && c != needle[base + l]
                && flip_case(c) != needle[base + l]
            {
                exact_whole = false;
            }
        }

        let mut prev_row = [0i32; LANES];
        let mut up_mm = [false; LANES];
        let mut row = [0i32; LANES];
        let mut mm = [false; LANES];
        for i in 1..=n {
            let nc = needle[i - 1];
            let fc = flip_case(nc);
            let ai = i * LANES;
            let diag_adj = adj[(i - 1) * LANES + LANES - 1];
            for l in 0..LANES {
                let c = chars[l];
                let exact = c == nc;
                let m = exact || c == fc;
                let mut d = if l > 0 { prev_row[l - 1] } else { diag_adj };
                if m {
                    d += bonus[l];
                }
                d = (d - mismatch).max(0);
                if exact {
                    d += mcb;
                }
                let mut u = prev_row[l] - gap_extend;
                if up_mm[l] {
                    u -= gap_open;
                }
                row[l] = d.max(u.max(0));
                mm[l] = m;
            }

            let mut g = gap_extend;
            let mut shift = 1;
            while shift < LANES {
                let mut tmp = [0i32; LANES];
                for l in 0..LANES {
                    let (src, src_mm) = if l >= shift {
                        (row[l - shift], mm[l - shift])
                    } else {
                        (adj[ai + LANES - shift + l], adj_mm[ai + LANES - shift + l])
                    };
                    let pen = g + if src_mm { gap_open } else { 0 };
                    tmp[l] = row[l].max((src - pen).max(0));
                }
                row = tmp;
                g *= 2;
                shift *= 2;
            }

            adj[(i - 1) * LANES..i * LANES].copy_from_slice(&prev_row);
            adj_mm[(i - 1) * LANES..i * LANES].copy_from_slice(&up_mm);
            prev_row = row;
            up_mm = mm;
        }
        adj[n * LANES..].copy_from_slice(&row);
        adj_mm[n * LANES..].copy_from_slice(&mm);
        best = best.max(row.iter().copied().max().unwrap_or(0));
    }

    if exact_whole && best > 0 {
        best += s.exact_match_bonus as i32;
    }
    best as u32
}

fn prefilter_window(needle: &[u8], hay: &[u8]) -> Option<(usize, usize)> {
    let eq = |c: u8, nc: u8| c == nc || c == flip_case(nc);
    let mut k = 0;
    let mut start = 0;
    for (j, &c) in hay.iter().enumerate() {
        if k < needle.len() && eq(c, needle[k]) {
            if k == 0 {
                start = j;
            }
            k += 1;
        }
    }
    if k < needle.len() {
        return None;
    }
    let last = *needle.last()?;
    let end = hay.iter().rposition(|&c| eq(c, last))? + 1;
    Some((start, end))
}

fn flip_case(c: u8) -> u8 {
    if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c
    }
}
