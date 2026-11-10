pub enum Verdict {
    Slower,
    Faster,
    Within,
}

pub struct Timing {
    pub name: String,
    pub base: f64,
    pub head: f64,
}

impl Timing {
    /// PR time against base in percent, `+12.0` is 12% slower.
    pub fn change(&self) -> f64 {
        100.0 * (self.head / self.base - 1.0)
    }

    /// Same 10% limits as `perf/report.sh`.
    pub fn verdict(&self) -> Verdict {
        if 10.0 * self.head > 11.0 * self.base {
            Verdict::Slower
        } else if 10.0 * self.head < 9.0 * self.base {
            Verdict::Faster
        } else {
            Verdict::Within
        }
    }
}

/// Pairs the bencher lines of both files by name, in the order of `head`.
pub fn parse(base: &str, head: &str) -> Vec<Timing> {
    let base: Vec<_> = base.lines().filter_map(line).collect();
    head.lines()
        .filter_map(line)
        .filter_map(|(name, head)| {
            let (_, base) = base.iter().find(|(known, _)| *known == name)?;
            Some(Timing {
                name: name.to_owned(),
                base: *base,
                head,
            })
        })
        .collect()
}

// "test <name> ... bench: <ns> ns/iter (+/- <n>)"
fn line(line: &str) -> Option<(&str, f64)> {
    let mut fields = line.split_whitespace();
    let name = fields.nth(1)?;
    let value: f64 = fields.nth(2)?.parse().ok()?;
    (value > 0.0).then_some((name, value))
}
