//! Renders the catch: a terminal summary and a self-contained HTML page.

use std::fmt::Write as _;

use crate::listing::B58Car;

fn format_price(price: Option<u32>) -> String {
    match price {
        Some(p) => {
            let s = p.to_string();
            let mut out = String::new();
            for (i, c) in s.chars().enumerate() {
                if i > 0 && (s.len() - i) % 3 == 0 {
                    out.push(',');
                }
                out.push(c);
            }
            format!("${out}")
        }
        None => "—".to_string(),
    }
}

fn format_mileage(mileage: Option<u32>) -> String {
    match mileage {
        Some(m) => format!("{} mi", format_price(Some(m)).trim_start_matches('$')),
        None => "—".to_string(),
    }
}

/// Prints an aligned table of the cars to stdout.
pub fn print_table(cars: &[B58Car]) {
    if cars.is_empty() {
        println!("No B58 cars found.");
        return;
    }
    let rows: Vec<[String; 5]> = cars
        .iter()
        .map(|c| {
            [
                format!("{} {} {}", c.year, c.model, c.trim)
                    .trim()
                    .to_string(),
                format_price(c.price),
                format_mileage(c.mileage),
                c.dealer.clone(),
                if c.phev {
                    "PHEV".to_string()
                } else {
                    String::new()
                },
            ]
        })
        .collect();
    let mut widths = [0usize; 5];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    for (row, car) in rows.iter().zip(cars) {
        let mut line = String::new();
        for (w, cell) in widths.iter().zip(row) {
            let _ = write!(line, "{cell:<w$}  ");
        }
        println!("{}", line.trim_end());
        println!("    {}", car.url);
    }
    println!("\n{} B58 car(s) found.", cars.len());
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Renders a standalone HTML page: one card per car, cheapest first.
pub fn render_html(cars: &[B58Car], generated_at: &str) -> String {
    let mut cards = String::new();
    for car in cars {
        let photo = match &car.photo {
            Some(url) => format!(
                r#"<img src="{}" alt="{}" loading="lazy">"#,
                escape(url),
                escape(&car.title)
            ),
            None => r#"<div class="no-photo">no photo</div>"#.to_string(),
        };
        let phev = if car.phev {
            r#" <span class="tag">PHEV</span>"#
        } else {
            ""
        };
        let _ = write!(
            cards,
            r#"
<article>
  <a href="{url}">{photo}</a>
  <div class="body">
    <h2><a href="{url}">{year} {model} {trim}</a>{phev}</h2>
    <p class="price">{price} · {mileage}</p>
    <p class="meta">{fitment}</p>
    <p class="meta">{dealer} · VIN {vin}</p>
  </div>
</article>"#,
            url = escape(&car.url),
            year = car.year,
            model = escape(&car.model),
            trim = escape(&car.trim),
            price = format_price(car.price),
            mileage = format_mileage(car.mileage),
            fitment = escape(&car.fitment),
            dealer = escape(&car.dealer),
            vin = escape(&car.vin),
        );
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>B58 cars near Portland</title>
<style>
  :root {{ color-scheme: light dark; }}
  * {{ box-sizing: border-box; margin: 0; }}
  body {{ font: 16px/1.5 system-ui, sans-serif; padding: 2rem; max-width: 72rem; margin: 0 auto; }}
  header {{ margin-bottom: 1.5rem; }}
  header p {{ opacity: .7; }}
  main {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(19rem, 1fr)); gap: 1.25rem; }}
  article {{ border: 1px solid color-mix(in srgb, currentColor 20%, transparent); border-radius: .75rem; overflow: hidden; }}
  article img, .no-photo {{ width: 100%; aspect-ratio: 4/3; object-fit: cover; display: block; }}
  .no-photo {{ display: grid; place-items: center; opacity: .5; background: color-mix(in srgb, currentColor 8%, transparent); }}
  .body {{ padding: .875rem 1rem 1rem; }}
  h2 {{ font-size: 1.05rem; }}
  h2 a {{ color: inherit; }}
  .price {{ font-weight: 600; margin-top: .25rem; }}
  .meta {{ font-size: .85rem; opacity: .7; }}
  .tag {{ font-size: .7rem; border: 1px solid currentColor; border-radius: .5rem; padding: 0 .4rem; vertical-align: middle; }}
</style>
</head>
<body>
<header>
  <h1>B58 cars near Portland</h1>
  <p>{count} used BMW(s) with the B58 engine · generated {generated}</p>
</header>
<main>{cards}
</main>
</body>
</html>
"#,
        count = cars.len(),
        generated = escape(generated_at),
    )
}

#[cfg(test)]
mod tests {
    use super::{format_mileage, format_price};

    #[test]
    fn formats_prices() {
        assert_eq!(format_price(Some(6_573)), "$6,573");
        assert_eq!(format_price(Some(38_995)), "$38,995");
        assert_eq!(format_price(Some(999)), "$999");
        assert_eq!(format_price(Some(1_000_000)), "$1,000,000");
        assert_eq!(format_price(None), "—");
    }

    #[test]
    fn formats_mileage() {
        assert_eq!(format_mileage(Some(41_203)), "41,203 mi");
        assert_eq!(format_mileage(None), "—");
    }
}
