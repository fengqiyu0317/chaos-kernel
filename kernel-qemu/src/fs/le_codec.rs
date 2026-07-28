// AGENT: share bounds-checked little-endian reads between filesystem disk
// formats while leaving each format responsible for its own validation rules.
pub(super) struct LeReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

// AGENT: centralize cursor arithmetic and portable integer decoding for
// untrusted filesystem bytes.
impl<'a> LeReader<'a> {
    // AGENT: begin decoding at the first byte of one complete format payload.
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    // AGENT: report unread bytes for count preflight without advancing.
    pub(super) fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    // AGENT: expose trailing padding without changing the checked cursor.
    pub(super) fn remaining_bytes(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    // AGENT: advance only after checked arithmetic and slice bounds both pass.
    pub(super) fn take(&mut self, len: usize) -> Result<&'a [u8], &'static str> {
        let end = self.offset.checked_add(len).ok_or("eio")?;
        let bytes = self.bytes.get(self.offset..end).ok_or("eio")?;
        self.offset = end;
        Ok(bytes)
    }

    // AGENT: decode one byte through the shared checked cursor.
    pub(super) fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }

    // AGENT: decode one stable little-endian 32-bit field.
    pub(super) fn u32(&mut self) -> Result<u32, &'static str> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| "eio")?;
        Ok(u32::from_le_bytes(bytes))
    }

    // AGENT: decode one stable little-endian 64-bit field.
    pub(super) fn u64(&mut self) -> Result<u64, &'static str> {
        let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| "eio")?;
        Ok(u64::from_le_bytes(bytes))
    }

    // AGENT: reject portable u64 disk values that do not fit this target.
    pub(super) fn usize(&mut self) -> Result<usize, &'static str> {
        usize::try_from(self.u64()?).map_err(|_| "eio")
    }
}

// AGENT: own one checked cursor into a fixed filesystem output region so
// callers cannot advance the offset independently from successful writes.
pub(super) struct LeWriter<'a> {
    bytes: &'a mut [u8],
    offset: usize,
}

// AGENT: centralize bounded little-endian encoding for fixed filesystem disk
// structures while preserving their zero-filled trailing bytes.
impl<'a> LeWriter<'a> {
    // AGENT: begin encoding at the first byte of one zero-initialized region.
    pub(super) fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    // AGENT: expose the committed byte count for fixed-format length checks.
    pub(super) fn position(&self) -> usize {
        self.offset
    }

    // AGENT: advance only after checked arithmetic and output bounds both pass.
    pub(super) fn bytes(&mut self, value: &[u8]) -> Result<(), &'static str> {
        let end = self.offset.checked_add(value.len()).ok_or("eio")?;
        self.bytes
            .get_mut(self.offset..end)
            .ok_or("eio")?
            .copy_from_slice(value);
        self.offset = end;
        Ok(())
    }

    // AGENT: encode one stable little-endian 32-bit field.
    pub(super) fn u32(&mut self, value: u32) -> Result<(), &'static str> {
        self.bytes(&value.to_le_bytes())
    }

    // AGENT: encode one stable little-endian 64-bit field.
    pub(super) fn u64(&mut self, value: u64) -> Result<(), &'static str> {
        self.bytes(&value.to_le_bytes())
    }

    // AGENT: reject host-width values that cannot enter the portable u64 ABI.
    pub(super) fn usize(&mut self, value: usize) -> Result<(), &'static str> {
        self.u64(u64::try_from(value).map_err(|_| "eio")?)
    }
}
