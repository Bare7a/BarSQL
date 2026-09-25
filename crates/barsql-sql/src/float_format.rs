// Like %g with the shortest digits. Exponent form below 1e-4 and from 1e6, with at least two exponent digits.
pub fn format_float_g(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf".into() } else { "-Inf".into() };
    }
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if !(-4..6).contains(&exp) {
        let (first, rest) = digits.split_at(1);
        let fraction = if rest.is_empty() { String::new() } else { format!(".{rest}") };
        let exp_sign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{first}{fraction}e{exp_sign}{:02}", exp.abs());
    }
    let point = exp + 1;
    if point <= 0 {
        return format!("{sign}0.{}{digits}", "0".repeat((-point) as usize));
    }
    let point = point as usize;
    if digits.len() <= point {
        format!("{sign}{digits}{}", "0".repeat(point - digits.len()))
    } else {
        format!("{sign}{}.{}", &digits[..point], &digits[point..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_shortest_g_form() {
        let cases = [
            (0.0, "0"),
            (-0.0, "-0"),
            (12.7, "12.7"),
            (266.0, "266"),
            (100000.0, "100000"),
            (1e6, "1e+06"),
            (1234567.0, "1.234567e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (0.0111808, "0.0111808"),
            (1.2e-5, "1.2e-05"),
            (-2.5, "-2.5"),
            (1e100, "1e+100"),
            (f64::INFINITY, "+Inf"),
            (f64::NAN, "NaN"),
        ];
        for (f, want) in cases {
            assert_eq!(format_float_g(f), want, "{f}");
        }
    }
}
