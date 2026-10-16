/// Validate before dav-server normalizes dot segments. Percent decoding occurs
/// exactly once per segment; encoded slash/backslash, traversal and NUL fail.
pub fn validate_path(path: &str, prefix: &str) -> Result<(), &'static str> {
    if !path.starts_with(prefix) {
        return Err("outside DAV root");
    }
    for segment in path.split('/') {
        let bytes = segment.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'%'
                && (i + 2 >= bytes.len()
                    || !bytes[i + 1].is_ascii_hexdigit()
                    || !bytes[i + 2].is_ascii_hexdigit())
            {
                return Err("invalid encoding");
            }
        }
        let decoded = percent_encoding::percent_decode_str(segment).collect::<Vec<_>>();
        if decoded == b"."
            || decoded == b".."
            || decoded.iter().any(|b| matches!(*b, b'/' | b'\\' | 0))
        {
            return Err("forbidden path segment");
        }
    }
    Ok(())
}
