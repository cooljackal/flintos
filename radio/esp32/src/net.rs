// SPDX-License-Identifier: Apache-2.0

//! The smoltcp link layer for an associated Wi-Fi station.
//!
//! Phase 6 of `doc/plan-radio.md`, issue #68. `smoltcp` is the TCP/IP stack —
//! Rust, `no_std`, and heapless: it never allocates on its own, working instead
//! from buffers we hand it. That suits ground rule 3, where only this tier
//! allocates and the kernel stays static; every buffer here comes from the
//! radio heap ([`kernel::heap`]).
//!
//! # What this file is
//!
//! The **device seam** — the boundary smoltcp's [`phy::Device`] trait sits on.
//! An associated station moves raw Ethernet frames two ways:
//!
//! - **Transmit** hands a frame straight to the blob's `esp_wifi_internal_tx`,
//!   the same raw-L2 path the 4-way handshake sends EAPOL through
//!   ([`crate::supplicant`]). smoltcp writes the frame into the token's buffer;
//!   [`TxToken::consume`] passes it down.
//! - **Receive** drains an [`RxQueue`] the MAC's receive callback fills. Each
//!   frame the blob delivers is copied into a free ring slot on the driver's
//!   task; [`phy::Device::receive`] hands the oldest one to smoltcp.
//!
//! # Status
//!
//! **6.2/6.3/6.4: the stack reaches the internet.** The [`RxQueue`] and
//! [`WifiDevice`] carry traffic both ways; [`rx_trampoline`] is the MAC's
//! receive callback filling the ring. [`obtain_dhcp_lease`] runs DHCP to a
//! lease, and [`dns_tcp_probe`] takes it further — one interface running DHCP,
//! then a DNS resolution, then a TCP connection with a byte exchange.
//! Validated on an ESP32-DevKitC against a real AP: a DHCP lease, `example.com`
//! resolved, a TCP connection to it, and an `HTTP/1.1 200 OK` read back, all
//! within a second of association. The ring behaviour is host-tested besides.
//!
//! Still to come for #68: **6.5** TLS, deferred by the plan.

use core::sync::atomic::{AtomicUsize, Ordering};

use kernel::smp::Spinlock;
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::time::Instant;

/// Largest Ethernet II frame smoltcp will build or accept, headers included and
/// FCS excluded (the MAC appends and strips the FCS). 14-byte header + 1500 MTU.
pub const MTU: usize = 1514;

/// How many received frames the queue holds before it drops the oldest. Sized
/// to ride out a poll interval's worth of arrivals without stalling the MAC;
/// 16 × [`MTU`] is ~24 KiB out of the radio heap.
pub const RX_SLOTS: usize = 16;

/// `WIFI_IF_STA` — the interface index the station's frames ride, matching
/// [`crate::supplicant`]'s `IF_STA`.
#[cfg(target_os = "none")]
const IF_STA: u32 = 0;

#[cfg(target_os = "none")]
extern "C" {
    /// `int esp_wifi_internal_tx(wifi_interface_t, void*, uint16_t)`. Declared
    /// in [`crate::supplicant`]; redeclared here so this module stands alone.
    /// Sends a raw L2 frame — the caller supplies the Ethernet header.
    fn esp_wifi_internal_tx(ifx: u32, buffer: *const core::ffi::c_void, len: u16) -> i32;

    /// `esp_err_t esp_wifi_internal_reg_rxcb(wifi_interface_t, wifi_rxcb_t)`.
    /// Registers the callback the MAC delivers received frames to. Signature
    /// from esp-idf `esp_private/wifi.h` at v4.4.
    fn esp_wifi_internal_reg_rxcb(ifx: u32, cb: RxCb) -> i32;

    /// `void esp_wifi_internal_free_rx_buffer(void* buffer)`. Returns the
    /// buffer handle (`eb`) the MAC lent the callback back to the driver; the
    /// frame's storage is the MAC's, not ours, so it is freed once copied.
    fn esp_wifi_internal_free_rx_buffer(eb: *mut core::ffi::c_void);

    /// `int esp_wifi_get_macaddr_internal(uint8_t if_index, uint8_t*)` — the
    /// station's own MAC, which smoltcp needs as its Ethernet hardware address.
    fn esp_wifi_get_macaddr_internal(ifx: u8, mac: *mut u8) -> i32;
}

/// `typedef esp_err_t (*wifi_rxcb_t)(void *buffer, uint16_t len, void *eb)` —
/// the shape [`esp_wifi_internal_reg_rxcb`] expects.
#[cfg(target_os = "none")]
type RxCb = unsafe extern "C" fn(buffer: *mut core::ffi::c_void, len: u16, eb: *mut core::ffi::c_void) -> i32;

/// One frame's worth of storage: the bytes plus how many are valid.
struct Slot {
    len: usize,
    bytes: [u8; MTU],
}

impl Slot {
    const fn new() -> Self {
        Self { len: 0, bytes: [0; MTU] }
    }
}

/// A single-producer/single-consumer ring of received frames.
///
/// The MAC receive callback is the producer, on the driver's task; the
/// interface poll is the consumer, on the network task. A full ring drops the
/// incoming frame rather than blocking the MAC — TCP recovers from a loss, a
/// wedged receive path does not. The [`Spinlock`] is held only for the index
/// arithmetic and the copy, never across a call out.
pub struct RxQueue {
    slots: Spinlock<[Slot; RX_SLOTS]>,
    /// Next slot the producer writes; only the producer advances it.
    head: AtomicUsize,
    /// Next slot the consumer reads; only the consumer advances it.
    tail: AtomicUsize,
    /// Frames dropped because the ring was full when they arrived. A counter,
    /// not an error: it is diagnostic, read back when a link looks lossy.
    dropped: AtomicUsize,
}

impl RxQueue {
    /// An empty queue. `const`, so it can back a `static` and be filled by the
    /// receive callback without a heap allocation for the ring itself.
    #[allow(clippy::declare_interior_mutable_const)]
    pub const fn new() -> Self {
        const EMPTY: Slot = Slot::new();
        Self {
            slots: Spinlock::new([EMPTY; RX_SLOTS]),
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    /// Copy a received frame into the ring. Called by the MAC receive callback.
    /// Returns `false` and bumps [`Self::dropped`] if the ring is full or the
    /// frame is too long to be one this MAC should have delivered.
    pub fn push(&self, frame: &[u8]) -> bool {
        if frame.len() > MTU {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let head = self.head.load(Ordering::Relaxed);
        let next = (head + 1) % RX_SLOTS;
        if next == self.tail.load(Ordering::Acquire) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.slots.with(|slots| {
            let slot = &mut slots[head];
            slot.bytes[..frame.len()].copy_from_slice(frame);
            slot.len = frame.len();
        });
        self.head.store(next, Ordering::Release);
        true
    }

    /// Take the oldest frame out, calling `f` with its bytes. Returns `None`
    /// when the ring is empty.
    fn pop_with<R>(&self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let tail = self.tail.load(Ordering::Relaxed);
        if tail == self.head.load(Ordering::Acquire) {
            return None;
        }
        let r = self.slots.with(|slots| {
            let slot = &slots[tail];
            f(&slot.bytes[..slot.len])
        });
        self.tail.store((tail + 1) % RX_SLOTS, Ordering::Release);
        Some(r)
    }

    /// How many frames the ring has dropped, cumulative.
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Default for RxQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// smoltcp's view of the associated station: an Ethernet device whose transmit
/// goes to the blob and whose receive drains an [`RxQueue`].
///
/// Zero-copy is not worth it here — the blob owns the receive buffer only for
/// the duration of its callback, so a frame is copied into the ring on arrival
/// regardless. The device borrows the queue rather than owning it: the queue is
/// a `static` the receive callback also names.
pub struct WifiDevice<'q> {
    rx: &'q RxQueue,
}

impl<'q> WifiDevice<'q> {
    /// Wrap a receive queue as a smoltcp device.
    pub fn new(rx: &'q RxQueue) -> Self {
        Self { rx }
    }
}

impl phy::Device for WifiDevice<'_> {
    type RxToken<'a>
        = WifiRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = WifiTxToken
    where
        Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU;
        // One frame in flight: transmit hands straight to the blob, which has
        // its own queueing, so smoltcp need not burst.
        caps.max_burst_size = Some(1);
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // A frame smoltcp receives may prompt an immediate reply (an ARP or a
        // TCP ACK), so a receive yields a transmit token alongside it.
        let mut frame = [0u8; MTU];
        let len = self.rx.pop_with(|bytes| {
            frame[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        })?;
        Some((WifiRxToken { frame, len }, WifiTxToken))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(WifiTxToken)
    }
}

/// A received frame, owned by the token for the length of the `consume` call.
pub struct WifiRxToken {
    frame: [u8; MTU],
    len: usize,
}

impl phy::RxToken for WifiRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame[..self.len])
    }
}

/// The right to transmit one frame: smoltcp fills the buffer, and `consume`
/// hands it to the blob.
pub struct WifiTxToken;

impl phy::TxToken for WifiTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut frame = [0u8; MTU];
        let capped = len.min(MTU);
        let result = f(&mut frame[..capped]);
        tx_raw(&frame[..capped]);
        result
    }
}

/// Hand a built frame to the MAC. The blob copies it out synchronously, so the
/// borrow ends when this returns.
#[cfg(target_os = "none")]
fn tx_raw(frame: &[u8]) {
    // SAFETY: the blob reads `len` bytes from `frame` and copies them before
    // returning; the pointer is valid for that read and outlives the call.
    let rc = unsafe {
        esp_wifi_internal_tx(
            IF_STA,
            frame.as_ptr() as *const core::ffi::c_void,
            frame.len() as u16,
        )
    };
    if rc != 0 {
        // A transmit failure is the link's problem to recover from, not this
        // seam's; smoltcp will retransmit. Recorded, not propagated.
        api::log_debug!("wifi tx dropped a frame: rc={}", rc);
    }
}

/// Host stand-in: there is no blob to transmit through. The frame is dropped,
/// which is what lets the ring and token logic be exercised off-target.
#[cfg(not(target_os = "none"))]
fn tx_raw(_frame: &[u8]) {}

// ── The receive path and a DHCP-driven interface (6.2 / 6.3) ─────────────────

/// The one receive queue, filled by [`rx_trampoline`] and drained by the
/// interface poll. A `static` because the MAC's callback is a bare `extern "C"`
/// function with no place to carry a handle — it reaches the queue by name.
#[cfg(target_os = "none")]
static RX_QUEUE: RxQueue = RxQueue::new();

/// The callback the MAC calls for every received frame, on the driver's task.
///
/// The frame lives in a buffer the MAC owns only until this returns, so it is
/// copied into the ring here and the buffer handed straight back. Returning
/// `ESP_OK` (0) is what the blob expects; there is no failure it acts on.
#[cfg(target_os = "none")]
unsafe extern "C" fn rx_trampoline(
    buffer: *mut core::ffi::c_void,
    len: u16,
    eb: *mut core::ffi::c_void,
) -> i32 {
    if !buffer.is_null() && len != 0 {
        // SAFETY: the MAC guarantees `len` valid bytes at `buffer` for the
        // duration of this call; `push` copies them out before returning.
        let frame = core::slice::from_raw_parts(buffer as *const u8, len as usize);
        RX_QUEUE.push(frame);
    }
    // The buffer handle is the MAC's to reclaim. A NULL `eb` means the frame
    // was one the MAC will not take back (already copied on its side), so free
    // only a real handle.
    if !eb.is_null() {
        esp_wifi_internal_free_rx_buffer(eb);
    }
    0
}

/// The station's own MAC address, for smoltcp's Ethernet hardware address.
#[cfg(target_os = "none")]
pub fn station_mac() -> [u8; 6] {
    let mut mac = [0u8; 6];
    // SAFETY: writes exactly six bytes into a six-byte buffer.
    unsafe { esp_wifi_get_macaddr_internal(0, mac.as_mut_ptr()) };
    mac
}

/// A DHCP lease, in plain bytes so the caller needs no smoltcp types.
#[derive(Clone, Copy, Debug)]
pub struct Lease {
    /// The assigned address and its prefix length.
    pub ip: [u8; 4],
    pub prefix_len: u8,
    /// The default gateway, if the server offered one.
    pub router: Option<[u8; 4]>,
    /// The first DNS server offered, if any.
    pub dns: Option<[u8; 4]>,
}

/// Register the receive callback, bring up a smoltcp interface on the
/// associated station, and run DHCP until it configures or `timeout_ms`
/// elapses. Returns the lease on success.
///
/// This is the first thing that moves a frame both ways: DHCP DISCOVER leaves
/// through [`tx_raw`], the server's OFFER/ACK arrives through [`rx_trampoline`]
/// into the ring, and smoltcp's state machine closes the exchange. It must run
/// **after** the station reports `Connected` — before association the MAC drops
/// everything.
///
/// Call once per association: it registers the callback and owns the interface
/// for the length of the call.
#[cfg(target_os = "none")]
pub fn obtain_dhcp_lease(timeout_ms: u64) -> Option<Lease> {
    use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
    use smoltcp::socket::dhcpv4;
    use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};

    let mac = station_mac();
    api::log_info!(
        "[net] station mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    // SAFETY: `rx_trampoline` matches `wifi_rxcb_t`, and the station is
    // associated (the caller's contract), so the MAC has a control block to
    // attach it to.
    let rc = unsafe { esp_wifi_internal_reg_rxcb(IF_STA, rx_trampoline) };
    if rc != 0 {
        api::log_error!("[net] could not register the rx callback: rc={}", rc);
        return None;
    }

    let mut device = WifiDevice::new(&RX_QUEUE);

    let now = || smoltcp::time::Instant::from_millis(api::timer::now_ms() as i64);
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    // The DHCP transaction id must not repeat across boots; the uptime clock in
    // microseconds is the seed esp-idf's own examples use for the same reason.
    config.random_seed = api::time::now_us();
    let mut iface = Interface::new(config, &mut device, now());

    let mut sock_storage: [SocketStorage; 1] = [SocketStorage::EMPTY];
    let mut sockets = SocketSet::new(&mut sock_storage[..]);
    let dhcp = sockets.add(dhcpv4::Socket::new());

    let start = api::timer::now_ms();
    loop {
        let t = now();
        iface.poll(t, &mut device, &mut sockets);

        match sockets.get_mut::<dhcpv4::Socket>(dhcp).poll() {
            Some(dhcpv4::Event::Configured(cfg)) => {
                let addr = cfg.address;
                iface.update_ip_addrs(|addrs| {
                    let _ = addrs.push(IpCidr::Ipv4(addr));
                });
                if let Some(router) = cfg.router {
                    let _ = iface.routes_mut().add_default_ipv4_route(router);
                }
                let lease = Lease {
                    ip: addr.address().octets(),
                    prefix_len: addr.prefix_len(),
                    router: cfg.router.map(|r| r.octets()),
                    dns: cfg.dns_servers.first().map(|d| d.octets()),
                };
                return Some(lease);
            }
            Some(dhcpv4::Event::Deconfigured) => {}
            None => {}
        }

        if api::timer::now_ms().saturating_sub(start) > timeout_ms {
            api::log_warn!("[net] no DHCP lease within {} ms", timeout_ms);
            api::log_warn!("[net] frames dropped for a full ring: {}", RX_QUEUE.dropped());
            return None;
        }
        // Yield: DHCP has retransmit timers of its own, and a tight poll would
        // starve the driver task that fills the ring.
        api::task::sleep_ms(50);
    }
}

/// The outcome of [`dns_tcp_probe`]: the lease, the resolved address, whether
/// the TCP connection established, and the first bytes the peer sent back.
#[cfg(target_os = "none")]
pub struct Probe {
    pub lease: Lease,
    /// The IPv4 address `host` resolved to.
    pub resolved: [u8; 4],
    /// Whether the TCP three-way handshake completed.
    pub connected: bool,
    /// The first bytes received on the connection (e.g. an HTTP status line).
    pub resp: [u8; 64],
    pub resp_len: usize,
}

/// A `core::fmt::Write` into a fixed slice, for building the request line
/// without an allocator. Stops at the slice's end.
#[cfg(target_os = "none")]
struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

#[cfg(target_os = "none")]
impl core::fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        let end = self.pos.checked_add(b.len()).ok_or(core::fmt::Error)?;
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.pos..end].copy_from_slice(b);
        self.pos = end;
        Ok(())
    }
}

/// Bring the interface up (DHCP), resolve `host` over DNS, open a TCP
/// connection to `host:port`, send a minimal HTTP request and read the peer's
/// first bytes. Phase 6.4 of the plan — DNS and TCP end to end.
///
/// This is one interface for the whole exchange rather than a lease followed by
/// a fresh stack: the DHCP lease, the ARP cache and the resolver all carry
/// forward into the connection. Runs a single poll loop over three sockets,
/// advancing a phase at a time; a whole-run `timeout_ms` bounds it. Must run
/// after the station reports `Connected`.
#[cfg(target_os = "none")]
pub fn dns_tcp_probe(host: &str, port: u16, timeout_ms: u64) -> Result<Probe, &'static str> {
    use core::fmt::Write as _;
    use smoltcp::iface::{Config, Interface, SocketSet, SocketStorage};
    use smoltcp::socket::{dhcpv4, dns, tcp};
    use smoltcp::wire::{
        DnsQueryType, EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address,
    };

    let mac = station_mac();
    api::log_info!(
        "[net] station mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    // SAFETY: `rx_trampoline` matches `wifi_rxcb_t` and the station is
    // associated (the caller's contract).
    if unsafe { esp_wifi_internal_reg_rxcb(IF_STA, rx_trampoline) } != 0 {
        return Err("could not register the rx callback");
    }

    let mut device = WifiDevice::new(&RX_QUEUE);
    let now = || smoltcp::time::Instant::from_millis(api::timer::now_ms() as i64);
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = api::time::now_us();
    let mut iface = Interface::new(config, &mut device, now());

    // Three sockets share one set; their backing storage is declared first so it
    // outlives the borrows the set takes.
    let mut sock_storage: [SocketStorage; 3] = [SocketStorage::EMPTY; 3];
    let mut dns_queries: [Option<dns::DnsQuery>; 1] = [None];
    let mut tcp_rx = [0u8; 2048];
    let mut tcp_tx = [0u8; 512];
    let mut sockets = SocketSet::new(&mut sock_storage[..]);
    let dhcp_h = sockets.add(dhcpv4::Socket::new());
    let dns_h = sockets.add(dns::Socket::new(&[], &mut dns_queries[..]));
    let tcp_h = sockets.add(tcp::Socket::new(
        tcp::SocketBuffer::new(&mut tcp_rx[..]),
        tcp::SocketBuffer::new(&mut tcp_tx[..]),
    ));

    // The request, built once. HTTP/1.0 with a Host header — the smallest thing
    // a name-based virtual host will answer.
    let mut req = [0u8; 128];
    let req = {
        let mut w = BufWriter { buf: &mut req, pos: 0 };
        write!(w, "GET / HTTP/1.0\r\nHost: {host}\r\n\r\n").map_err(|_| "request line too long")?;
        let end = w.pos;
        &req[..end]
    };

    #[derive(PartialEq)]
    enum Phase {
        Dhcp,
        Resolving,
        Connecting,
        Exchanging,
    }
    let mut phase = Phase::Dhcp;
    let mut probe = Probe {
        lease: Lease { ip: [0; 4], prefix_len: 0, router: None, dns: None },
        resolved: [0; 4],
        connected: false,
        resp: [0; 64],
        resp_len: 0,
    };
    let mut query: Option<dns::QueryHandle> = None;
    let start = api::timer::now_ms();

    loop {
        iface.poll(now(), &mut device, &mut sockets);

        match phase {
            Phase::Dhcp => {
                if let Some(dhcpv4::Event::Configured(cfg)) =
                    sockets.get_mut::<dhcpv4::Socket>(dhcp_h).poll()
                {
                    let addr = cfg.address;
                    iface.update_ip_addrs(|a| {
                        let _ = a.push(IpCidr::Ipv4(addr));
                    });
                    if let Some(router) = cfg.router {
                        let _ = iface.routes_mut().add_default_ipv4_route(router);
                    }
                    probe.lease = Lease {
                        ip: addr.address().octets(),
                        prefix_len: addr.prefix_len(),
                        router: cfg.router.map(|r| r.octets()),
                        dns: cfg.dns_servers.first().map(|d| d.octets()),
                    };
                    api::log_info!(
                        "[net] DHCP lease {}.{}.{}.{}/{}",
                        probe.lease.ip[0], probe.lease.ip[1], probe.lease.ip[2],
                        probe.lease.ip[3], probe.lease.prefix_len
                    );

                    // Hand the offered resolvers to the DNS socket, then ask for
                    // the host. Nothing resolves before this.
                    let mut servers = [IpAddress::Ipv4(Ipv4Address::new(0, 0, 0, 0)); 3];
                    let mut n = 0;
                    for d in cfg.dns_servers.iter() {
                        servers[n] = IpAddress::Ipv4(*d);
                        n += 1;
                    }
                    if n == 0 {
                        return Err("the lease named no DNS server");
                    }
                    let dns_sock = sockets.get_mut::<dns::Socket>(dns_h);
                    dns_sock.update_servers(&servers[..n]);
                    query = Some(
                        dns_sock
                            .start_query(iface.context(), host, DnsQueryType::A)
                            .map_err(|_| "the DNS query would not start")?,
                    );
                    api::log_info!("[net] resolving {}", host);
                    phase = Phase::Resolving;
                }
            }
            Phase::Resolving => {
                let dns_sock = sockets.get_mut::<dns::Socket>(dns_h);
                match dns_sock.get_query_result(query.unwrap()) {
                    Ok(addrs) => {
                        let v4 = addrs.iter().find_map(|a| match a {
                            IpAddress::Ipv4(v) => Some(*v),
                            #[allow(unreachable_patterns)]
                            _ => None,
                        });
                        let Some(v4) = v4 else {
                            return Err("DNS returned no A record");
                        };
                        probe.resolved = v4.octets();
                        api::log_info!(
                            "[net] {} is {}.{}.{}.{}",
                            host, probe.resolved[0], probe.resolved[1],
                            probe.resolved[2], probe.resolved[3]
                        );
                        // An ephemeral local port, varied by the uptime clock.
                        let local = 49152 + (api::time::now_us() as u16 & 0x3fff);
                        let tcp_sock = sockets.get_mut::<tcp::Socket>(tcp_h);
                        tcp_sock
                            .connect(iface.context(), (IpAddress::Ipv4(v4), port), local)
                            .map_err(|_| "the TCP connection would not start")?;
                        api::log_info!("[net] connecting to port {}", port);
                        phase = Phase::Connecting;
                    }
                    Err(dns::GetQueryResultError::Pending) => {}
                    Err(_) => return Err("DNS resolution failed"),
                }
            }
            Phase::Connecting => {
                let tcp_sock = sockets.get_mut::<tcp::Socket>(tcp_h);
                if tcp_sock.may_send() {
                    probe.connected = true;
                    api::log_info!("[net] TCP connected; sending request");
                    let _ = tcp_sock.send_slice(req);
                    phase = Phase::Exchanging;
                }
            }
            Phase::Exchanging => {
                let tcp_sock = sockets.get_mut::<tcp::Socket>(tcp_h);
                if tcp_sock.can_recv() {
                    probe.resp_len = tcp_sock.recv_slice(&mut probe.resp).unwrap_or(0);
                    tcp_sock.close();
                    return Ok(probe);
                }
                // The peer closed without sending, but the connection did
                // establish — that alone is bytes moving both ways.
                if !tcp_sock.is_active() {
                    return Ok(probe);
                }
            }
        }

        if api::timer::now_ms().saturating_sub(start) > timeout_ms {
            api::log_warn!("[net] frames dropped for a full ring: {}", RX_QUEUE.dropped());
            // A connection that established still proves the phase, even if the
            // peer was slow to answer.
            if probe.connected {
                return Ok(probe);
            }
            return Err(match phase {
                Phase::Dhcp => "timed out before a DHCP lease",
                Phase::Resolving => "timed out resolving the host",
                Phase::Connecting => "timed out connecting",
                Phase::Exchanging => "timed out awaiting a reply",
            });
        }
        api::task::sleep_ms(20);
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use super::*;

    #[test]
    fn a_pushed_frame_comes_back_out_unchanged() {
        let q = RxQueue::new();
        assert!(q.push(&[1, 2, 3, 4]));
        let mut out = [0u8; MTU];
        let n = q.pop_with(|b| {
            out[..b.len()].copy_from_slice(b);
            b.len()
        });
        assert_eq!(n, Some(4));
        assert_eq!(&out[..4], &[1, 2, 3, 4]);
        // Drained: nothing left.
        assert!(q.pop_with(|_| ()).is_none());
    }

    #[test]
    fn the_ring_holds_one_fewer_than_its_slots_and_drops_the_rest() {
        let q = RxQueue::new();
        // A slot is spent on the full/empty distinction, so capacity is N-1.
        for i in 0..RX_SLOTS - 1 {
            assert!(q.push(&[i as u8]), "slot {i} should accept");
        }
        assert!(!q.push(&[0xff]), "the ring should now be full");
        assert_eq!(q.dropped(), 1);
        // The oldest frame is the first one pushed.
        assert_eq!(q.pop_with(|b| b[0]), Some(0));
    }

    #[test]
    fn an_oversize_frame_is_refused_not_truncated() {
        let q = RxQueue::new();
        let big = [0u8; MTU + 1];
        assert!(!q.push(&big));
        assert_eq!(q.dropped(), 1);
        assert!(q.pop_with(|_| ()).is_none());
    }

    #[test]
    fn the_ring_wraps_and_keeps_fifo_order() {
        let q = RxQueue::new();
        // Fill, drain, and refill so head and tail wrap past the end.
        for round in 0..3 {
            for i in 0..RX_SLOTS - 1 {
                assert!(q.push(&[(round * 10 + i) as u8]));
            }
            for i in 0..RX_SLOTS - 1 {
                assert_eq!(q.pop_with(|b| b[0]), Some((round * 10 + i) as u8));
            }
        }
    }
}
