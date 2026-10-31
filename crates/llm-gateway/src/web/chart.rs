//! Line charts drawn as SVG on the server, as in mnemograph's statistics. The SVG stretches to
//! the chart's box and holds only the lines; labels are HTML around it, so text keeps its size
//! at any width.

use std::fmt::Write;

/// One line of a chart.
pub struct Series {
    pub label: String,
    /// A CSS color, e.g. `var(--line-0)`.
    pub color: String,
    /// One value per point on the x axis.
    pub values: Vec<f64>,
}

/// A line chart of `series` over `x_labels` (one per point; only the first and last are
/// shown). `format` writes the values of the y axis and the legend; `title` describes the
/// chart for screen readers.
pub fn line_chart(
    title: &str,
    x_labels: &[String],
    series: &[Series],
    format: impl Fn(f64) -> String,
) -> String {
    let points = x_labels.len().max(1);
    let max = series
        .iter()
        .flat_map(|s| s.values.iter().copied())
        .fold(0.0, f64::max);
    let scale = if max > 0.0 { max } else { 1.0 };
    // x runs from 0 to the last point (at least 1, so a single day is a flat line); y from 0
    // at the top to 100 at the bottom.
    let width = (points - 1).max(1);
    let mut svg = format!(
        "<svg viewBox=\"0 0 {width} 100\" preserveAspectRatio=\"none\" role=\"img\" \
         aria-label=\"{}\">",
        escape(title)
    );
    for y in [0, 50, 100] {
        let _ = write!(
            svg,
            "<line class=\"grid\" x1=\"0\" x2=\"{width}\" y1=\"{y}\" y2=\"{y}\"/>"
        );
    }
    for line in series {
        let y = |v: f64| 100.0 - v * 100.0 / scale;
        let mut coordinates: Vec<String> = line
            .values
            .iter()
            .enumerate()
            .map(|(x, &v)| format!("{x},{:.2}", y(v)))
            .collect();
        if line.values.len() == 1 {
            coordinates.push(format!("1,{:.2}", y(line.values[0])));
        }
        let _ = write!(
            svg,
            "<polyline points=\"{}\" stroke=\"{}\"/>",
            coordinates.join(" "),
            escape(&line.color)
        );
    }
    svg.push_str("</svg>");

    let first = x_labels.first().map(String::as_str).unwrap_or("");
    let last = x_labels.last().map(String::as_str).unwrap_or("");
    let mut legend = String::new();
    for line in series {
        let total: f64 = line.values.iter().sum();
        let _ = write!(
            legend,
            "<span><span class=\"swatch\" style=\"background: {}\"></span> {} ({})</span>",
            escape(&line.color),
            escape(&line.label),
            escape(&format(total))
        );
    }
    format!(
        "<figure class=\"chart\">\
           <div class=\"chart-plot\"><span class=\"chart-max\">{}</span>{svg}</div>\
           <div class=\"chart-x\"><span>{}</span><span>{}</span></div>\
           <figcaption class=\"legend meta\">{legend}</figcaption>\
         </figure>",
        escape(&format(max)),
        escape(first),
        if points > 1 {
            escape(last)
        } else {
            String::new()
        },
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(values: Vec<f64>) -> Series {
        Series {
            label: "mnemo<b>".into(),
            color: "var(--line-0)".into(),
            values,
        }
    }

    #[test]
    fn lines_are_scaled_to_the_largest_value() {
        let chart = line_chart(
            "Cost",
            &["1 Oct".into(), "2 Oct".into(), "3 Oct".into()],
            &[series(vec![0.0, 0.5, 1.0])],
            |v| format!("{v:.1}"),
        );
        assert!(chart.contains("viewBox=\"0 0 2 100\""), "{chart}");
        assert!(
            chart.contains("points=\"0,100.00 1,50.00 2,0.00\""),
            "{chart}"
        );
        assert!(chart.contains("<span class=\"chart-max\">1.0</span>"));
        assert!(chart.contains("<span>1 Oct</span><span>3 Oct</span>"));
        // The legend shows each line's total; labels are escaped.
        assert!(chart.contains("mnemo&lt;b&gt; (1.5)"), "{chart}");
    }

    #[test]
    fn a_single_point_is_a_flat_line() {
        let chart = line_chart("Cost", &["1 Oct".into()], &[series(vec![0.0])], |v| {
            v.to_string()
        });
        assert!(chart.contains("viewBox=\"0 0 1 100\""), "{chart}");
        assert!(chart.contains("points=\"0,100.00 1,100.00\""), "{chart}");
    }
}
