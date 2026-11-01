//! DLT wire format (AUTOSAR PRS_Dlt): zero-copy views of the storage,
//! standard and extended headers of one stored message.
//!
//! ```text
//! storage header  16 B  "DLT\x01", seconds u32 LE, microseconds i32 LE, ECU id
//! standard header  4 B  HTYP, MCNT, LEN u16 BE (standard header + rest)
//!                 +4 B  ECU id      if HTYP.WEID
//!                 +4 B  session id  if HTYP.WSID (u32 BE)
//!                 +4 B  timestamp   if HTYP.WTMS (u32 BE, 0.1 ms)
//! extended header 10 B  MSIN, NOAR, APID, CTID   if HTYP.UEH
//! payload               byte order from HTYP.MSBF
//! ```

pub(crate) const MAGIC: &[u8; 4] = b"DLT\x01";
pub(crate) const STORAGE_LEN: usize = 16;

pub(crate) const HTYP_UEH: u8 = 0x01;
pub(crate) const HTYP_MSBF: u8 = 0x02;
pub(crate) const HTYP_WEID: u8 = 0x04;
pub(crate) const HTYP_WSID: u8 = 0x08;
pub(crate) const HTYP_WTMS: u8 = 0x10;
pub(crate) const HTYP_VERSION_1: u8 = 0x20;
pub(crate) const MSIN_VERB: u8 = 0x01;

pub(crate) const MSTP_LOG: u8 = 0;
pub(crate) const MSTP_APP_TRACE: u8 = 1;
pub(crate) const MSTP_NW_TRACE: u8 = 2;
pub(crate) const MSTP_CONTROL: u8 = 3;

/// What a storage header says about a message: when the logger stored it
/// and the ECU id it gave. A `.dlt` file has it in front of every message;
/// a capture (pcap, MF4) supplies it from the capture's own time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storage<'a> {
    pub secs: u32,
    pub usecs: i32,
    /// NUL-padded; borrowed, so header filters on `ecu` never allocate.
    pub ecu: &'a [u8; 4],
}

/// An unknown ECU id.
pub const NO_ECU: &[u8; 4] = &[0; 4];

impl<'a> Storage<'a> {
    /// The storage header at `buf[0]` (16 bytes starting with `DLT\x01`).
    pub fn parse(buf: &'a [u8]) -> Option<Storage<'a>> {
        if buf.len() < STORAGE_LEN || &buf[..4] != MAGIC {
            return None;
        }
        Some(Storage {
            secs: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            usecs: i32::from_le_bytes(buf[8..12].try_into().unwrap()),
            ecu: buf[12..16].try_into().unwrap(),
        })
    }
}

/// One message: standard header up to the end of its payload, plus what
/// its storage header says (from the file, or from a capture).
#[derive(Clone, Copy)]
pub(crate) struct Message<'a> {
    /// Standard header through payload.
    bytes: &'a [u8],
    storage: Storage<'a>,
    /// Offset of the extended header (or payload, if there is none).
    ext: usize,
}

impl<'a> Message<'a> {
    /// The stored message (storage header first) at `buf[0]`, if its
    /// headers are well formed and it fits in `buf`.
    pub fn parse(buf: &'a [u8]) -> Option<Message<'a>> {
        let storage = Storage::parse(buf)?;
        Message::parse_std(&buf[STORAGE_LEN..], storage)
    }

    /// The message whose standard header is at `buf[0]`, with `storage`
    /// from elsewhere (a capture's time), if it is well formed and fits.
    pub fn parse_std(buf: &'a [u8], storage: Storage<'a>) -> Option<Message<'a>> {
        if buf.len() < 4 {
            return None;
        }
        let htyp = buf[0];
        if htyp & 0xE0 != HTYP_VERSION_1 {
            return None;
        }
        let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        let std_len = 4 + 4 * [HTYP_WEID, HTYP_WSID, HTYP_WTMS].iter().filter(|&&f| htyp & f != 0).count();
        let ext_len = if htyp & HTYP_UEH != 0 { 10 } else { 0 };
        if len < std_len + ext_len || buf.len() < len {
            return None;
        }
        Some(Message { bytes: &buf[..len], storage, ext: std_len })
    }

    /// Size from the standard header on (the `LEN` field).
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    fn htyp(&self) -> u8 {
        self.bytes[0]
    }

    fn has(&self, flag: u8) -> bool {
        self.htyp() & flag != 0
    }

    /// Offset of the optional standard-header field `flag`, if present.
    fn opt_field(&self, flag: u8) -> Option<&'a [u8; 4]> {
        if !self.has(flag) {
            return None;
        }
        let before = [HTYP_WEID, HTYP_WSID, HTYP_WTMS].iter().take_while(|&&f| f != flag).filter(|&&f| self.has(f));
        let at = 4 + 4 * before.count();
        self.bytes[at..at + 4].try_into().ok()
    }

    /// ECU id from the standard header, else from the storage header.
    pub fn ecu(&self) -> &'a [u8] {
        trim_id(self.opt_field(HTYP_WEID).unwrap_or(self.storage.ecu))
    }

    pub fn mcnt(&self) -> u8 {
        self.bytes[1]
    }

    pub fn session_id(&self) -> Option<u32> {
        self.opt_field(HTYP_WSID).map(|b| u32::from_be_bytes(*b))
    }

    /// ECU uptime in 0.1 ms units.
    pub fn timestamp(&self) -> Option<u32> {
        self.opt_field(HTYP_WTMS).map(|b| u32::from_be_bytes(*b))
    }

    fn msin(&self) -> Option<u8> {
        self.has(HTYP_UEH).then(|| self.bytes[self.ext])
    }

    pub fn verbose(&self) -> bool {
        self.msin().is_some_and(|m| m & MSIN_VERB != 0)
    }

    /// Message type (`MSTP_*`), if there is an extended header.
    pub fn mstp(&self) -> Option<u8> {
        self.msin().map(|m| (m >> 1) & 0x07)
    }

    /// Message type info: the log level for log messages.
    pub fn mtin(&self) -> Option<u8> {
        self.msin().map(|m| m >> 4)
    }

    pub fn arg_count(&self) -> u8 {
        if self.has(HTYP_UEH) { self.bytes[self.ext + 1] } else { 0 }
    }

    pub fn apid(&self) -> Option<&'a [u8]> {
        self.has(HTYP_UEH).then(|| trim_id(self.bytes[self.ext + 2..self.ext + 6].try_into().unwrap()))
    }

    pub fn ctid(&self) -> Option<&'a [u8]> {
        self.has(HTYP_UEH).then(|| trim_id(self.bytes[self.ext + 6..self.ext + 10].try_into().unwrap()))
    }

    pub fn big_endian(&self) -> bool {
        self.has(HTYP_MSBF)
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[self.ext + if self.has(HTYP_UEH) { 10 } else { 0 }..]
    }
}

/// IDs are padded with NULs to 4 bytes.
fn trim_id(id: &[u8; 4]) -> &[u8] {
    let n = id.iter().position(|&b| b == 0).unwrap_or(4);
    &id[..n]
}
