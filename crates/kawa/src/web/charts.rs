//! Server-rendered SVG charts.

use maud::{Markup, html};

pub struct Point {
    pub label: String,
    pub link: String,
    pub x: f64,
    pub y: f64,
}

const WIDTH: f64 = 720.0;
const HEIGHT: f64 = 420.0;
const PAD: f64 = 48.0;

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

/// Impact (x) against efficiency (y), split at the medians of the plotted
/// points. Labels describe patterns, not ratings.
pub fn quadrant(points: &[Point]) -> Markup {
    let y_min = points
        .iter()
        .map(|p| p.y)
        .fold(1.0_f64, f64::min)
        .clamp(0.0, 0.9);
    let x_of = |x: f64| PAD + x * (WIDTH - 2.0 * PAD);
    let y_of = |y: f64| {
        HEIGHT - PAD - (y - y_min) / (1.0 - y_min).max(f64::EPSILON) * (HEIGHT - 2.0 * PAD)
    };
    let mx = median(&mut points.iter().map(|p| p.x).collect::<Vec<_>>());
    let my = median(&mut points.iter().map(|p| p.y).collect::<Vec<_>>());
    let (left, right, top, bottom) = (PAD + 6.0, WIDTH - PAD - 6.0, PAD + 14.0, HEIGHT - PAD - 8.0);
    html! {
        svg viewBox=(format!("0 0 {WIDTH} {HEIGHT}")) width=(WIDTH) height=(HEIGHT) role="img"
            aria-label="Impact against efficiency per developer" {
            line.axis x1=(PAD) y1=(HEIGHT - PAD) x2=(WIDTH - PAD) y2=(HEIGHT - PAD) {}
            line.axis x1=(PAD) y1=(PAD) x2=(PAD) y2=(HEIGHT - PAD) {}
            line.median x1=(x_of(mx)) y1=(PAD) x2=(x_of(mx)) y2=(HEIGHT - PAD) {}
            line.median x1=(PAD) y1=(y_of(my)) x2=(WIDTH - PAD) y2=(y_of(my)) {}
            text.quadrant x=(left) y=(top) { "Focused changes, little churn" }
            text.quadrant x=(right) y=(top) text-anchor="end" { "Broad changes, little churn" }
            text.quadrant x=(left) y=(bottom) { "Focused changes, more churn" }
            text.quadrant x=(right) y=(bottom) text-anchor="end" { "Broad changes, more churn" }
            text x=(WIDTH / 2.0) y=(HEIGHT - 12.0) text-anchor="middle" { "Impact (relative to the selection)" }
            text x="14" y=(HEIGHT / 2.0) transform=(format!("rotate(-90 14 {})", HEIGHT / 2.0)) text-anchor="middle" {
                "Efficiency"
            }
            text x=(PAD) y=(HEIGHT - PAD + 16.0) text-anchor="middle" { "0" }
            text x=(WIDTH - PAD) y=(HEIGHT - PAD + 16.0) text-anchor="middle" { "1" }
            text x=(PAD - 6.0) y=(y_of(1.0) + 4.0) text-anchor="end" { "100%" }
            text x=(PAD - 6.0) y=(y_of(y_min) + 4.0) text-anchor="end" { (format!("{:.0}%", y_min * 100.0)) }
            @for point in points {
                a href=(point.link) {
                    circle cx=(x_of(point.x)) cy=(y_of(point.y)) r="5" {
                        title { (format!("{}: impact {:.2}, efficiency {:.0}%", point.label, point.x, point.y * 100.0)) }
                    }
                    text x=(x_of(point.x) + 8.0) y=(y_of(point.y) + 4.0) { (point.label) }
                }
            }
        }
    }
}
