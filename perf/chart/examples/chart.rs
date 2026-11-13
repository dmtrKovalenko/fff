use fff_perf_chart::{Timing, Verdict, parse};
use fframes::{
    AudioMap, Color, CpuFrameRenderer, Duration, FFramesContext, Frame, Previewer, RenderOptions,
    StaticMediaProvider, Svgr, Video, include_media_dir, svgr,
};
use std::process::ExitCode;

include_media_dir!(struct ChartMedia, "media");

const WIDTH: usize = 1280;
const HEIGHT: usize = 6000;
// GitHub shows the image about 850 px wide.
const SCALE: f64 = 1.5;

const MONO: &str = "IBM Plex Mono";
const ADVANCE: f32 = 0.6;

const PANEL: &str = "#fafafa";
const RAIL: &str = "#d8d8de";
const EDGE: &str = "#a8a8b2";
const DIM: &str = "#74747e";
const INK: &str = "#18181b";
const GREEN: &str = "#17a34a";
const RED: &str = "#e5484d";
const BLUE: &str = "#2563b0";

const MARGIN: f32 = 60.0;
const LABEL_X: f32 = 84.0;
const LABEL_CHARS: usize = 46;
const TRACK: (f32, f32) = (620.0, 1080.0);
const CHANGE_X: f32 = 1196.0;
const FIRST_GROUP_Y: f32 = 196.0;
const ROW: f32 = 30.0;
const BOX_PAD: f32 = 16.0;
const GROUP_GAP: f32 = 34.0;

struct Group {
    name: String,
    timings: Vec<Timing>,
}

struct Chart {
    groups: Vec<Group>,
    // Percent at both ends of the track.
    span: f32,
}

impl Chart {
    fn new(timings: Vec<Timing>) -> Self {
        let worst = timings
            .iter()
            .filter_map(Timing::change)
            .map(f64::abs)
            .fold(0.0, f64::max);
        let span = (worst / 10.0).ceil().clamp(2.0, 10.0) as f32 * 10.0;
        let mut groups: Vec<Group> = Vec::new();
        for timing in timings {
            let name = group_name(&timing.name);
            match groups.iter_mut().find(|group| group.name == name) {
                Some(group) => group.timings.push(timing),
                _ => groups.push(Group {
                    name,
                    timings: vec![timing],
                }),
            }
        }
        Self { groups, span }
    }

    fn height(&self) -> f32 {
        let boxes: f32 = self
            .groups
            .iter()
            .map(|group| group.height() + GROUP_GAP)
            .sum();
        FIRST_GROUP_Y + boxes - GROUP_GAP + MARGIN
    }

    fn x(&self, change: f32) -> f32 {
        let middle = (TRACK.0 + TRACK.1) / 2.0;
        middle + change.clamp(-self.span, self.span) / self.span * (TRACK.1 - TRACK.0) / 2.0
    }

    fn header(&self) -> Svgr<'static> {
        let all = || self.groups.iter().flat_map(|group| &group.timings);
        let slower = all()
            .filter(|t| matches!(t.verdict(), Verdict::Slower))
            .count();
        let faster = all()
            .filter(|t| matches!(t.verdict(), Verdict::Faster))
            .count();
        let within = all()
            .filter(|t| matches!(t.verdict(), Verdict::Within))
            .count();
        let mut x = MARGIN;
        let legend: Vec<_> = [
            (RED, format!("{slower} slower")),
            (GREEN, format!("{faster} faster")),
            (EDGE, format!("{within} within ±10%")),
        ]
        .into_iter()
        .map(|(fill, content)| {
            let at = x;
            x += 40.0 + (content.chars().count() + 3) as f32 * ADVANCE * 18.0;
            svgr!(
                <g>
                    <rect x={at} y="103" width="28" height="18" rx="4" fill={fill} />
                    <text x={at + 40.0} y="118" font-size="18" fill={DIM}>{content}</text>
                </g>
            )
        })
        .collect();
        let ticks: Vec<_> = [
            (-self.span, 0.0, "middle", format!("-{}%", self.span), DIM),
            (0.0, 4.0, "end", "base".to_owned(), DIM),
            (10.0, -4.0, "start", "limit".to_owned(), RED),
            (self.span, 0.0, "middle", format!("+{}%", self.span), DIM),
        ]
        .into_iter()
        .map(|(change, shift, anchor, content, fill)| {
            svgr!(
                <text x={self.x(change) + shift} y="170" font-size="18" text-anchor={anchor}
                    fill={fill}>
                    {content}
                </text>
            )
        })
        .collect();
        svgr!(
            <g>
                <text x={MARGIN} y="78" font-size="22" font-weight="600" fill={INK}>
                    "FFF core performance"
                </text>
                <text x={MARGIN + 21.0 * ADVANCE * 22.0} y="78" font-size="22" fill={DIM}>
                    "PR time against base"
                </text>
                {legend}
                {ticks}
            </g>
        )
    }

    fn group(&self, group: &Group, y: f32) -> Svgr<'static> {
        let top = y + 38.0;
        let bottom = top + group.height() - 38.0;
        let slower = group.count(|verdict| matches!(verdict, Verdict::Slower));
        let tail = match slower {
            0 => format!("{} benchmarks", group.timings.len()),
            _ => format!("{} benchmarks, {slower} slower", group.timings.len()),
        };
        let tail_x = MARGIN + (group.name.chars().count() + 1) as f32 * ADVANCE * 22.0;
        let rows: Vec<_> = group
            .timings
            .iter()
            .enumerate()
            .map(|(index, timing)| self.row(timing, top + BOX_PAD + ROW * (index as f32 + 0.5)))
            .collect();
        svgr!(
            <g>
                <text x={MARGIN} y={y + 22.0} font-size="22" font-weight="600" fill={INK}>
                    {group.name.clone()}
                </text>
                <text x={tail_x} y={y + 22.0} font-size="22" fill={DIM}>{tail}</text>
                <rect
                    x={MARGIN}
                    y={top}
                    width={WIDTH as f32 - 2.0 * MARGIN}
                    height={bottom - top}
                    rx="18"
                    fill={PANEL}
                    stroke={EDGE}
                    stroke-width="2"
                />
                <line x1={self.x(0.0)} y1={top + 8.0} x2={self.x(0.0)} y2={bottom - 8.0}
                    stroke={EDGE} stroke-width="2" />
                <line x1={self.x(10.0)} y1={top + 8.0} x2={self.x(10.0)} y2={bottom - 8.0}
                    stroke={RED} stroke-width="2" stroke-dasharray="3 6" />
                {rows}
            </g>
        )
    }

    fn row(&self, timing: &Timing, y: f32) -> Svgr<'static> {
        let (fill, text, weight) = match timing.verdict() {
            Verdict::Slower => (RED, INK, 600),
            Verdict::Faster => (GREEN, INK, 600),
            Verdict::Within => (EDGE, DIM, 400),
            Verdict::New(ns) => return self.unpaired_row(timing, y, BLUE, "NEW", ns),
            Verdict::Removed(ns) => return self.unpaired_row(timing, y, DIM, "REMOVED", ns),
        };
        let change = timing.change().unwrap() as f32;
        let (from, to) = (self.x(change.min(0.0)), self.x(change.max(0.0)));
        svgr!(
            <g font-size="18">
                <text x={LABEL_X} y={y + 6.0} font-weight={weight} fill={text}>
                    {label(&timing.name)}
                </text>
                <line x1={TRACK.0} y1={y} x2={TRACK.1} y2={y} stroke={RAIL} stroke-width="2.5"
                    stroke-linecap="round" />
                <rect x={from} y={y - 6.0} width={(to - from).max(3.0)} height="12" rx="4"
                    fill={fill} />
                <text x={CHANGE_X} y={y + 6.0} text-anchor="end" font-weight={weight}
                    fill={if weight == 600 { fill } else { DIM }}>
                    {format!("{change:+.1}%")}
                </text>
            </g>
        )
    }

    fn unpaired_row(
        &self,
        timing: &Timing,
        y: f32,
        color: &'static str,
        status: &'static str,
        ns: f64,
    ) -> Svgr<'static> {
        let elapsed = std::time::Duration::from_secs_f64(ns / 1e9);
        svgr!(
            <g font-size="18" fill={color}>
                <text x={LABEL_X} y={y + 6.0}>{label(&timing.name)}</text>
                <text x={TRACK.0} y={y + 6.0} font-weight="600">{status.to_owned()}</text>
                <text x={CHANGE_X} y={y + 6.0} text-anchor="end">{format!("{elapsed:.2?}")}</text>
            </g>
        )
    }
}

impl Group {
    fn height(&self) -> f32 {
        38.0 + 2.0 * BOX_PAD + ROW * self.timings.len() as f32
    }

    fn count(&self, matches: impl Fn(Verdict) -> bool) -> usize {
        self.timings.iter().filter(|t| matches(t.verdict())).count()
    }
}

impl Video for Chart {
    const FPS: usize = 1;
    const WIDTH: usize = WIDTH;
    const HEIGHT: usize = HEIGHT;
    const BACKGROUND_COLOR: Color = Color::WHITE;

    fn duration(&self) -> Duration<'_> {
        Duration::Frames(1)
    }

    fn audio(&self) -> AudioMap<'_> {
        AudioMap::none()
    }

    fn render_frame<'a>(&'a self, _frame: Frame, _ctx: &FFramesContext<'a, '_>) -> Svgr<'a> {
        let mut y = FIRST_GROUP_Y;
        let groups: Vec<_> = self
            .groups
            .iter()
            .map(|group| {
                let at = y;
                y += group.height() + GROUP_GAP;
                self.group(group, at)
            })
            .collect();
        svgr!(
            <svg xmlns="http://www.w3.org/2000/svg" width={WIDTH} height={HEIGHT}
                font-family={MONO} font-weight="400">
                {self.header()}
                {groups}
            </svg>
        )
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [base, head, output] = args.as_slice() else {
        eprintln!("usage: chart <base.bench> <head.bench> <output.png>");
        return ExitCode::from(2);
    };
    match render(base, head, output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn render(base: &str, head: &str, output: &str) -> Result<(), Box<dyn std::error::Error>> {
    let timings = parse(
        &std::fs::read_to_string(base)?,
        &std::fs::read_to_string(head)?,
    );
    if timings.is_empty() {
        return Err("no benchmark timings".into());
    }
    let chart = Chart::new(timings);
    let media = ChartMedia::prepare()?;
    let options = RenderOptions {
        media: Some(&media),
        scale_resolution: SCALE,
        default_font: MONO,
        ..Default::default()
    };
    let frame = Previewer::new(&chart, &options)?.render(0, &mut CpuFrameRenderer::new(16))?;
    let height = (chart.height().min(HEIGHT as f32) as f64 * SCALE) as u32;
    frame.crop(0, 0, frame.width, height).save_png(output)?;
    Ok(())
}

// "ext4/fff" for the repository lanes, "core" for the rest.
fn group_name(name: &str) -> String {
    match name.split_once('/') {
        Some(("core", _)) | None => "core".to_owned(),
        Some((fs, rest)) => format!("{fs}/{}", rest.split('/').next().unwrap_or_default()),
    }
}

// The name without its group and without the benchmark binary.
fn label(name: &str) -> String {
    let skip = if name.starts_with("core/") { 2 } else { 3 };
    let label = name.split('/').skip(skip).collect::<Vec<_>>().join("/");
    if label.chars().count() <= LABEL_CHARS {
        return label;
    }
    let tail: String = label
        .chars()
        .skip(label.chars().count() - LABEL_CHARS + 1)
        .collect();
    format!("…{tail}")
}
