//! Which BMWs carry the B58 engine, keyed on the VIN-decoded model year,
//! model designation, and trim — never on dealer listing text.
//!
//! US-market fitment. Sources: BMW press specs per generation; the B58
//! launched in the 2016 340i (F30) and has powered every "40i"-badged car
//! and the M x40i models since. The year guards below exist for names that
//! BMW reused across engines (e.g. a 2015 740i is an N55 car).

/// A confirmed B58 fitment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    /// Human-readable rule that matched, e.g. "540i (G30/G60)".
    pub designation: &'static str,
    /// True for B58-based plug-in hybrids.
    pub phev: bool,
    /// True for the X-series SAVs (X3–X7), hidden by default.
    pub suv: bool,
}

/// One row of the fitment table.
struct Rule {
    /// Matches the start of the VIN-decoded model, lowercased ("m340i", "x5").
    model_prefix: &'static str,
    /// When set, must appear somewhere in the combined model+series+trim
    /// text, lowercased. Used for SUVs/Z4 where the model alone ("X5") is
    /// ambiguous; vPIC places the designation in either field depending on
    /// the model year.
    trim_contains: Option<&'static str>,
    first_year: u16,
    /// Inclusive; `None` means still in production.
    last_year: Option<u16>,
    designation: &'static str,
    phev: bool,
    suv: bool,
}

// Kept one row per rule so the table reads as a table.
#[rustfmt::skip]
const RULES: &[Rule] = &[
    // Sedans/coupes where the model name pins the engine.
    Rule { model_prefix: "340i", trim_contains: None, first_year: 2016, last_year: Some(2018), designation: "340i (F30)", phev: false, suv: false },
    Rule { model_prefix: "440i", trim_contains: None, first_year: 2017, last_year: Some(2020), designation: "440i (F32/F33/F36)", phev: false, suv: false },
    Rule { model_prefix: "m240i", trim_contains: None, first_year: 2017, last_year: None, designation: "M240i (F22/G42)", phev: false, suv: false },
    Rule { model_prefix: "m340i", trim_contains: None, first_year: 2019, last_year: None, designation: "M340i (G20)", phev: false, suv: false },
    Rule { model_prefix: "m440i", trim_contains: None, first_year: 2021, last_year: None, designation: "M440i (G22/G23/G26)", phev: false, suv: false },
    Rule { model_prefix: "540i", trim_contains: None, first_year: 2017, last_year: None, designation: "540i (G30/G60)", phev: false, suv: false },
    Rule { model_prefix: "740i", trim_contains: None, first_year: 2016, last_year: None, designation: "740i (G11/G70)", phev: false, suv: false },
    Rule { model_prefix: "745e", trim_contains: None, first_year: 2020, last_year: Some(2022), designation: "745e (G12, B58 PHEV)", phev: true, suv: false },
    Rule { model_prefix: "840i", trim_contains: None, first_year: 2020, last_year: None, designation: "840i (G14/G15/G16)", phev: false, suv: false },
    // SUVs and Z4, where the trim carries the engine designation.
    Rule { model_prefix: "x3", trim_contains: Some("m40i"), first_year: 2018, last_year: Some(2024), designation: "X3 M40i (G01)", phev: false, suv: true },
    Rule { model_prefix: "x3", trim_contains: Some("m50"), first_year: 2025, last_year: None, designation: "X3 M50 (G45)", phev: false, suv: true },
    Rule { model_prefix: "x4", trim_contains: Some("m40i"), first_year: 2019, last_year: None, designation: "X4 M40i (G02)", phev: false, suv: true },
    Rule { model_prefix: "x5", trim_contains: Some("40i"), first_year: 2019, last_year: None, designation: "X5 40i (G05)", phev: false, suv: true },
    Rule { model_prefix: "x5", trim_contains: Some("45e"), first_year: 2021, last_year: Some(2023), designation: "X5 xDrive45e (B58 PHEV)", phev: true, suv: true },
    Rule { model_prefix: "x5", trim_contains: Some("50e"), first_year: 2024, last_year: None, designation: "X5 xDrive50e (B58 PHEV)", phev: true, suv: true },
    Rule { model_prefix: "x6", trim_contains: Some("40i"), first_year: 2020, last_year: None, designation: "X6 40i (G06)", phev: false, suv: true },
    Rule { model_prefix: "x7", trim_contains: Some("40i"), first_year: 2019, last_year: None, designation: "X7 xDrive40i (G07)", phev: false, suv: true },
    Rule { model_prefix: "z4", trim_contains: Some("m40i"), first_year: 2020, last_year: None, designation: "Z4 M40i (G29)", phev: false, suv: false },
];

/// Classifies a VIN-decoded car. `model` and `trim` are the vPIC `Model` and
/// concatenated `Series`/`Trim` fields; matching is case-insensitive.
pub fn classify(year: u16, model: &str, trim: &str) -> Option<Fit> {
    let model = model.trim().to_lowercase();
    let haystack = format!("{model} {}", trim.to_lowercase());
    RULES
        .iter()
        .find(|r| {
            model.starts_with(r.model_prefix)
                && r.trim_contains.is_none_or(|t| haystack.contains(t))
                && year >= r.first_year
                && r.last_year.is_none_or(|last| year <= last)
        })
        .map(|r| Fit {
            designation: r.designation,
            phev: r.phev,
            suv: r.suv,
        })
}

/// True when the decoded engine data rules out a B58: every B58 is a 3.0 L
/// inline six. Cars matching a fitment rule but contradicting this are
/// dropped with a warning — it means the VIN decode disagrees with the table.
pub fn engine_contradicts(cylinders: Option<u8>, displacement_l: Option<f32>) -> bool {
    if cylinders.is_some_and(|c| c != 6) {
        return true;
    }
    displacement_l.is_some_and(|d| !(2.9..=3.1).contains(&d))
}

#[cfg(test)]
mod tests {
    use super::{classify, engine_contradicts};

    #[test]
    fn b58_cars_match() {
        // (year, model, trim, expected designation fragment)
        let cases = [
            (2016, "340i", "xDrive", "340i"),
            (2018, "340i xDrive Gran Turismo", "", "340i"),
            (2019, "440i", "Gran Coupe", "440i"),
            (2021, "M340i", "xDrive", "M340i"),
            (2024, "M240i", "xDrive", "M240i"),
            (2017, "540i", "", "540i"),
            (2025, "540i", "xDrive", "540i"),
            (2016, "740i", "", "740i"),
            (2021, "745e", "xDrive", "745e"),
            (2023, "X3", "xDrive M40i", "X3 M40i"),
            (2025, "X3", "M50 xDrive", "X3 M50"),
            (2023, "X5", "xDrive40i", "X5 40i"),
            (2020, "X5", "sDrive40i", "X5 40i"),
            (2022, "X5", "xDrive45e", "45e"),
            (2021, "X7", "xDrive40i", "X7"),
            (2022, "Z4", "M40i", "Z4"),
        ];
        for (year, model, trim, expect) in cases {
            let fit = classify(year, model, trim)
                .unwrap_or_else(|| panic!("{year} {model} {trim} should be B58"));
            assert!(
                fit.designation.contains(expect),
                "{year} {model} {trim} matched {} instead of *{expect}*",
                fit.designation
            );
        }
    }

    #[test]
    fn non_b58_cars_do_not_match() {
        let cases = [
            (2015, "340i", ""), // name predates B58? no such car, but the year guard holds
            (2015, "335i", "xDrive"), // N55
            (2018, "330i", "xDrive"), // B48 four-cylinder
            (2016, "M240i", ""), // 2016 M235i era; M240i starts 2017
            (2015, "740i", ""), // N55 F01
            (2016, "540i", ""), // no 2016 540i; G30 starts 2017
            (2020, "X5", "M50i"), // N63 V8
            (2019, "X7", "xDrive50i"), // N63 V8
            (2021, "M4", "Competition"), // S58, not B58
            (2020, "M2", "Competition"), // S55
            (2015, "i8", ""),   // 1.5 L three-cylinder hybrid
            (2018, "X3", "xDrive30i"), // B46/B48
        ];
        for (year, model, trim) in cases {
            assert!(
                classify(year, model, trim).is_none(),
                "{year} {model} {trim} must not classify as B58"
            );
        }
    }

    #[test]
    fn suvs_are_marked() {
        assert!(classify(2023, "X5", "xDrive40i").unwrap().suv);
        assert!(classify(2024, "X3", "M40i").unwrap().suv);
        assert!(!classify(2022, "Z4", "M40i").unwrap().suv);
        assert!(!classify(2021, "M340i", "xDrive").unwrap().suv);
    }

    #[test]
    fn engine_sanity_check() {
        assert!(!engine_contradicts(Some(6), Some(3.0)));
        assert!(!engine_contradicts(None, None));
        assert!(engine_contradicts(Some(8), Some(4.4)));
        assert!(engine_contradicts(Some(6), Some(2.0)));
        assert!(engine_contradicts(None, Some(4.4)));
    }
}
