//! Fixed-capacity byte ring for scrollback replay on attach.

use std::collections::VecDeque;

pub struct Ring {
    buf: VecDeque<u8>,
    cap: usize,
    /// Total bytes ever pushed; lets a reader know how much was dropped.
    total: u64,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap.min(1 << 20)),
            cap,
            total: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        if bytes.len() >= self.cap {
            self.buf.clear();
            self.buf.extend(&bytes[bytes.len() - self.cap..]);
            return;
        }
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.buf.drain(..overflow);
        }
        self.buf.extend(bytes);
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_tail() {
        let mut r = Ring::new(8);
        r.push(b"abcde");
        assert_eq!(r.snapshot(), b"abcde");
        r.push(b"fgh");
        assert_eq!(r.snapshot(), b"abcdefgh");
        r.push(b"ij");
        assert_eq!(r.snapshot(), b"cdefghij");
        r.push(b"0123456789xyz");
        assert_eq!(r.snapshot(), b"56789xyz");
        assert_eq!(r.total(), 23);
        assert_eq!(r.len(), 8);
    }
}
