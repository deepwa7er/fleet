//! The normalized shapes a listing passes through: what an adapter scrapes
//! (`RawListing`), and what the report renders after VIN decoding and B58
//! classification (`B58Car`).

use serde::Serialize;

/// A used vehicle as scraped from one dealer site, before VIN decoding.
///
/// Adapters fill in whatever their source exposes; only `vin`, `title`,
/// `url`, and `dealer` are guaranteed. Everything model-related is decided
/// later from the VIN, never from the dealer's text.
#[derive(Debug, Clone)]
pub struct RawListing {
    pub vin: String,
    /// The dealer's own listing title, e.g. "2021 BMW M340i xDrive".
    pub title: String,
    pub price: Option<u32>,
    pub mileage: Option<u32>,
    /// Absolute URL of the vehicle detail page.
    pub url: String,
    /// Absolute URL of the primary photo, when the source exposes one.
    pub photo: Option<String>,
    /// Display name of the dealership the listing came from.
    pub dealer: String,
}

/// A confirmed B58 car, ready for the report.
#[derive(Debug, Clone, Serialize)]
pub struct B58Car {
    pub vin: String,
    /// Model year from the VIN decode.
    pub year: u16,
    /// Model designation from the VIN decode, e.g. "M340i" or "X5".
    pub model: String,
    /// Series/trim from the VIN decode, e.g. "xDrive40i". Empty when the
    /// model name alone identifies the car.
    pub trim: String,
    /// Which fitment rule matched, e.g. "540i (G30/G60)".
    pub fitment: String,
    /// True for B58-based plug-in hybrids (745e, X5 xDrive45e/50e).
    pub phev: bool,
    pub price: Option<u32>,
    pub mileage: Option<u32>,
    pub url: String,
    pub photo: Option<String>,
    pub dealer: String,
    /// The dealer's own listing title, kept for cross-checking.
    pub title: String,
}

/// Parses a human-formatted quantity like "232,623 mi" or "$38,995" into an
/// integer, ignoring every non-digit. Returns `None` when no digits remain or
/// the value is zero (dealer platforms use 0 for "call for price").
pub fn parse_quantity(text: &str) -> Option<u32> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    match digits.parse() {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_quantity;

    #[test]
    fn parses_formatted_quantities() {
        assert_eq!(parse_quantity("232,623 mi"), Some(232_623));
        assert_eq!(parse_quantity("$38,995"), Some(38_995));
        assert_eq!(parse_quantity("6573"), Some(6_573));
        assert_eq!(parse_quantity("0"), None);
        assert_eq!(parse_quantity("call for price"), None);
    }
}
