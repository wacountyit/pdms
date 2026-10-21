// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Server-side chart geometry. Templates emit SVG; colors come from CSS classes.

#[derive(Debug, Clone, PartialEq)]
pub struct VBar {
    pub path: String,
    pub x_mid: f64,
    pub top: f64,
    pub value: i64,
    pub label: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VBars {
    pub width: f64,
    pub height: f64,
    pub baseline: f64,
    pub bars: Vec<VBar>,
    pub aria: String,
}

/// Rounded top corners only (data end), square at the baseline.
fn top_rounded(x: f64, y: f64, w: f64, h: f64, r: f64) -> String {
    if h <= 0.0 {
        return String::new();
    }
    let r = r.min(w / 2.0).min(h);
    format!(
        "M{x:.1},{b:.1}V{y1:.1}Q{x:.1},{y:.1} {x1:.1},{y:.1}H{x2:.1}Q{xw:.1},{y:.1} {xw:.1},{y1:.1}V{b:.1}Z",
        b = y + h,
        y1 = y + r,
        x1 = x + r,
        x2 = x + w - r,
        xw = x + w
    )
}

/// Vertical bars for (label, value, tooltip) with an aria summary prefix.
pub fn vbars(data: &[(String, i64, String)], aria_prefix: &str) -> VBars {
    let (width, height, top_pad, label_h) = (600.0, 220.0, 24.0, 28.0);
    let baseline = height - label_h;
    let max = data.iter().map(|d| d.1).max().unwrap_or(0).max(1) as f64;
    let n = data.len().max(1) as f64;
    let slot = width / n;
    let bar_w = (slot * 0.5).min(48.0);
    let bars = data
        .iter()
        .enumerate()
        .map(|(i, (label, value, title))| {
            let h = (*value as f64 / max) * (baseline - top_pad);
            let x = slot * i as f64 + (slot - bar_w) / 2.0;
            VBar {
                path: top_rounded(x, baseline - h, bar_w, h, 4.0),
                x_mid: x + bar_w / 2.0,
                top: baseline - h,
                value: *value,
                label: label.clone(),
                title: title.clone(),
            }
        })
        .collect();
    let summary: Vec<String> = data.iter().map(|d| format!("{} {}", d.0, d.1)).collect();
    VBars {
        width,
        height,
        baseline,
        bars,
        aria: format!("{aria_prefix}: {}.", summary.join(", ")),
    }
}

/// One row of a horizontal bar chart. The label is HTML text on its own line
/// (`charts/hbars.html`); the bar is a small SVG whose width is `pct` of the
/// row. Inside it a rect with `rx` is squared off over its left half by a
/// second rect, so only the data end (right) is rounded.
#[derive(Debug, Clone, PartialEq)]
pub struct HBar {
    pub label: String,
    pub value: i64,
    /// Bar length as a percentage of the row (0 = no mark).
    pub pct: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HBars {
    pub bars: Vec<HBar>,
    pub aria: String,
}

/// The longest bar's share of the row; the rest leaves room for the count
/// after it.
const HBAR_MAX_PCT: f64 = 80.0;
/// The shortest bar for a non-zero value, so a small count still shows a mark.
const HBAR_MIN_PCT: f64 = 1.0;

/// Horizontal bars, rounded at the right (data) end. Each department name is
/// HTML text on its own line above its bar, so long names wrap instead of
/// shrinking with the chart or overflowing it.
pub fn hbars(data: &[(String, i64)], aria_prefix: &str) -> HBars {
    let max = data.iter().map(|d| d.1).max().unwrap_or(0).max(1) as f64;
    let round = |x: f64| (x * 10.0).round() / 10.0;
    let bars = data
        .iter()
        .map(|(label, value)| {
            let pct = if *value > 0 {
                round((*value as f64 / max * HBAR_MAX_PCT).max(HBAR_MIN_PCT))
            } else {
                0.0
            };
            HBar {
                label: label.clone(),
                value: *value,
                pct,
            }
        })
        .collect();
    let summary: Vec<String> = data.iter().map(|d| format!("{} {}", d.0, d.1)).collect();
    HBars {
        bars,
        aria: format!("{aria_prefix}: {}.", summary.join(", ")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub x: f64,
    pub w: f64,
    pub class: &'static str,
    pub label: &'static str,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stacked {
    pub width: f64,
    pub segments: Vec<Segment>,
    pub total: i64,
    pub aria: String,
}

/// One stacked bar with 2px gaps between non-empty segments.
pub fn stacked(parts: &[(&'static str, &'static str, i64)], aria_prefix: &str) -> Stacked {
    let width = 600.0;
    let total: i64 = parts.iter().map(|p| p.2).sum();
    let nonzero: Vec<_> = parts.iter().filter(|p| p.2 > 0).collect();
    let gaps = nonzero.len().saturating_sub(1) as f64 * 2.0;
    let usable = (width - gaps).max(0.0);
    let mut x = 0.0;
    let mut segments = Vec::new();
    for (class, label, count) in nonzero {
        let w = if total > 0 {
            *count as f64 / total as f64 * usable
        } else {
            0.0
        };
        segments.push(Segment {
            x,
            w,
            class,
            label,
            count: *count,
        });
        x += w + 2.0;
    }
    let summary: Vec<String> = parts.iter().map(|p| format!("{} {}", p.1, p.2)).collect();
    Stacked {
        width,
        segments,
        total,
        aria: format!("{aria_prefix}: {}.", summary.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vbars_scale_to_the_tallest_and_round_the_top() {
        let c = vbars(
            &[
                ("Nov".into(), 3, "November 2026: 3 due".into()),
                ("Dec".into(), 1, "December 2026: 1 due".into()),
                ("Jan".into(), 0, "January 2027: 0 due".into()),
            ],
            "Items due per month",
        );
        assert_eq!(c.bars.len(), 3);
        assert!(c.bars[0].top < c.bars[1].top, "3 is taller than 1");
        assert!(c.bars[2].path.is_empty(), "zero has no mark");
        assert!(c.bars[0].path.starts_with('M') && c.bars[0].path.contains('Q'));
        assert_eq!(c.aria, "Items due per month: Nov 3, Dec 1, Jan 0.");
    }

    #[test]
    fn stacked_leaves_two_pixel_gaps_and_skips_empty() {
        let s = stacked(
            &[
                ("chart-seg-good", "Active", 3),
                ("chart-seg-info", "In review", 1),
                ("chart-seg-warning", "Renewal due", 0),
                ("chart-seg-critical", "Overdue", 2),
            ],
            "Documents by status",
        );
        assert_eq!(s.total, 6);
        assert_eq!(s.segments.len(), 3);
        let gap = s.segments[1].x - (s.segments[0].x + s.segments[0].w);
        assert!((gap - 2.0).abs() < 1e-9);
        let end = s.segments[2].x + s.segments[2].w;
        assert!((end - 600.0).abs() < 1e-6);
    }

    #[test]
    fn hbars_rows_and_empty_input() {
        let h = hbars(
            &[("Operations".into(), 2), ("Human Resources".into(), 1)],
            "Overdue by department",
        );
        assert_eq!(h.bars.len(), 2);
        assert!(h.bars[0].pct > h.bars[1].pct);
        assert_eq!(
            h.aria,
            "Overdue by department: Operations 2, Human Resources 1."
        );
        let e = hbars(&[], "Overdue by department");
        assert!(e.bars.is_empty());
    }

    #[test]
    fn hbars_scale_to_the_longest_and_leave_room_for_the_count() {
        let h = hbars(
            &[
                ("Information Technology and Communications".into(), 4),
                ("Operations".into(), 1),
                ("Parks".into(), 0),
            ],
            "Overdue items by department",
        );
        // The longest bar stops short of the row, leaving room for its count.
        assert_eq!(h.bars[0].pct, HBAR_MAX_PCT);
        assert_eq!(h.bars[1].pct, 20.0);
        // Zero draws no mark.
        assert_eq!(h.bars[2].pct, 0.0);
        // A long name is kept whole: it is HTML text, not squeezed into a
        // fixed column.
        assert_eq!(h.bars[0].label, "Information Technology and Communications");
    }

    #[test]
    fn hbars_tiny_values_still_show_a_mark() {
        let h = hbars(&[("Big".into(), 1000), ("Small".into(), 1)], "x");
        assert_eq!(h.bars[1].pct, HBAR_MIN_PCT);
        // Percentages are rounded to one decimal for tidy markup.
        let t = hbars(&[("A".into(), 3), ("B".into(), 1)], "x");
        assert_eq!(t.bars[1].pct, 26.7);
    }
}
