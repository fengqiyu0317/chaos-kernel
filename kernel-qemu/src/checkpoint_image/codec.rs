use core::convert::TryFrom;

use super::CheckpointError;

// AGENT: checked conversion helper for serialized vector counts.
pub(super) fn checked_usize_to_u32(value: usize) -> Result<u32, CheckpointError> {
    u32::try_from(value).map_err(|_| CheckpointError::LengthOverflow)
}

// AGENT: checked conversion helper for serialized section lengths.
pub(super) fn checked_usize_to_u64(value: usize) -> Result<u64, CheckpointError> {
    u64::try_from(value).map_err(|_| CheckpointError::LengthOverflow)
}

// AGENT: checked conversion helper for decoded vector counts.
pub(super) fn checked_u32_to_usize(value: u32) -> Result<usize, CheckpointError> {
    usize::try_from(value).map_err(|_| CheckpointError::LengthOverflow)
}

// AGENT: checked conversion helper for decoded section lengths.
pub(super) fn checked_u64_to_usize(value: u64) -> Result<usize, CheckpointError> {
    usize::try_from(value).map_err(|_| CheckpointError::LengthOverflow)
}

// AGENT: append a little-endian u16 to a byte vector.
pub(super) fn put_u16(out: &mut alloc::vec::Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

// AGENT: append a little-endian u32 to a byte vector.
pub(super) fn put_u32(out: &mut alloc::vec::Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

// AGENT: append a little-endian u64 to a byte vector.
pub(super) fn put_u64(out: &mut alloc::vec::Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

// AGENT: small cursor for bounds-checked little-endian decoding.
pub(super) struct Cursor<'a> {
    bytes: &'a [u8],
    pub(super) pos: usize,
}

// AGENT: implement exact byte reads for the local checkpoint decoder.
impl<'a> Cursor<'a> {
    // AGENT: create a cursor over one image or section payload.
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    // AGENT: report unconsumed bytes after the parser reaches a logical end.
    pub(super) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    // AGENT: ensure no trailing bytes remain in a fixed-size payload.
    pub(super) fn expect_end(&self) -> Result<(), CheckpointError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(CheckpointError::BadSection)
        }
    }

    // AGENT: read a fixed-size byte array.
    pub(super) fn read_array<const N: usize>(&mut self) -> Result<[u8; N], CheckpointError> {
        let bytes = self.read_bytes(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    // AGENT: read a borrowed byte slice with overflow and bounds checks.
    pub(super) fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], CheckpointError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(CheckpointError::LengthOverflow)?;
        if end > self.bytes.len() {
            return Err(CheckpointError::Truncated);
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    // AGENT: skip reserved bytes while still enforcing input length.
    pub(super) fn skip(&mut self, len: usize) -> Result<(), CheckpointError> {
        let _ = self.read_bytes(len)?;
        Ok(())
    }

    // AGENT: decode one u8.
    pub(super) fn read_u8(&mut self) -> Result<u8, CheckpointError> {
        Ok(self.read_array::<1>()?[0])
    }

    // AGENT: decode one little-endian u16.
    pub(super) fn read_u16(&mut self) -> Result<u16, CheckpointError> {
        Ok(u16::from_le_bytes(self.read_array::<2>()?))
    }

    // AGENT: decode one little-endian u32.
    pub(super) fn read_u32(&mut self) -> Result<u32, CheckpointError> {
        Ok(u32::from_le_bytes(self.read_array::<4>()?))
    }

    // AGENT: decode one little-endian u64.
    pub(super) fn read_u64(&mut self) -> Result<u64, CheckpointError> {
        Ok(u64::from_le_bytes(self.read_array::<8>()?))
    }
}
