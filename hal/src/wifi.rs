// SPDX-License-Identifier: Apache-2.0

//! Wi-Fi station abstraction — the contract both radio backends satisfy.
//!
//! FlintOS is growing two ways to be a Wi-Fi station, and this is the seam
//! between them and the application:
//!
//! - **The blob backend** (`radio/esp32`): Espressif's binary radio driver
//!   plus a supplicant. It works only on the ESP32 and pulls in vendor
//!   binaries, so it is an opt-in ("unsafe") build.
//! - **The pure-Rust backend** (later, in `drivers/`): a first-party
//!   supplicant over [`crypto`](../../crypto) and a radio driver trait,
//!   portable to any FlintOS target.
//!
//! Both implement [`Station`]. An application says "join this network" against
//! the trait and does not know or care which backend answers — exactly as a
//! logical driver talks to [`Bus`](crate::bus::Bus) without knowing which
//! controller is underneath. The trait lives in `hal` for that reason: `hal`
//! is contracts and nothing else, and `api` re-exports it so a driver crate
//! (which may name `api` but not `hal`) can still implement it.
//!
//! # Asynchronous by nature
//!
//! Associating is not a function that returns when it is done — it is a
//! request, and the answer arrives later. [`Station::connect`] returns as soon
//! as the attempt is *accepted*; whether it succeeded comes back as a
//! [`StationEvent::Connected`] or [`StationEvent::Disconnected`] through the
//! handler the caller registered. Scanning is the same. This matches how the
//! driver actually behaves and avoids pretending a two-second, failure-prone
//! radio exchange is a blocking call.

use core::fmt;

// ── Identifiers ──────────────────────────────────────────────────────────────

/// The largest an SSID may be: 32 octets (IEEE 802.11).
pub const SSID_MAX: usize = 32;

/// A network name.
///
/// Held as bytes and a length, not a string, because an SSID is octets and is
/// **not required to be UTF-8** — a station that could only join UTF-8 networks
/// would be quietly broken in parts of the world.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ssid {
    bytes: [u8; SSID_MAX],
    len: u8,
}

impl Ssid {
    /// Build an SSID from bytes. `None` if longer than [`SSID_MAX`].
    pub fn new(name: &[u8]) -> Option<Self> {
        if name.len() > SSID_MAX {
            return None;
        }
        let mut bytes = [0u8; SSID_MAX];
        bytes[..name.len()].copy_from_slice(name);
        Some(Self {
            bytes,
            len: name.len() as u8,
        })
    }

    /// The SSID's octets, without the trailing padding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    /// As a string, if it happens to be valid UTF-8.
    pub fn as_str(&self) -> Option<&str> {
        core::str::from_utf8(self.as_bytes()).ok()
    }
}

impl fmt::Debug for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_str() {
            Some(s) => write!(f, "Ssid({s:?})"),
            None => write!(f, "Ssid({} non-utf8 bytes)", self.len),
        }
    }
}

/// A BSSID — the MAC address of an access point.
pub type Bssid = [u8; 6];

// ── Security ─────────────────────────────────────────────────────────────────

/// The security a network uses, or a station asks for.
///
/// Deliberately small to start: the personal (pre-shared-key) modes that cover
/// the overwhelming majority of networks. WPA3-only, enterprise/EAP and the
/// legacy WEP modes are named as future work rather than half-supported —
/// a security enum that lists a mode it cannot actually join is a lie the
/// application would act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// No security. Anyone may join and traffic is in the clear.
    Open,
    /// WPA2 Personal (WPA2-PSK), AES-CCMP. The common case.
    Wpa2Psk,
    /// WPA3 Personal (SAE).
    Wpa3Sae,
    /// A transitional network advertising both WPA2-PSK and WPA3-SAE.
    Wpa2Wpa3Psk,
}

/// What a station presents to join a network.
///
/// Borrowed rather than owned: a passphrase lives in the caller's memory for
/// the moment of the connect call and should not be copied into a long-lived
/// struct that outlives its usefulness.
#[derive(Clone, Copy)]
pub enum Credentials<'a> {
    /// For [`Security::Open`]: nothing to present.
    None,
    /// A WPA2/WPA3 passphrase, 8 to 63 printable ASCII characters. It is run
    /// through PBKDF2 to derive the key; see [`crypto::wpa_psk`](../../crypto).
    Passphrase(&'a [u8]),
    /// A pre-derived 256-bit PMK, for a caller that computed it once and cached
    /// it rather than paying PBKDF2's cost on every join.
    Psk([u8; 32]),
}

impl Credentials<'_> {
    /// Whether these credentials are well-formed for `security`. Checked here
    /// so both backends reject the same inputs the same way, before the radio
    /// is ever touched.
    pub fn valid_for(&self, security: Security) -> bool {
        match (self, security) {
            (Credentials::None, Security::Open) => true,
            (Credentials::Passphrase(p), s) if s != Security::Open => {
                (8..=63).contains(&p.len())
            }
            (Credentials::Psk(_), s) => s != Security::Open,
            _ => false,
        }
    }
}

// ── Requests ─────────────────────────────────────────────────────────────────

/// What to connect to.
#[derive(Clone, Copy)]
pub struct ConnectRequest<'a> {
    /// The network name.
    pub ssid: Ssid,
    /// The security to use.
    pub security: Security,
    /// The credentials for that security.
    pub credentials: Credentials<'a>,
    /// Join only this specific AP, if set — otherwise the best match for the
    /// SSID. Pins the choice on a multi-AP network.
    pub bssid: Option<Bssid>,
    /// Try only this channel, if set. Skips the full scan when the caller
    /// already knows where the AP is.
    pub channel: Option<u8>,
}

/// How to scan.
#[derive(Clone, Copy, Default)]
pub struct ScanRequest {
    /// Look only for this SSID, if set — a directed probe rather than a survey.
    pub ssid: Option<Ssid>,
    /// Scan only this channel, if set.
    pub channel: Option<u8>,
    /// Listen passively for beacons instead of sending probe requests. Slower,
    /// but silent and it finds hidden-SSID networks' presence.
    pub passive: bool,
}

/// One access point a scan turned up.
#[derive(Debug, Clone, Copy)]
pub struct ApInfo {
    /// The advertised network name (empty for a hidden SSID).
    pub ssid: Ssid,
    /// The AP's MAC address.
    pub bssid: Bssid,
    /// The primary channel it is on.
    pub channel: u8,
    /// Signal strength in dBm — negative, closer to zero is stronger.
    pub rssi: i8,
    /// The strongest security it advertises.
    pub security: Security,
}

// ── State and events ─────────────────────────────────────────────────────────

/// Where the station is in the connect lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StationState {
    /// Not associated and not trying.
    Disconnected,
    /// A connect is in flight: scanning, authenticating or handshaking.
    Connecting,
    /// Associated, key handshake complete, ready to carry traffic.
    Connected,
}

/// Why a station is not, or is no longer, connected.
///
/// A generalisation of the reason codes every driver reports, kept small and
/// meaningful rather than mirroring 802.11's fifty-odd. The distinction that
/// matters to an application is *whose* fault and whether retrying helps:
/// a wrong passphrase ([`AuthFailed`](Self::AuthFailed)) will never succeed on
/// retry, a missing AP ([`NoApFound`](Self::NoApFound)) might.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// No reason given, or one that does not map to the others.
    Unspecified,
    /// No AP with that SSID was found in range.
    NoApFound,
    /// Authentication was refused — most often a wrong passphrase.
    AuthFailed,
    /// Association was refused after authentication succeeded.
    AssocFailed,
    /// The WPA 4-way key handshake did not complete in time — a wrong
    /// passphrase also lands here on many APs.
    FourWayTimeout,
    /// The association handshake timed out before the key handshake.
    HandshakeTimeout,
    /// The link was up and then lost — beacons stopped, or the AP kicked us.
    ConnectionLost,
    /// The AP dropped the association because the station went idle. Common
    /// when nothing runs above the link — no DHCP, no traffic — so the AP ages
    /// the station out.
    AssocExpired,
    /// The AP replied that the station is not associated (an 802.11 class-3
    /// frame from a non-associated station). Typically the aftermath of the AP
    /// having already aged the association out.
    NotAssociated,
    /// This station asked to disconnect. Not a failure.
    Requested,
}

/// Something the station is telling the application about.
///
/// Delivered through the handler registered with
/// [`Station::set_event_handler`], on whatever task the backend dispatches
/// events on — never in an interrupt. Small and `Copy` so it can cross that
/// boundary by value.
#[derive(Debug, Clone, Copy)]
pub enum StationEvent {
    /// A scan finished; `count` access points are available to read with
    /// [`Station::scan_results`].
    ScanDone {
        /// How many APs were found.
        count: u16,
    },
    /// The station associated and finished its key handshake.
    Connected {
        /// The AP that was joined.
        bssid: Bssid,
        /// The channel it is on.
        channel: u8,
    },
    /// The station is not connected — a connect attempt failed, or an
    /// established link dropped.
    Disconnected {
        /// Why.
        reason: DisconnectReason,
    },
}

/// The handler an application registers to hear [`StationEvent`]s.
///
/// A plain `fn`, not a closure, because it runs on the backend's event task
/// and must carry no borrowed environment across that boundary — the same
/// constraint the existing radio event dispatch has.
pub type EventHandler = fn(StationEvent);

/// A live view of the station, read synchronously at any time.
#[derive(Debug, Clone, Copy)]
pub struct StationStatus {
    /// The lifecycle state.
    pub state: StationState,
    /// The network joined, if [`Connected`](StationState::Connected).
    pub ssid: Option<Ssid>,
    /// The AP joined, if connected.
    pub bssid: Option<Bssid>,
    /// The channel, if connected.
    pub channel: u8,
    /// The last measured signal strength in dBm, if connected.
    pub rssi: i8,
    /// The security in use, if connected.
    pub security: Security,
}

// ── Errors ───────────────────────────────────────────────────────────────────

/// Why a station operation could not even be *started*.
///
/// Distinct from [`DisconnectReason`]: these are refusals at the call, before
/// the radio does anything — a malformed request, or the wrong state. The
/// outcome of an accepted request arrives as a [`StationEvent`], not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiError {
    /// The backend has not been brought up yet.
    NotReady,
    /// The credentials do not fit the requested security — wrong length
    /// passphrase, a key for an open network, and so on.
    BadCredentials,
    /// A connect or scan is already in flight.
    Busy,
    /// An operation that needs an active connection was called without one.
    NotConnected,
    /// The backend refused for a reason of its own; the code is its native
    /// error, for diagnostics only.
    Backend(i32),
}

impl fmt::Display for WifiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The result of a station operation.
pub type WifiResult<T> = Result<T, WifiError>;

// ── The trait ────────────────────────────────────────────────────────────────

/// A Wi-Fi station: the operations an application performs to be on a network.
///
/// Every method that involves the air is *initiating*, not *completing* — see
/// the module docs. A backend implements this; an application holds a `&mut
/// dyn Station` and does not know which one.
pub trait Station {
    /// Begin a scan. Results arrive as [`StationEvent::ScanDone`], after which
    /// [`scan_results`](Station::scan_results) reads them.
    fn scan(&mut self, request: &ScanRequest) -> WifiResult<()>;

    /// Copy up to `out.len()` scan results into `out`, returning how many were
    /// written. Call after a [`ScanDone`](StationEvent::ScanDone); the backend
    /// may release its own copy once read.
    fn scan_results(&mut self, out: &mut [ApInfo]) -> WifiResult<usize>;

    /// Begin connecting. Returns once the attempt is accepted; the outcome
    /// comes as [`Connected`](StationEvent::Connected) or
    /// [`Disconnected`](StationEvent::Disconnected). The request is validated
    /// (credentials against security) before the radio is touched.
    fn connect(&mut self, request: &ConnectRequest) -> WifiResult<()>;

    /// Disconnect from the current network, or cancel a connect in flight.
    fn disconnect(&mut self) -> WifiResult<()>;

    /// The current state and link details, read without blocking.
    fn status(&self) -> StationStatus;

    /// Register the handler for [`StationEvent`]s, returning the previous one.
    /// A `None` handler stops delivery.
    fn set_event_handler(&mut self, handler: Option<EventHandler>) -> Option<EventHandler>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssid_round_trips_and_rejects_oversize() {
        let s = Ssid::new(b"flintnet").unwrap();
        assert_eq!(s.as_bytes(), b"flintnet");
        assert_eq!(s.as_str(), Some("flintnet"));
        assert!(Ssid::new(&[0u8; SSID_MAX]).is_some());
        assert!(Ssid::new(&[0u8; SSID_MAX + 1]).is_none());
    }

    #[test]
    fn a_non_utf8_ssid_is_still_valid() {
        let s = Ssid::new(&[0xff, 0xfe, 0x00, 0x41]).unwrap();
        assert_eq!(s.as_bytes(), &[0xff, 0xfe, 0x00, 0x41]);
        assert_eq!(s.as_str(), None);
    }

    #[test]
    fn credential_validation_matches_security() {
        assert!(Credentials::None.valid_for(Security::Open));
        assert!(!Credentials::None.valid_for(Security::Wpa2Psk));

        // 8..=63 characters for a PSK passphrase.
        assert!(!Credentials::Passphrase(b"short").valid_for(Security::Wpa2Psk));
        assert!(Credentials::Passphrase(b"password").valid_for(Security::Wpa2Psk));
        assert!(Credentials::Passphrase(&[b'a'; 63]).valid_for(Security::Wpa3Sae));
        assert!(!Credentials::Passphrase(&[b'a'; 64]).valid_for(Security::Wpa2Psk));

        // A passphrase for an open network is wrong; so is a key.
        assert!(!Credentials::Passphrase(b"password").valid_for(Security::Open));
        assert!(!Credentials::Psk([0u8; 32]).valid_for(Security::Open));
        assert!(Credentials::Psk([0u8; 32]).valid_for(Security::Wpa2Psk));
    }
}
