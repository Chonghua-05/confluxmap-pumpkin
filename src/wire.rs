//! Big-endian wire primitives shared by both companion channels.
//!
//! The two channels (`confluxmap:map_sync` and `confluxmap:waypoints_v1`) are
//! byte-for-byte mirrors of the Java reference codecs, which read and write
//! through `DataInput`/`DataOutput`:
//!
//! * fixed-width integers are big-endian;
//! * strings are a `u16` byte length followed by raw UTF-8;
//! * booleans are one byte, `1` or `0`, and **anything else is an error** - the
//!   reference decoder rejects it rather than reading it as "true";
//! * UUIDs are two big-endian `i64`s (most significant first).
//!
//! Every read is bounds-checked and returns `Option`, because all of these bytes
//! came off the network. Callers turn a `None` into their own protocol error.

use crate::identity::Id;

/// A bounds-checked big-endian cursor over an untrusted payload.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Wraps `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// Reads one unsigned byte.
    pub fn u8(&mut self) -> Option<u8> {
        let value = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(value)
    }

    /// Reads two bytes as an unsigned big-endian integer.
    pub fn u16(&mut self) -> Option<u16> {
        let hi = u16::from(self.u8()?);
        let lo = u16::from(self.u8()?);
        Some((hi << 8) | lo)
    }

    /// Reads four bytes as a signed big-endian integer.
    pub fn i32(&mut self) -> Option<i32> {
        let mut bytes = [0u8; 4];
        for byte in &mut bytes {
            *byte = self.u8()?;
        }
        Some(i32::from_be_bytes(bytes))
    }

    /// Reads eight bytes as a signed big-endian integer.
    pub fn i64(&mut self) -> Option<i64> {
        let mut bytes = [0u8; 8];
        for byte in &mut bytes {
            *byte = self.u8()?;
        }
        Some(i64::from_be_bytes(bytes))
    }

    /// Reads eight bytes as an IEEE-754 double.
    pub fn f64(&mut self) -> Option<f64> {
        let mut bytes = [0u8; 8];
        for byte in &mut bytes {
            *byte = self.u8()?;
        }
        Some(f64::from_be_bytes(bytes))
    }

    /// Reads a boolean, rejecting any encoding other than `0` or `1`.
    pub fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// Reads a `u16`-length-prefixed UTF-8 string, rejecting a length above
    /// `max_bytes` before allocating anything.
    pub fn utf(&mut self, max_bytes: usize) -> Option<String> {
        let len = usize::from(self.u16()?);
        if len > max_bytes {
            return None;
        }
        let slice = self.buf.get(self.pos..self.pos + len)?;
        self.pos += len;
        core::str::from_utf8(slice).ok().map(str::to_owned)
    }

    /// Reads a UUID: most significant `i64` first.
    ///
    /// It comes back as [`Id`] rather than the plugin API's `Uuid`, which a WIT
    /// record cannot compare; callers that need the API type convert with
    /// [`Id::uuid`]. The bytes are the same either way.
    pub fn uuid(&mut self) -> Option<Id> {
        Some(Id {
            high: self.i64()? as u64,
            low: self.i64()? as u64,
        })
    }

    /// Bytes left after the cursor.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
}

/// A field that cannot be encoded because its UTF-8 form exceeds the cap. The
/// reference encoder throws here; so does the Rust one, but the caller decides
/// whether that drops one message or fails a whole handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtfTooLong {
    /// Encoded length of the offending value.
    pub len: usize,
    /// The cap it exceeded.
    pub max: usize,
}

/// Appends one byte.
pub fn u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

/// Appends one `1`/`0` byte.
pub fn bool(out: &mut Vec<u8>, value: bool) {
    out.push(if value { 1 } else { 0 });
}

/// Appends a big-endian `i32`.
pub fn i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Appends a big-endian `i64`.
pub fn i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Appends an IEEE-754 double.
pub fn f64(out: &mut Vec<u8>, value: f64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Appends a UUID as two big-endian `i64`s.
///
/// Takes [`Id`] rather than the plugin API's `Uuid`, which cannot be compared;
/// the layout is unaffected, since both are the same two halves.
pub fn uuid(out: &mut Vec<u8>, value: Id) {
    i64(out, value.high as i64);
    i64(out, value.low as i64);
}

/// Appends a `u16`-length-prefixed UTF-8 string.
///
/// Unlike the `map_sync` policy encoder, which clamps a long field at a
/// character boundary so a misconfigured dimension name cannot cost the whole
/// handshake, this one fails: within the waypoint protocol every string is
/// client-supplied and the reference server treats an over-long one as a
/// malformed request.
pub fn utf(out: &mut Vec<u8>, value: &str, max: usize) -> Result<(), UtfTooLong> {
    let bytes = value.as_bytes();
    if bytes.len() > max {
        return Err(UtfTooLong {
            len: bytes.len(),
            max,
        });
    }
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fixed_width_values_big_endian() {
        let mut r = Reader::new(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        assert_eq!(r.u8(), Some(0x01));
        assert_eq!(r.i32(), Some(0x0203_0405));
        assert_eq!(r.remaining(), 3);
        assert_eq!(r.u8(), Some(0x06));
        assert_eq!(r.u16(), Some(0x0708));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn a_truncated_read_is_none_not_a_panic() {
        let mut r = Reader::new(&[0x00]);
        assert_eq!(r.u8(), Some(0));
        assert_eq!(r.u16(), None);
        assert_eq!(r.i64(), None);
    }

    #[test]
    fn booleans_must_be_zero_or_one() {
        assert_eq!(Reader::new(&[0]).bool(), Some(false));
        assert_eq!(Reader::new(&[1]).bool(), Some(true));
        assert_eq!(Reader::new(&[2]).bool(), None);
    }

    #[test]
    fn utf_rejects_an_oversized_length_prefix() {
        // Length prefix 0x0101 = 257, above a cap of 256.
        assert_eq!(Reader::new(&[0x01, 0x01]).utf(256), None);
        // A length that overruns the buffer is a truncation, not a cap failure.
        assert_eq!(Reader::new(&[0x00, 0x04, b'a']).utf(256), None);
    }

    #[test]
    fn utf_round_trips_and_measures_bytes_not_chars() {
        let value = "长";
        assert_eq!(value.len(), 3);
        let mut out = Vec::new();
        utf(&mut out, value, 256).expect("three bytes fit");
        assert_eq!(out[..2], [0x00, 0x03]);
        assert_eq!(Reader::new(&out).utf(256).as_deref(), Some(value));
    }

    #[test]
    fn utf_fails_rather_than_truncating() {
        let long = "a".repeat(257);
        assert_eq!(
            utf(&mut Vec::new(), &long, 256),
            Err(UtfTooLong { len: 257, max: 256 })
        );
    }

    #[test]
    fn uuid_round_trips_through_two_i64s() {
        let id = Id {
            high: 0x0011_2233_4455_6677,
            low: 0x8899_aabb_ccdd_eeff,
        };
        let mut out = Vec::new();
        uuid(&mut out, id);
        let decoded = Reader::new(&out).uuid().expect("16 bytes");
        assert_eq!(decoded, id);
        assert_eq!(decoded.text(), "00112233-4455-6677-8899-aabbccddeeff");
    }
}
