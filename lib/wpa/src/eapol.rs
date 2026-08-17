// SPDX-License-Identifier: Apache-2.0

//! The EAPOL-Key frame: the wire format of the 4-way handshake.
//!
//! Every message of the handshake is an EAPOL-Key frame — an 802.1X header
//! followed by a fixed 95-byte key body and a variable key-data trailer. This
//! module reads a received frame, identifies which of the four messages it is
//! from its `key_info` flags, verifies its MIC, and builds the station's two
//! replies. It is pure byte-handling over [`crypto`](../../crypto); the state
//! machine that decides *what* to do lives in [`super::handshake`].
//!
//! Layout and flag values are transcribed from esp-idf's `wpa_common.h` /
//! `wpa.c` — the reference the blobs on the other end were built against —
//! rather than from a reading of the standard, for the same reason
//! [`super`]'s crypto matches its vectors: interoperation is the whole point.

/// The 802.1X header: version, type, body length. Precedes the key body.
pub const EAPOL_HDR_LEN: usize = 4;
/// The fixed part of the EAPOL-Key body, before the variable key data.
pub const KEY_BODY_LEN: usize = 95;
/// The smallest a valid EAPOL-Key frame can be: header + fixed body.
pub const MIN_FRAME_LEN: usize = EAPOL_HDR_LEN + KEY_BODY_LEN;

// Field offsets from the start of the frame (the 802.1X header). The key body
// begins at EAPOL_HDR_LEN; each offset below already includes it.
const OFF_VERSION: usize = 0;
const OFF_TYPE: usize = 1;
const OFF_DESC_TYPE: usize = 4;
const OFF_KEY_INFO: usize = 5;
// key_length (7), key_iv (49), key_rsc (65) and key_id (73) are part of the
// frame but not read here; they are accounted for by the fixed KEY_BODY_LEN and
// documented in the module header. OFF_MIC skips past them.
const OFF_REPLAY: usize = 9;
const OFF_NONCE: usize = 17;
const OFF_MIC: usize = 81;
const OFF_KEY_DATA_LEN: usize = 97;
const OFF_KEY_DATA: usize = 99;

/// EAPOL packet type 3 — an EAPOL-Key frame.
pub const EAPOL_TYPE_KEY: u8 = 3;
/// Key descriptor type 2 — RSN (WPA2).
pub const KEY_DESC_RSN: u8 = 2;
/// The MIC is 16 bytes for every descriptor version handled here.
pub const MIC_LEN: usize = 16;
/// A nonce is 32 bytes.
pub const NONCE_LEN: usize = 32;

// key_info bit flags, from wpa_common.h.
mod flag {
    /// The low three bits select the MIC and key-wrap algorithm.
    pub const VERSION_MASK: u16 = 0x0007;
    /// Descriptor version 2: HMAC-SHA1 MIC, AES key-wrapped key data. WPA2-CCMP.
    pub const VERSION_SHA1: u16 = 2;
    /// Descriptor version 3: AES-CMAC MIC, AES key-wrapped key data.
    pub const VERSION_CMAC: u16 = 3;
    /// Pairwise key (the PTK handshake) rather than a group-key update.
    pub const KEY_TYPE: u16 = 0x0008;
    /// Install the key.
    pub const INSTALL: u16 = 0x0040;
    /// Acknowledged — set on the authenticator's messages (1 and 3).
    pub const ACK: u16 = 0x0080;
    /// The MIC field is present and computed.
    pub const MIC: u16 = 0x0100;
    /// The link is secure; set from message 3 on.
    pub const SECURE: u16 = 0x0200;
    /// The key data is encrypted (AES-key-wrapped).
    pub const ENCR_KEY_DATA: u16 = 0x1000;
}

/// Which of the four handshake messages a frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    /// AP → STA: the ANonce. Has ACK, no MIC.
    One,
    /// AP → STA: ANonce again, MIC, the encrypted GTK. Has ACK, MIC, INSTALL.
    Three,
    /// Anything that is not a message the station receives (2 and 4 are the
    /// ones it *sends*, so it should never parse them as input).
    Other,
}

/// A read-only view over a received EAPOL-Key frame.
///
/// Borrows the buffer; the accessors read fields by offset. Construct with
/// [`parse`](EapolKey::parse), which bounds-checks the length once so the
/// accessors need not.
pub struct EapolKey<'a> {
    buf: &'a [u8],
}

impl<'a> EapolKey<'a> {
    /// Parse `buf` as an EAPOL-Key frame, checking it is long enough for the
    /// fixed body and for the key data it claims. `None` if malformed.
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < MIN_FRAME_LEN {
            return None;
        }
        if buf[OFF_TYPE] != EAPOL_TYPE_KEY {
            return None;
        }
        let this = Self { buf };
        if buf.len() < OFF_KEY_DATA + this.key_data_len() {
            return None;
        }
        Some(this)
    }

    fn be16(&self, off: usize) -> u16 {
        u16::from_be_bytes([self.buf[off], self.buf[off + 1]])
    }

    /// The raw `key_info` word.
    pub fn key_info(&self) -> u16 {
        self.be16(OFF_KEY_INFO)
    }

    /// The key descriptor version (1, 2 or 3) from the low three bits.
    pub fn version(&self) -> u16 {
        self.key_info() & flag::VERSION_MASK
    }

    /// The advertised key-data length.
    pub fn key_data_len(&self) -> usize {
        self.be16(OFF_KEY_DATA_LEN) as usize
    }

    /// The 8-byte replay counter, big-endian order preserved.
    pub fn replay_counter(&self) -> [u8; 8] {
        self.buf[OFF_REPLAY..OFF_REPLAY + 8].try_into().unwrap()
    }

    /// The key nonce — ANonce on messages 1 and 3.
    pub fn nonce(&self) -> [u8; NONCE_LEN] {
        self.buf[OFF_NONCE..OFF_NONCE + NONCE_LEN].try_into().unwrap()
    }

    /// The key data (IE list on message 1, encrypted GTK on message 3).
    pub fn key_data(&self) -> &[u8] {
        &self.buf[OFF_KEY_DATA..OFF_KEY_DATA + self.key_data_len()]
    }

    /// Which handshake message this is, from its flags.
    pub fn message(&self) -> Message {
        let ki = self.key_info();
        let pairwise = ki & flag::KEY_TYPE != 0;
        let has_mic = ki & flag::MIC != 0;
        let has_ack = ki & flag::ACK != 0;
        if pairwise && has_ack && !has_mic {
            Message::One
        } else if pairwise && has_ack && has_mic && ki & flag::INSTALL != 0 {
            Message::Three
        } else {
            Message::Other
        }
    }

    /// Whether the key data is AES-key-wrapped (message 3).
    pub fn key_data_encrypted(&self) -> bool {
        self.key_info() & flag::ENCR_KEY_DATA != 0
    }

    /// Verify the frame's MIC against `kck`. The MIC covers the whole frame
    /// with the MIC field itself zeroed; version 2 is HMAC-SHA1 truncated to
    /// 16 bytes, version 3 is AES-CMAC. Constant-time compare.
    pub fn verify_mic(&self, kck: &[u8]) -> bool {
        let mut theirs = [0u8; MIC_LEN];
        theirs.copy_from_slice(&self.buf[OFF_MIC..OFF_MIC + MIC_LEN]);
        let ours = match compute_mic(self.version(), kck, self.buf) {
            Some(m) => m,
            None => return false,
        };
        let mut diff = 0u8;
        for (a, b) in ours.iter().zip(theirs.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

/// Compute the MIC over `frame` under `kck` for descriptor `version`.
///
/// The MIC field in the frame is treated as zero for the computation, whatever
/// it currently holds — so this works both to verify a received frame (its MIC
/// is present) and to fill in an outgoing one (its MIC is still zero).
pub fn compute_mic(version: u16, kck: &[u8], frame: &[u8]) -> Option<[u8; MIC_LEN]> {
    if frame.len() < MIN_FRAME_LEN {
        return None;
    }
    // Copy is unavoidable: the MIC field must read as zero, and `frame` is
    // borrowed. The handshake frames are ~120 bytes, so a stack copy is cheap.
    let mut work = [0u8; 256];
    if frame.len() > work.len() {
        return None;
    }
    work[..frame.len()].copy_from_slice(frame);
    for b in &mut work[OFF_MIC..OFF_MIC + MIC_LEN] {
        *b = 0;
    }
    let msg = &work[..frame.len()];

    let mut mic = [0u8; MIC_LEN];
    match version {
        flag::VERSION_SHA1 => {
            // HMAC-SHA1 is 20 bytes; the MIC is its first 16.
            let full = crypto::hmac_sha1(kck, msg);
            mic.copy_from_slice(&full[..MIC_LEN]);
        }
        flag::VERSION_CMAC => {
            mic = crypto::aes_cmac(kck.try_into().ok()?, msg);
        }
        _ => return None, // version 1 (HMAC-MD5) is not supported
    }
    Some(mic)
}

/// Build the station's reply (message 2 or 4) into `out`, returning its length.
///
/// `reply_info` is the `key_info` for the reply — the caller sets the flags per
/// the message. `nonce` is the SNonce for message 2, ignored (send zeros) for
/// message 4. `key_data` is the station's RSN IE for message 2, empty for
/// message 4. The MIC is computed last, over the finished frame.
#[allow(clippy::too_many_arguments)]
pub fn build_reply(
    version: u16,
    kck: &[u8],
    reply_info: u16,
    replay_counter: &[u8; 8],
    nonce: &[u8; NONCE_LEN],
    key_data: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    let total = OFF_KEY_DATA + key_data.len();
    if out.len() < total {
        return None;
    }
    for b in out[..total].iter_mut() {
        *b = 0;
    }

    // 802.1X header: version 2, type EAPOL-Key, body length.
    out[OFF_VERSION] = 2;
    out[OFF_TYPE] = EAPOL_TYPE_KEY;
    let body_len = (total - EAPOL_HDR_LEN) as u16;
    out[2..4].copy_from_slice(&body_len.to_be_bytes());

    out[OFF_DESC_TYPE] = KEY_DESC_RSN;
    out[OFF_KEY_INFO..OFF_KEY_INFO + 2].copy_from_slice(&reply_info.to_be_bytes());
    // key_length: the reply mirrors nothing here; left zero, as the reference
    // does for messages 2 and 4.
    out[OFF_REPLAY..OFF_REPLAY + 8].copy_from_slice(replay_counter);
    out[OFF_NONCE..OFF_NONCE + NONCE_LEN].copy_from_slice(nonce);
    // key_iv, key_rsc, key_id stay zero.
    out[OFF_KEY_DATA_LEN..OFF_KEY_DATA_LEN + 2]
        .copy_from_slice(&(key_data.len() as u16).to_be_bytes());
    out[OFF_KEY_DATA..total].copy_from_slice(key_data);

    // MIC last, over the whole frame with the MIC field still zero.
    let mic = compute_mic(version, kck, &out[..total])?;
    out[OFF_MIC..OFF_MIC + MIC_LEN].copy_from_slice(&mic);
    Some(total)
}

/// Descriptor version 2 — HMAC-SHA1 MIC, AES-key-wrapped key data. The version
/// WPA2-CCMP uses, and the only one the handshake here handles.
pub fn flag_version_sha1() -> u16 {
    flag::VERSION_SHA1
}

/// The `key_info` for message 2: version, pairwise, MIC.
pub fn reply_info_msg2(version: u16) -> u16 {
    version | flag::KEY_TYPE | flag::MIC
}

/// The `key_info` for message 4: version, pairwise, MIC, and SECURE carried
/// over from message 3.
pub fn reply_info_msg4(version: u16, msg3_secure: bool) -> u16 {
    let mut ki = version | flag::KEY_TYPE | flag::MIC;
    if msg3_secure {
        ki |= flag::SECURE;
    }
    ki
}

/// Whether a frame's `key_info` has the SECURE bit — carried into message 4.
pub fn info_is_secure(key_info: u16) -> bool {
    key_info & flag::SECURE != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal message-1 frame: pairwise + ACK, version 2, an ANonce.
    fn msg1(anonce: [u8; 32], replay: [u8; 8]) -> [u8; MIN_FRAME_LEN] {
        let mut f = [0u8; MIN_FRAME_LEN];
        f[OFF_VERSION] = 2;
        f[OFF_TYPE] = EAPOL_TYPE_KEY;
        let body = (KEY_BODY_LEN) as u16;
        f[2..4].copy_from_slice(&body.to_be_bytes());
        f[OFF_DESC_TYPE] = KEY_DESC_RSN;
        let ki = flag::VERSION_SHA1 | flag::KEY_TYPE | flag::ACK;
        f[OFF_KEY_INFO..OFF_KEY_INFO + 2].copy_from_slice(&ki.to_be_bytes());
        f[OFF_REPLAY..OFF_REPLAY + 8].copy_from_slice(&replay);
        f[OFF_NONCE..OFF_NONCE + 32].copy_from_slice(&anonce);
        f
    }

    #[test]
    fn parses_and_identifies_message_one() {
        let anonce = [0xa1u8; 32];
        let replay = [0, 0, 0, 0, 0, 0, 0, 1];
        let f = msg1(anonce, replay);
        let k = EapolKey::parse(&f).unwrap();
        assert_eq!(k.message(), Message::One);
        assert_eq!(k.version(), 2);
        assert_eq!(k.nonce(), anonce);
        assert_eq!(k.replay_counter(), replay);
        assert!(!k.key_data_encrypted());
    }

    #[test]
    fn a_message_three_is_recognised_by_its_flags() {
        let mut f = msg1([0; 32], [0; 8]);
        let ki = flag::VERSION_SHA1
            | flag::KEY_TYPE
            | flag::ACK
            | flag::MIC
            | flag::INSTALL
            | flag::SECURE
            | flag::ENCR_KEY_DATA;
        f[OFF_KEY_INFO..OFF_KEY_INFO + 2].copy_from_slice(&ki.to_be_bytes());
        let k = EapolKey::parse(&f).unwrap();
        assert_eq!(k.message(), Message::Three);
        assert!(k.key_data_encrypted());
    }

    #[test]
    fn a_short_or_wrong_type_frame_is_rejected() {
        assert!(EapolKey::parse(&[0u8; 10]).is_none());
        let mut f = msg1([0; 32], [0; 8]);
        f[OFF_TYPE] = 1; // not EAPOL-Key
        assert!(EapolKey::parse(&f).is_none());
    }

    #[test]
    fn mic_verifies_against_the_underlying_hmac() {
        // Build a frame, compute its MIC, place it, and confirm verify passes
        // and a tampered frame fails. This pins the "MIC field zeroed" rule and
        // the HMAC-SHA1-truncated-to-16 against crypto's vector-tested HMAC.
        let kck = [0x0bu8; 16];
        let mut f = msg1([0x22; 32], [0; 8]);
        let mic = compute_mic(2, &kck, &f).unwrap();
        f[OFF_MIC..OFF_MIC + MIC_LEN].copy_from_slice(&mic);

        let k = EapolKey::parse(&f).unwrap();
        assert!(k.verify_mic(&kck), "correct MIC must verify");

        // Independently: MIC == first 16 bytes of HMAC-SHA1 over the zeroed-MIC
        // frame.
        let mut zeroed = f;
        for b in &mut zeroed[OFF_MIC..OFF_MIC + MIC_LEN] {
            *b = 0;
        }
        assert_eq!(&mic, &crypto::hmac_sha1(&kck, &zeroed)[..16]);

        f[OFF_NONCE] ^= 0x01; // tamper
        assert!(!EapolKey::parse(&f).unwrap().verify_mic(&kck));
    }

    #[test]
    fn a_reply_round_trips_through_parse() {
        let kck = [0x11u8; 16];
        let snonce = [0x33u8; 32];
        let replay = [0, 0, 0, 0, 0, 0, 0, 2];
        let rsn_ie = [0x30, 0x14, 0x01, 0x00]; // a stub RSN IE prefix
        let mut out = [0u8; 256];
        let n = build_reply(
            2,
            &kck,
            reply_info_msg2(2),
            &replay,
            &snonce,
            &rsn_ie,
            &mut out,
        )
        .unwrap();

        let k = EapolKey::parse(&out[..n]).unwrap();
        assert_eq!(k.nonce(), snonce);
        assert_eq!(k.replay_counter(), replay);
        assert_eq!(k.key_data(), &rsn_ie);
        assert!(k.verify_mic(&kck), "our own MIC must verify");
        // Message 2 has MIC and no ACK, so from the station's view it is Other
        // (not a message it would receive).
        assert_eq!(k.message(), Message::Other);
    }
}
