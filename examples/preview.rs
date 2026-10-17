use anyhow::Result;
use ratatui::style::Color;
use std::fmt::Write;

fn color(value: Color, default: &str) -> String {
    match value {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Green => "#1db954".into(),
        Color::DarkGray => "#89919b".into(),
        Color::Black => "#101419".into(),
        _ => default.into(),
    }
}
fn main() -> Result<()> {
    let buffer = spotify_cli::tui::preview()?;
    let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1040\" height=\"500\" viewBox=\"0 0 1040 500\"><title>Spot terminal interface, illustrative sample music</title><rect width=\"1040\" height=\"500\" rx=\"12\" fill=\"#101419\"/><g font-family=\"Cascadia Mono,Consolas,monospace\" font-size=\"16\">");
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            let px = 20 + x * 10;
            let py = 20 + y * 20;
            if cell.bg != Color::Reset {
                write!(
                    svg,
                    "<rect x=\"{px}\" y=\"{py}\" width=\"10\" height=\"20\" fill=\"{}\"/>",
                    color(cell.bg, "#101419")
                )?;
            }
            if cell.symbol() != " " {
                let symbol = cell
                    .symbol()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                write!(
                    svg,
                    "<text x=\"{px}\" y=\"{}\" fill=\"{}\">{symbol}</text>",
                    py + 16,
                    color(cell.fg, "#e3e9ef")
                )?;
            }
        }
    }
    svg.push_str("</g></svg>\n");
    std::fs::write("docs/preview.svg", svg)?;
    println!("Wrote docs/preview.svg (illustrative sample music)");
    Ok(())
}
