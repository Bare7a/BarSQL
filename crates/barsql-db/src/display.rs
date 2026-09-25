pub fn push_float(value: f64, out: &mut String) {
    let mut buf = ryu_js::Buffer::new();
    out.push_str(buf.format(value));
}

pub fn push_float_f32(value: f32, out: &mut String) {
    let mut buf = ryu_js::Buffer::new();
    out.push_str(buf.format(value));
}

pub fn push_bytes(bytes: &[u8], out: &mut String) {
    match std::str::from_utf8(bytes) {
        Ok(text) => out.push_str(text),
        Err(_) => {
            out.push_str("\\x");
            out.push_str(&hex::encode(bytes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(value: f64) -> String {
        let mut out = String::new();
        push_float(value, &mut out);
        out
    }

    #[test]
    fn numbers_render_their_shortest_digits() {
        assert_eq!(render(0.1), "0.1");
        assert_eq!(render(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(render(1e20), "100000000000000000000");
        assert_eq!(render(1e21), "1e+21");
        assert_eq!(render(1e-7), "1e-7");
        assert_eq!(render(-0.0), "0");
        assert_eq!(render(1.100000023841858), "1.100000023841858");
        assert_eq!(render(f64::NAN), "NaN");
    }

    #[test]
    fn float32_renders_its_shortest_digits() {
        let mut out = String::new();
        push_float_f32(1.1, &mut out);
        assert_eq!(out, "1.1");
        out.clear();
        push_float_f32(3.40282e38, &mut out);
        assert_eq!(out, "3.40282e+38");
    }

    #[test]
    fn bytes_render_as_text_or_hex() {
        let mut out = String::new();
        push_bytes(b"hello", &mut out);
        assert_eq!(out, "hello");
        out.clear();
        push_bytes(&[0xde, 0xad, 0xbe, 0xef], &mut out);
        assert_eq!(out, "\\xdeadbeef");
    }
}
