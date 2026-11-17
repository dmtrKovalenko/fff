pub enum Verdict {
    Slower,
    Faster,
    Within,
    New(f64),
    Removed(f64),
}

pub struct Timing {
    pub name: String,
    base: Option<f64>,
    head: Option<f64>,
}

impl Timing {
    /// PR time against base in percent, `+12.0` is 12% slower.
    pub fn change(&self) -> Option<f64> {
        let base = self.base?;
        Some(100.0 * (self.head? - base) / base)
    }

    /// Same 10% limits as `perf/report.sh`.
    pub fn verdict(&self) -> Verdict {
        match (self.base, self.head) {
            (None, Some(ns)) => Verdict::New(ns),
            (Some(ns), None) => Verdict::Removed(ns),
            (Some(b), Some(h)) if 10.0 * h > 11.0 * b => Verdict::Slower,
            (Some(b), Some(h)) if 10.0 * h < 9.0 * b => Verdict::Faster,
            _ => Verdict::Within,
        }
    }
}

pub fn parse(base: &str, head: &str) -> Vec<Timing> {
    let mut base: std::collections::BTreeMap<_, _> = base.lines().filter_map(line).collect();
    let mut timings: Vec<_> = head
        .lines()
        .filter_map(line)
        .map(|(name, head)| Timing {
            name: name.to_owned(),
            base: base.remove(name),
            head: Some(head),
        })
        .collect();
    timings.extend(base.into_iter().map(|(name, base)| Timing {
        name: name.to_owned(),
        base: Some(base),
        head: None,
    }));
    timings
}

// "test <name> ... bench: <ns> ns/iter (+/- <n>)"
fn line(line: &str) -> Option<(&str, f64)> {
    let mut fields = line.split_whitespace();
    let name = fields.nth(1)?;
    let value: u64 = fields.nth(2)?.parse().ok()?;
    (value > 0).then_some((name, value as f64))
}
