//! Server-rendered SVG charts.

use maud::{Markup, html};

use super::es::dec;

pub struct Point {
    pub label: String,
    pub link: String,
    pub x: f64,
    pub y: f64,
}

const WIDTH: f64 = 960.0;
const HEIGHT: f64 = 520.0;
const LEFT: f64 = 56.0;
const RIGHT: f64 = 24.0;
const TOP: f64 = 48.0;
const BOTTOM: f64 = 52.0;
const RADIUS: f64 = 5.0;
const CHAR_WIDTH: f64 = 6.4;
const LABEL_HEIGHT: f64 = 13.0;
const CORNER_CHAR_WIDTH: f64 = 7.8;

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n == 0 {
        0.5
    } else if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Rect {
    fn overlaps(&self, other: &Rect) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    fn inside(&self, bounds: &Rect) -> bool {
        self.x0 >= bounds.x0 && self.x1 <= bounds.x1 && self.y0 >= bounds.y0 && self.y1 <= bounds.y1
    }
}

struct Label {
    x: f64,
    y: f64,
    anchor: &'static str,
}

/// Lowest efficiency shown, rounded down to a 5% step so the lowest point
/// keeps some room below it.
fn y_floor(points: &[Point]) -> f64 {
    let min = points.iter().map(|p| p.y).fold(1.0_f64, f64::min);
    ((min - 0.02) * 20.0).floor() / 20.0
}

fn y_ticks(floor: f64) -> Vec<f64> {
    let step = if 1.0 - floor > 0.3 { 0.1 } else { 0.05 };
    let mut ticks = Vec::new();
    let mut t = 1.0;
    while t >= floor - 1e-9 {
        ticks.push(t);
        t -= step;
    }
    ticks
}

/// Places each label next to its point where it covers no other label or
/// point; a label that fits nowhere is shown only on hover.
fn place_labels(
    points: &[(f64, f64, &str)],
    bounds: Rect,
    reserved: &[Rect],
) -> Vec<Option<Label>> {
    let dots: Vec<Rect> = points
        .iter()
        .map(|&(x, y, _)| Rect {
            x0: x - RADIUS,
            y0: y - RADIUS,
            x1: x + RADIUS,
            y1: y + RADIUS,
        })
        .collect();
    let crowd = |i: usize| {
        points
            .iter()
            .filter(|p| (p.0 - points[i].0).abs() < 80.0 && (p.1 - points[i].1).abs() < 20.0)
            .count()
    };
    // Isolated points choose first, so crowded clusters lose labels before they do.
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by_key(|&i| (crowd(i), i));
    let mut taken: Vec<Rect> = reserved.to_vec();
    let mut labels: Vec<Option<Label>> = (0..points.len()).map(|_| None).collect();
    for i in order {
        let (x, y, text) = points[i];
        let w = text.chars().count() as f64 * CHAR_WIDTH;
        let r = RADIUS;
        let candidate = |lx: f64, ly: f64, anchor: &'static str| {
            let left = match anchor {
                "start" => lx - 1.0,
                "end" => lx - w - 1.0,
                _ => lx - w / 2.0,
            };
            let rect = Rect {
                x0: left,
                y0: ly - LABEL_HEIGHT + 3.0,
                x1: left + w + 2.0,
                y1: ly + 3.0,
            };
            (lx, ly, anchor, rect)
        };
        let candidates = [
            candidate(x + r + 4.0, y + 4.0, "start"),
            candidate(x - r - 4.0, y + 4.0, "end"),
            candidate(x, y - r - 5.0, "middle"),
            candidate(x, y + r + 13.0, "middle"),
            candidate(x + r, y - r - 3.0, "start"),
            candidate(x + r, y + r + 11.0, "start"),
            candidate(x - r, y - r - 3.0, "end"),
            candidate(x - r, y + r + 11.0, "end"),
        ];
        for (lx, ly, anchor, rect) in candidates {
            let free = rect.inside(&bounds)
                && !taken.iter().any(|t| t.overlaps(&rect))
                && !dots
                    .iter()
                    .enumerate()
                    .any(|(j, d)| j != i && d.overlaps(&rect));
            if free {
                taken.push(rect);
                labels[i] = Some(Label {
                    x: lx,
                    y: ly,
                    anchor,
                });
                break;
            }
        }
    }
    labels
}

/// Impact (x) against efficiency (y), split at the medians of the plotted
/// points. Labels describe patterns, not ratings.
pub fn quadrant(points: &[Point]) -> Markup {
    let y_min = y_floor(points).clamp(0.0, 0.9);
    let (x0, x1, y0, y1) = (LEFT, WIDTH - RIGHT, TOP, HEIGHT - BOTTOM);
    let x_of = |x: f64| x0 + x * (x1 - x0);
    let y_of = |y: f64| y1 - (y - y_min) / (1.0 - y_min).max(f64::EPSILON) * (y1 - y0);
    let mx = x_of(median(&mut points.iter().map(|p| p.x).collect::<Vec<_>>()));
    let my = y_of(median(&mut points.iter().map(|p| p.y).collect::<Vec<_>>()));

    let corners = [
        ("Cambios acotados, poco churn", false, true),
        ("Cambios amplios, poco churn", true, true),
        ("Cambios acotados, más churn", false, false),
        ("Cambios amplios, más churn", true, false),
    ];
    let reserved: Vec<Rect> = corners
        .iter()
        .map(|&(text, right, top)| {
            let w = text.chars().count() as f64 * CORNER_CHAR_WIDTH;
            let x = if right { x1 - w } else { x0 };
            // Top captions sit above the plot, where the busiest band of points is.
            let y = if top {
                y0 - 9.0 - LABEL_HEIGHT
            } else {
                y1 - 6.0 - LABEL_HEIGHT
            };
            Rect {
                x0: x,
                y0: y,
                x1: x + w,
                y1: y + LABEL_HEIGHT,
            }
        })
        .collect();
    let placed: Vec<(f64, f64, &str)> = points
        .iter()
        .map(|p| (x_of(p.x), y_of(p.y), p.label.as_str()))
        .collect();
    let bounds = Rect {
        x0,
        y0,
        x1: WIDTH - 2.0,
        y1,
    };
    let labels = place_labels(&placed, bounds, &reserved);

    html! {
        svg.quadrant-chart viewBox=(format!("0 0 {WIDTH} {HEIGHT}")) role="img"
            aria-label="Impacto frente a eficiencia por persona" {
            rect.plot x=(x0) y=(y0) width=(x1 - x0) height=(y1 - y0) {}
            rect.zone x=(x0) y=(y0) width=(mx - x0) height=(my - y0) {}
            rect.zone x=(mx) y=(my) width=(x1 - mx) height=(y1 - my) {}
            @for t in y_ticks(y_min) {
                line.grid x1=(x0) y1=(y_of(t)) x2=(x1) y2=(y_of(t)) {}
                text.tick x=(x0 - 8.0) y=(y_of(t) + 4.0) text-anchor="end" { (format!("{:.0}%", t * 100.0)) }
            }
            @for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                line.grid x1=(x_of(t)) y1=(y0) x2=(x_of(t)) y2=(y1) {}
                text.tick x=(x_of(t)) y=(y1 + 16.0) text-anchor="middle" { (dec(t, 2)) }
            }
            line.median x1=(mx) y1=(y0) x2=(mx) y2=(y1) {}
            line.median x1=(x0) y1=(my) x2=(x1) y2=(my) {}
            @for (i, (text, _, _)) in corners.iter().enumerate() {
                text.corner x=(reserved[i].x0) y=(reserved[i].y1 - 3.0) { (text) }
            }
            text.axis-title x=((x0 + x1) / 2.0) y=(HEIGHT - 10.0) text-anchor="middle" { "Impacto (relativo a la selección)" }
            text.axis-title x="14" y=((y0 + y1) / 2.0) transform=(format!("rotate(-90 14 {})", (y0 + y1) / 2.0)) text-anchor="middle" {
                "Eficiencia"
            }
            @for (point, label) in points.iter().zip(&labels) {
                a.dot href=(point.link) {
                    title { (format!("{}: impacto {}, eficiencia {:.0}%", point.label, dec(point.x, 2), point.y * 100.0)) }
                    circle cx=(x_of(point.x)) cy=(y_of(point.y)) r=(RADIUS) {}
                    @match label {
                        Some(l) => text.label x=(l.x) y=(l.y) text-anchor=(l.anchor) { (point.label) },
                        None => text.label.hover x=(x_of(point.x) + RADIUS + 4.0) y=(y_of(point.y) - 8.0) { (point.label) },
                    }
                }
            }
        }
    }
}

/// Work types and their colour slots, in the fixed order used on every page.
pub const WORK: [(&str, &str); 4] = [
    ("Trabajo nuevo", "s1"),
    ("Refactor de legacy", "s2"),
    ("Ayuda a otros", "s3"),
    ("Churn", "s4"),
];

/// Pull request stages and their colour slots.
pub const STAGES: [(&str, &str); 3] = [("Desarrollo", "s1"), ("Espera", "s2"), ("Revisión", "s3")];

pub struct Segment {
    pub label: &'static str,
    pub class: &'static str,
    pub value: f64,
    /// The value as shown in the tooltip.
    pub text: String,
}

/// One horizontal bar split in proportion to the segment values.
pub fn stack(segments: &[Segment]) -> Markup {
    let total: f64 = segments.iter().map(|s| s.value).sum();
    let summary = segments
        .iter()
        .map(|s| format!("{} {}", s.label, s.text))
        .collect::<Vec<_>>()
        .join(", ");
    html! {
        div.stack role="img" aria-label=(summary) {
            @if total > 0.0 {
                @for s in segments.iter().filter(|s| s.value > 0.0) {
                    span class=(s.class) style=(format!("flex-grow:{:.4}", s.value / total))
                        title=(format!("{}: {} ({:.0}%)", s.label, s.text, s.value / total * 100.0)) {}
                }
            }
        }
    }
}

pub fn legend(items: &[(&str, &str)]) -> Markup {
    html! {
        div.legend {
            @for (label, class) in items {
                span { i class=(class) {} (label) }
            }
        }
    }
}

/// Axis value in short form: 1500 as "1.5k".
fn compact(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{}M", (value / 100_000.0).round() / 10.0).replace('.', ",")
    } else if value >= 1000.0 {
        format!("{}k", (value / 100.0).round() / 10.0).replace('.', ",")
    } else {
        format!("{value:.0}")
    }
}

fn nice_ceiling(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(value.log10().floor());
    [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .iter()
        .map(|m| m * magnitude)
        .find(|step| *step >= value)
        .unwrap_or(10.0 * magnitude)
}

/// Stacked bars of changed lines by work type, one bar per period.
pub fn stacked_bars(periods: &[(String, [u64; 4])]) -> Markup {
    const W: f64 = 960.0;
    const H: f64 = 240.0;
    const L: f64 = 56.0;
    const B: f64 = 28.0;
    const T: f64 = 12.0;
    let max = periods
        .iter()
        .map(|(_, v)| v.iter().sum::<u64>())
        .max()
        .unwrap_or(0) as f64;
    let top = nice_ceiling(max);
    let band = (W - L - 8.0) / periods.len().max(1) as f64;
    let bar = (band * 0.62).min(56.0);
    let y_of = |v: f64| H - B - v / top * (H - B - T);
    let x_of = |i: usize| L + band * i as f64 + (band - bar) / 2.0;
    let mut rects = Vec::new();
    for (i, (label, values)) in periods.iter().enumerate() {
        let mut below = 0.0;
        for (j, value) in values.iter().enumerate().filter(|(_, v)| **v > 0) {
            let v = *value as f64;
            let (y0, y1) = (y_of(below + v), y_of(below));
            // A 2px gap keeps stacked segments apart without a stroke.
            rects.push((
                WORK[j].1,
                x_of(i),
                y0,
                (y1 - y0 - 2.0).max(1.0),
                format!("{label} · {}: {value} líneas", WORK[j].0),
            ));
            below += v;
        }
    }
    html! {
        svg.bars viewBox=(format!("0 0 {W} {H}")) role="img" aria-label="Líneas cambiadas por semana según tipo de trabajo" {
            @for t in [0.0, 0.5, 1.0] {
                line.grid x1=(L) y1=(y_of(top * t)) x2=(W - 8.0) y2=(y_of(top * t)) {}
                text.tick x=(L - 8.0) y=(y_of(top * t) + 4.0) text-anchor="end" { (compact(top * t)) }
            }
            @for (class, x, y, height, title) in &rects {
                rect class=(class) x=(x) y=(y) width=(bar) height=(height) rx="2" { title { (title) } }
            }
            @for (i, (label, _)) in periods.iter().enumerate() {
                text.tick x=(x_of(i) + bar / 2.0) y=(H - 8.0) text-anchor="middle" { (label) }
            }
        }
    }
}
