// SPDX-License-Identifier: Apache-2.0

//! The 4-way handshake state machine.
//!
//! This is the station's half of WPA2-PSK, radio-blind: it is fed each
//! received EAPOL-Key frame and returns what the station should do — send a
//! reply, install keys, or nothing. It never touches a radio, which is what
//! lets it run unchanged on any backend and be driven end to end by a host
//! test.
//!
//! The exchange, once the PMK is known (from the passphrase and SSID via
//! [`crypto::wpa_psk`]):
//!
//! 1. AP → message 1: the ANonce. The station picks its own SNonce, derives the
//!    PTK, and replies with message 2 (SNonce + its RSN IE, MIC'd).
//! 2. AP → message 3: MIC'd, carrying the group key wrapped under the KEK. The
//!    station verifies the MIC, unwraps the GTK, and replies with message 4.
//!    Both keys are now installed.
//!
//! The PTK for CCMP is 48 bytes: the key-confirmation key (KCK, MIC), the
//! key-encryption key (KEK, unwraps the GTK), and the temporal key (TK, the
//! data cipher), 16 bytes each.

use crate::eapol::{self, EapolKey, Message, NONCE_LEN};
use crate::keydata::{self, Gtk};

/// The PTK length for CCMP: KCK(16) + KEK(16) + TK(16).
const PTK_LEN: usize = 48;

/// A source of randomness for the SNonce.
///
/// The supplicant must not choose its nonce predictably — a reused or guessable
/// SNonce weakens the key. The caller supplies the platform's entropy (the
/// ESP32 hardware RNG); the handshake only asks for 32 bytes and never for the
/// same nonce twice.
pub trait Rng {
    /// Fill `buf` with cryptographically random bytes.
    fn fill(&mut self, buf: &mut [u8]);
}

/// Where the handshake is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for message 1.
    Idle,
    /// Message 1 seen, message 2 sent, waiting for message 3.
    AwaitingThree,
    /// Message 3 verified, message 4 sent, keys derived. Done.
    Complete,
    /// The handshake failed and will not complete.
    Failed,
}

/// What the caller should do after feeding a frame to [`Supplicant::on_eapol`].
pub enum Action<'a> {
    /// Nothing to do — the frame was not for us, or was a duplicate.
    None,
    /// Send this EAPOL frame back to the AP.
    Send(&'a [u8]),
    /// Send this reply, then install these keys — the handshake completed. The
    /// PTK's temporal key is the unicast cipher key; the GTK is the broadcast
    /// key.
    Complete {
        /// Message 4, to send before the link goes secure.
        reply: &'a [u8],
        /// The 16-byte temporal (data) key from the PTK.
        tk: [u8; 16],
        /// The group key.
        gtk: Gtk,
    },
}

/// The station's 4-way handshake.
///
/// Holds the secrets for the duration of one handshake: the PMK, the nonces,
/// and the derived PTK. Built once the PMK and both MAC addresses are known.
pub struct Supplicant {
    pmk: [u8; 32],
    /// The AP's address (authenticator).
    aa: [u8; 6],
    /// This station's address (supplicant).
    spa: [u8; 6],
    state: State,
    anonce: [u8; NONCE_LEN],
    snonce: [u8; NONCE_LEN],
    ptk: [u8; PTK_LEN],
    /// The replay counter from the AP's last message, echoed in the reply.
    replay: [u8; 8],
    /// A scratch buffer the returned frames borrow from. One handshake sends
    /// one reply at a time, so a single buffer suffices.
    out: [u8; 256],
}

impl Supplicant {
    /// A supplicant for one association. `pmk` is from
    /// [`crypto::wpa_psk`], `aa` the AP's MAC, `spa` this station's MAC.
    pub fn new(pmk: [u8; 32], aa: [u8; 6], spa: [u8; 6]) -> Self {
        Self {
            pmk,
            aa,
            spa,
            state: State::Idle,
            anonce: [0; NONCE_LEN],
            snonce: [0; NONCE_LEN],
            ptk: [0; PTK_LEN],
            replay: [0; 8],
            out: [0; 256],
        }
    }

    /// The current state.
    pub fn state(&self) -> State {
        self.state
    }

    /// The KCK — the first 16 bytes of the PTK. Used for the MIC.
    fn kck(&self) -> &[u8] {
        &self.ptk[..16]
    }

    /// The KEK — the second 16 bytes. Unwraps the GTK.
    fn kek(&self) -> &[u8] {
        &self.ptk[16..32]
    }

    /// The TK — the last 16 bytes. The data cipher key.
    fn tk(&self) -> [u8; 16] {
        self.ptk[32..48].try_into().unwrap()
    }

    /// Feed one received EAPOL-Key frame (starting at the 802.1X header) and a
    /// randomness source. Returns the action to take.
    pub fn on_eapol(&mut self, frame: &[u8], rng: &mut impl Rng) -> Action<'_> {
        let key = match EapolKey::parse(frame) {
            Some(k) => k,
            None => return Action::None,
        };
        // Only descriptor version 2 (HMAC-SHA1 / AES-wrap, i.e. CCMP) is
        // handled; anything else is not this handshake.
        if key.version() != eapol::flag_version_sha1() {
            return Action::None;
        }

        match key.message() {
            Message::One => self.on_message_one(&key, rng),
            Message::Three => self.on_message_three(&key),
            Message::Other => Action::None,
        }
    }

    fn on_message_one(&mut self, key: &EapolKey, rng: &mut impl Rng) -> Action<'_> {
        // ANonce from the AP; fresh SNonce of our own; derive the PTK.
        self.anonce = key.nonce();
        rng.fill(&mut self.snonce);
        self.ptk = crypto::ptk::<PTK_LEN>(&self.pmk, &self.aa, &self.spa, &self.anonce, &self.snonce);
        self.replay = key.replay_counter();

        // Message 2: our SNonce and RSN IE, MIC'd under the fresh KCK.
        let info = eapol::reply_info_msg2(eapol::flag_version_sha1());
        let kck = *array16(self.kck());
        let n = match eapol::build_reply(
            eapol::flag_version_sha1(),
            &kck,
            info,
            &self.replay,
            &self.snonce,
            &keydata::RSN_IE_WPA2_PSK_CCMP,
            &mut self.out,
        ) {
            Some(n) => n,
            None => {
                self.state = State::Failed;
                return Action::None;
            }
        };
        self.state = State::AwaitingThree;
        Action::Send(&self.out[..n])
    }

    fn on_message_three(&mut self, key: &EapolKey) -> Action<'_> {
        if self.state != State::AwaitingThree {
            return Action::None;
        }
        // The MIC on message 3 proves the AP holds the same PTK — so it proves
        // the AP knows the PSK. A failure here is a wrong passphrase or an
        // attacker; either way, refuse.
        let kck = *array16(self.kck());
        if !key.verify_mic(&kck) {
            self.state = State::Failed;
            return Action::None;
        }
        // The ANonce must match message 1's; a different one is a replay or a
        // muddled exchange.
        if key.nonce() != self.anonce {
            self.state = State::Failed;
            return Action::None;
        }

        // Unwrap the GTK from the encrypted key data with the KEK, then find
        // the GTK KDE inside.
        let mut plain = [0u8; 256];
        let kd = key.key_data();
        let gtk = if key.key_data_encrypted() {
            let kek = self.kek();
            let n = match crypto::aes_unwrap(kek, kd, &mut plain) {
                Ok(n) => n,
                Err(_) => {
                    self.state = State::Failed;
                    return Action::None;
                }
            };
            keydata::find_gtk(&plain[..n])
        } else {
            keydata::find_gtk(kd)
        };
        let gtk = match gtk {
            Some(g) => g,
            None => {
                self.state = State::Failed;
                return Action::None;
            }
        };

        // Message 4: MIC'd, SECURE carried over, no key data.
        self.replay = key.replay_counter();
        let info = eapol::reply_info_msg4(
            eapol::flag_version_sha1(),
            eapol::info_is_secure(key.key_info()),
        );
        let zero_nonce = [0u8; NONCE_LEN];
        let n = match eapol::build_reply(
            eapol::flag_version_sha1(),
            &kck,
            info,
            &self.replay,
            &zero_nonce,
            &[],
            &mut self.out,
        ) {
            Some(n) => n,
            None => {
                self.state = State::Failed;
                return Action::None;
            }
        };
        self.state = State::Complete;
        Action::Complete {
            reply: &self.out[..n],
            tk: self.tk(),
            gtk,
        }
    }
}

/// Borrow a 16-byte slice as an array. The callers pass PTK sub-slices that are
/// exactly 16 bytes, so the unwrap cannot fire.
fn array16(s: &[u8]) -> &[u8; 16] {
    s.try_into().expect("16-byte key slice")
}

#[cfg(test)]
mod tests {
    use super::*;

    // A deterministic RNG for the tests: fills with a fixed pattern so the
    // SNonce and thus the PTK are reproducible. Never do this on hardware.
    struct FixedRng(u8);
    impl Rng for FixedRng {
        fn fill(&mut self, buf: &mut [u8]) {
            for b in buf.iter_mut() {
                *b = self.0;
            }
        }
    }

    // Build a message-1 frame with a given ANonce and replay counter.
    fn make_msg1(anonce: [u8; 32], replay: [u8; 8]) -> [u8; eapol::MIN_FRAME_LEN] {
        let mut f = [0u8; eapol::MIN_FRAME_LEN];
        f[1] = eapol::EAPOL_TYPE_KEY;
        f[2..4].copy_from_slice(&(eapol::KEY_BODY_LEN as u16).to_be_bytes());
        f[4] = eapol::KEY_DESC_RSN;
        // version 2 | pairwise | ack
        let ki: u16 = 2 | 0x08 | 0x80;
        f[5..7].copy_from_slice(&ki.to_be_bytes());
        f[9..17].copy_from_slice(&replay);
        f[17..49].copy_from_slice(&anonce);
        f
    }

    #[test]
    fn message_one_derives_the_ptk_and_produces_a_valid_message_two() {
        let pmk = crypto::wpa_psk(b"password", b"testnet");
        let aa = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
        let spa = [0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb];
        let mut sup = Supplicant::new(pmk, aa, spa);

        let anonce = [0x5a; 32];
        let msg1 = make_msg1(anonce, [0, 0, 0, 0, 0, 0, 0, 1]);
        let mut rng = FixedRng(0x33);

        // Copy the reply out so its borrow of `sup` ends before we read the
        // PTK below.
        let mut reply = [0u8; 256];
        let reply_len = match sup.on_eapol(&msg1, &mut rng) {
            Action::Send(r) => {
                reply[..r.len()].copy_from_slice(r);
                r.len()
            }
            _ => panic!("expected a message-2 send"),
        };
        assert_eq!(sup.state(), State::AwaitingThree);

        let k = EapolKey::parse(&reply[..reply_len]).unwrap();
        // Message 2 carries our SNonce and echoes the replay counter, and its
        // MIC verifies under the KCK we derived.
        assert_eq!(k.nonce(), [0x33; 32]);
        assert_eq!(k.replay_counter(), [0, 0, 0, 0, 0, 0, 0, 1]);
        assert!(k.verify_mic(&sup.ptk[..16]));

        // The derived PTK must equal an independent derivation from the same
        // inputs — this ties the state machine to crypto's tested PRF.
        let expect = crypto::ptk::<48>(&pmk, &aa, &spa, &anonce, &[0x33; 32]);
        assert_eq!(sup.ptk, expect);
    }

    #[test]
    fn a_frame_that_is_not_message_one_or_three_is_ignored() {
        let mut sup = Supplicant::new([0; 32], [0; 6], [1; 6]);
        let mut rng = FixedRng(1);
        // A message-2-shaped frame (MIC, no ACK) is Other from our side.
        let mut f = make_msg1([0; 32], [0; 8]);
        let ki: u16 = 2 | 0x08 | 0x100; // pairwise | MIC, no ACK
        f[5..7].copy_from_slice(&ki.to_be_bytes());
        assert!(matches!(sup.on_eapol(&f, &mut rng), Action::None));
        assert_eq!(sup.state(), State::Idle);
    }

    #[test]
    fn a_full_handshake_completes_and_recovers_the_keys() {
        // The strongest test: a simulated AP that shares the PMK derives the
        // same PTK, wraps a GTK, and MIC's a real message 3. The supplicant
        // must complete and hand back exactly the temporal key and GTK.
        let pmk = crypto::wpa_psk(b"correct horse", b"flintnet");
        let aa = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let spa = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
        let mut sup = Supplicant::new(pmk, aa, spa);

        // Message 1 → message 2, capturing the SNonce.
        let anonce = [0xA1u8; 32];
        let msg1 = make_msg1(anonce, [0, 0, 0, 0, 0, 0, 0, 1]);
        let mut snonce = [0u8; 32];
        match sup.on_eapol(&msg1, &mut FixedRng(0x5C)) {
            Action::Send(r) => {
                snonce.copy_from_slice(&EapolKey::parse(r).unwrap().nonce());
            }
            _ => panic!("no message 2"),
        }

        // AP side: the same PTK, its KCK and KEK.
        let ptk = crypto::ptk::<48>(&pmk, &aa, &spa, &anonce, &snonce);
        let kck = &ptk[..16];
        let kek = &ptk[16..32];

        // A GTK, wrapped in its KDE, wrapped under the KEK.
        let gtk_key = [0xEEu8; 16];
        let mut kde = [0u8; 24];
        kde[0] = 0xDD;
        kde[1] = 22;
        kde[2..5].copy_from_slice(&[0x00, 0x0f, 0xac]);
        kde[5] = 1; // GTK KDE
        kde[6] = 1 | 0x04; // key id 1, tx
        kde[8..24].copy_from_slice(&gtk_key);
        let mut wrapped = [0u8; 32];
        let wlen = crypto::aes_wrap(kek, &kde, &mut wrapped).unwrap();

        // Build message 3: pairwise|ack|mic|install|secure|encr, ANonce again,
        // the wrapped GTK as key data, MIC computed last.
        let mut m3 = [0u8; 256];
        m3[1] = eapol::EAPOL_TYPE_KEY;
        m3[4] = eapol::KEY_DESC_RSN;
        let ki: u16 = 2 | 0x08 | 0x80 | 0x100 | 0x40 | 0x200 | 0x1000;
        m3[5..7].copy_from_slice(&ki.to_be_bytes());
        m3[9..17].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2]);
        m3[17..49].copy_from_slice(&anonce);
        m3[97..99].copy_from_slice(&(wlen as u16).to_be_bytes());
        m3[99..99 + wlen].copy_from_slice(&wrapped[..wlen]);
        let total = 99 + wlen;
        m3[2..4].copy_from_slice(&((total - 4) as u16).to_be_bytes());
        let mic = eapol::compute_mic(2, kck, &m3[..total]).unwrap();
        m3[81..97].copy_from_slice(&mic);

        match sup.on_eapol(&m3[..total], &mut FixedRng(0)) {
            Action::Complete { reply, tk, gtk } => {
                // Message 4 is MIC'd and verifies.
                let k4 = EapolKey::parse(reply).unwrap();
                assert!(k4.verify_mic(kck));
                // The temporal key is the PTK's last 16 bytes.
                assert_eq!(tk, <[u8; 16]>::try_from(&ptk[32..48]).unwrap());
                // The GTK came back intact.
                assert_eq!(gtk.key_id, 1);
                assert_eq!(&gtk.key[..16], &gtk_key);
                assert_eq!(sup.state(), State::Complete);
            }
            _ => panic!("handshake did not complete"),
        }
    }

    #[test]
    fn a_message_three_with_a_bad_mic_fails_the_handshake() {
        let pmk = crypto::wpa_psk(b"password", b"net");
        let aa = [0x02, 0, 0, 0, 0, 1];
        let spa = [0x02, 0, 0, 0, 0, 2];
        let mut sup = Supplicant::new(pmk, aa, spa);
        let anonce = [0xA1u8; 32];
        let _ = sup.on_eapol(&make_msg1(anonce, [0, 0, 0, 0, 0, 0, 0, 1]), &mut FixedRng(0x5C));

        // Message 3 with a MIC that is not computed under the real KCK.
        let mut m3 = [0u8; 256];
        m3[1] = eapol::EAPOL_TYPE_KEY;
        m3[4] = eapol::KEY_DESC_RSN;
        let ki: u16 = 2 | 0x08 | 0x80 | 0x100 | 0x40 | 0x200 | 0x1000;
        m3[5..7].copy_from_slice(&ki.to_be_bytes());
        m3[17..49].copy_from_slice(&anonce);
        let total = 99;
        m3[2..4].copy_from_slice(&((total - 4) as u16).to_be_bytes());
        // MIC left as garbage (all zero) — will not verify.
        assert!(matches!(sup.on_eapol(&m3[..total], &mut FixedRng(0)), Action::None));
        assert_eq!(sup.state(), State::Failed);
    }

    #[test]
    fn message_three_before_one_is_ignored() {
        let mut sup = Supplicant::new([0; 32], [0; 6], [1; 6]);
        let mut f = make_msg1([0; 32], [0; 8]);
        let ki: u16 = 2 | 0x08 | 0x80 | 0x100 | 0x40 | 0x200 | 0x1000;
        f[5..7].copy_from_slice(&ki.to_be_bytes());
        // No message 1 seen, so state is Idle and message 3 is refused.
        assert!(matches!(sup.on_eapol(&f, &mut FixedRng(1)), Action::None));
    }
}
