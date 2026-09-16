//! Run ids: 8 chars of Crockford base32 (no i/l/o/u), 40 random bits. Short
//! enough to type and to fit in a socket name, unique enough for one machine.

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

pub fn new_id() -> std::io::Result<String> {
    let mut bytes = [0u8; 5];
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))?;
    let mut acc: u64 = 0;
    for b in bytes {
        acc = (acc << 8) | b as u64;
    }
    Ok((0..8)
        .rev()
        .map(|i| ALPHABET[((acc >> (i * 5)) & 31) as usize] as char)
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_short_and_distinct() {
        let a = super::new_id().expect("/dev/urandom");
        let b = super::new_id().expect("/dev/urandom");
        assert_eq!(a.len(), 8);
        assert_ne!(a, b);
        assert!(a.bytes().all(|c| super::ALPHABET.contains(&c)));
    }
}
