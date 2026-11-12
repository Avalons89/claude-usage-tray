//! Anthropic's published API prices, so local usage can be shown as "what this would cost on the API".

/// Dollars per million tokens.
struct Rate {
    input: f64,
    output: f64,
    cache_read: f64,
}

const fn rate(input: f64, output: f64, cache_read: f64) -> Rate {
    Rate { input, output, cache_read }
}

// Matched by substring in this order, so a longer id wins over its prefix ("opus-5-5" before "opus-5").
// Prices as of Nov 2026. A model missing here costs $0 until it is added.
const RATES: &[(&str, Rate)] = &[
    ("fable-5-2", rate(8.0, 40.0, 0.20)),
    ("mythos-5-2", rate(8.0, 40.0, 0.20)),
    ("fable-5-1", rate(10.0, 50.0, 0.25)),
    ("mythos-5-1", rate(10.0, 50.0, 0.25)),
    ("fable-5", rate(10.0, 50.0, 1.00)),
    ("mythos-5", rate(10.0, 50.0, 1.00)),
    ("opus-5-5", rate(4.0, 20.0, 0.20)),
    ("opus-5", rate(5.0, 25.0, 0.50)),
    ("opus-4-8", rate(5.0, 25.0, 0.50)),
    ("opus-4-7", rate(5.0, 25.0, 0.50)),
    ("opus-4-6", rate(5.0, 25.0, 0.50)),
    ("sonnet-5-5", rate(2.0, 10.0, 0.10)),
    ("sonnet-5", rate(2.0, 10.0, 0.20)),
    ("sonnet-4-6", rate(3.0, 15.0, 0.30)),
    ("haiku-5-5", rate(0.10, 0.50, 0.01)),
    ("haiku-4-5", rate(1.0, 5.0, 0.10)),
];

const CACHE_WRITE_5M: f64 = 1.25;
const CACHE_WRITE_1H: f64 = 2.0;
const FAST_MODE: f64 = 2.0;

pub struct Billable {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub fast: bool,
}

pub fn cost(model: &str, usage: &Billable) -> f64 {
    let Some((_, rate)) = RATES.iter().find(|(id, _)| model.contains(id)) else {
        return 0.0;
    };
    let per_million = usage.input as f64 * rate.input
        + usage.output as f64 * rate.output
        + usage.cache_read as f64 * rate.cache_read
        + usage.cache_write_5m as f64 * rate.input * CACHE_WRITE_5M
        + usage.cache_write_1h as f64 * rate.input * CACHE_WRITE_1H;
    let speed = if usage.fast { FAST_MODE } else { 1.0 };
    per_million / 1_000_000.0 * speed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input_only(input: u64) -> Billable {
        Billable { input, output: 0, cache_read: 0, cache_write_5m: 0, cache_write_1h: 0, fast: false }
    }

    #[test]
    fn longer_model_id_wins_over_its_prefix() {
        assert_eq!(cost("claude-opus-5-5", &input_only(1_000_000)), 4.0);
        assert_eq!(cost("claude-opus-5", &input_only(1_000_000)), 5.0);
        assert_eq!(cost("claude-fable-5-1", &input_only(1_000_000)), 10.0);
    }

    #[test]
    fn unknown_model_costs_nothing() {
        assert_eq!(cost("<synthetic>", &input_only(1_000_000)), 0.0);
    }

    #[test]
    fn cache_writes_and_fast_mode_use_multipliers() {
        let usage = Billable { cache_write_5m: 1_000_000, cache_write_1h: 1_000_000, fast: true, ..input_only(0) };
        // Opus 5.5: 5m write $5 + 1h write $8, doubled for fast mode.
        assert_eq!(cost("claude-opus-5-5", &usage), 26.0);
    }
}
