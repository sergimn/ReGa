//! Just enough IPv4 for the tunnel's UDP path: parse a header, wrap a UDP
//! datagram (in fragments when it doesn't fit the tunnel), and put
//! fragments back together.
//!
//! Moonlight sees 127.0.0.1 as a local network and asks for 1392-byte video
//! packets. With their headers those are bigger than the tunnel's MTU of
//! 1400, so Windows fragments them and [`Reassembler`] joins them up again.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

pub const HEADER_LEN: usize = 20;
pub const UDP_HEADER_LEN: usize = 8;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;
/// The most an IPv4 packet can carry after a 20-byte header.
const MAX_PAYLOAD: usize = 65535 - HEADER_LEN;
const MORE_FRAGMENTS: u16 = 0x2000;
const OFFSET_MASK: u16 = 0x1fff;

/// The fields of an IPv4 header the tunnel looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub header_len: usize,
    pub total_len: usize,
    pub id: u16,
    pub more_fragments: bool,
    /// In bytes.
    pub fragment_offset: usize,
    pub protocol: u8,
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
}

impl Header {
    /// `None` for anything that isn't a well-formed IPv4 packet.
    pub fn parse(packet: &[u8]) -> Option<Self> {
        if packet.len() < HEADER_LEN || packet[0] >> 4 != 4 {
            return None;
        }
        let header_len = usize::from(packet[0] & 0x0f) * 4;
        let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
        if header_len < HEADER_LEN || total_len < header_len || total_len > packet.len() {
            return None;
        }
        let flags = u16::from_be_bytes([packet[6], packet[7]]);
        Some(Self {
            header_len,
            total_len,
            id: u16::from_be_bytes([packet[4], packet[5]]),
            more_fragments: flags & MORE_FRAGMENTS != 0,
            fragment_offset: usize::from(flags & OFFSET_MASK) * 8,
            protocol: packet[9],
            src: Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]),
            dst: Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]),
        })
    }

    pub fn is_fragment(&self) -> bool {
        self.more_fragments || self.fragment_offset != 0
    }
}

/// The ports and payload of the UDP datagram in a whole (unfragmented)
/// IPv4 packet.
pub fn udp_payload(packet: &[u8]) -> Option<(u16, u16, &[u8])> {
    let header = Header::parse(packet)?;
    if header.protocol != PROTO_UDP || header.is_fragment() {
        return None;
    }
    let udp = &packet[header.header_len..header.total_len];
    if udp.len() < UDP_HEADER_LEN {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
    if len < UDP_HEADER_LEN || len > udp.len() {
        return None;
    }
    let src = u16::from_be_bytes([udp[0], udp[1]]);
    let dst = u16::from_be_bytes([udp[2], udp[3]]);
    Some((src, dst, &udp[UDP_HEADER_LEN..len]))
}

/// IPv4 packets that carry `payload` from `src` to `dst` over UDP, split
/// into fragments when the whole packet is bigger than `mtu`. `None` when
/// the payload can't fit in one IPv4 packet at all.
pub fn udp_packets(
    src: SocketAddrV4,
    dst: SocketAddrV4,
    payload: &[u8],
    id: u16,
    mtu: usize,
) -> Option<Vec<Vec<u8>>> {
    let udp_len = UDP_HEADER_LEN + payload.len();
    if udp_len > MAX_PAYLOAD {
        return None;
    }
    let mut udp = Vec::with_capacity(udp_len);
    udp.extend_from_slice(&src.port().to_be_bytes());
    udp.extend_from_slice(&dst.port().to_be_bytes());
    udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);
    let mut pseudo = [0u8; 12];
    pseudo[..4].copy_from_slice(&src.ip().octets());
    pseudo[4..8].copy_from_slice(&dst.ip().octets());
    pseudo[9] = PROTO_UDP;
    pseudo[10..].copy_from_slice(&(udp_len as u16).to_be_bytes());
    let sum = checksum(&[&pseudo, &udp]);
    // Zero means "no checksum" in UDP, so a computed zero goes out as its
    // ones' complement twin.
    let sum = if sum == 0 { 0xffff } else { sum };
    udp[6..8].copy_from_slice(&sum.to_be_bytes());

    let (src, dst) = (*src.ip(), *dst.ip());
    if HEADER_LEN + udp.len() <= mtu {
        return Some(vec![packet(src, dst, PROTO_UDP, id, 0, &udp)]);
    }
    // Every fragment but the last carries a multiple of 8 bytes.
    let chunk = (mtu.saturating_sub(HEADER_LEN) / 8 * 8).max(8);
    let count = udp.len().div_ceil(chunk);
    Some(
        udp.chunks(chunk)
            .enumerate()
            .map(|(i, part)| {
                let mut flags = (i * chunk / 8) as u16;
                if i + 1 < count {
                    flags |= MORE_FRAGMENTS;
                }
                packet(src, dst, PROTO_UDP, id, flags, part)
            })
            .collect(),
    )
}

fn packet(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: u8,
    id: u16,
    flags: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&[0x45, 0]);
    out.extend_from_slice(&((HEADER_LEN + payload.len()) as u16).to_be_bytes());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&[64, protocol, 0, 0]);
    out.extend_from_slice(&src.octets());
    out.extend_from_slice(&dst.octets());
    let sum = checksum(&[&out]);
    out[10..12].copy_from_slice(&sum.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// The Internet checksum of the parts joined together. Every part but the
/// last must have an even length.
fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    for part in parts {
        let (words, rest) = part.as_chunks::<2>();
        for word in words {
            sum += u32::from(u16::from_be_bytes(*word));
        }
        if let [last] = rest {
            sum += u32::from(*last) << 8;
        }
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Joins IPv4 fragments back into whole packets. It holds a few packets at
/// a time and gives up on one after two seconds: a video packet that late
/// is no use anyway.
pub struct Reassembler {
    pending: Vec<Partial>,
}

struct Partial {
    key: (Ipv4Addr, Ipv4Addr, u16, u8),
    started: Instant,
    header: Option<Vec<u8>>,
    data: Vec<u8>,
    /// Byte ranges received so far, sorted and merged.
    have: Vec<(usize, usize)>,
    total: Option<usize>,
}

const MAX_PENDING: usize = 32;
const TIMEOUT: Duration = Duration::from_secs(2);

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Takes one fragment (`header` is its parsed header) and returns the
    /// whole packet once every piece of it has arrived.
    pub fn push(&mut self, header: &Header, fragment: &[u8], now: Instant) -> Option<Vec<u8>> {
        self.pending
            .retain(|p| now.saturating_duration_since(p.started) < TIMEOUT);
        let payload = &fragment[header.header_len..header.total_len];
        let start = header.fragment_offset;
        let end = start + payload.len();
        if end > MAX_PAYLOAD || (header.more_fragments && !payload.len().is_multiple_of(8)) {
            return None;
        }
        let key = (header.src, header.dst, header.id, header.protocol);
        let index = match self.pending.iter().position(|p| p.key == key) {
            Some(i) => i,
            None => {
                if self.pending.len() == MAX_PENDING {
                    self.pending.remove(0);
                }
                self.pending.push(Partial {
                    key,
                    started: now,
                    header: None,
                    data: Vec::new(),
                    have: Vec::new(),
                    total: None,
                });
                self.pending.len() - 1
            }
        };
        let partial = &mut self.pending[index];
        if !header.more_fragments {
            if partial.total.is_some_and(|t| t != end) {
                self.pending.remove(index);
                return None;
            }
            partial.total = Some(end);
        }
        if partial.total.is_some_and(|t| end > t) {
            self.pending.remove(index);
            return None;
        }
        if partial.data.len() < end {
            partial.data.resize(end, 0);
        }
        partial.data[start..end].copy_from_slice(payload);
        if start == 0 {
            partial.header = Some(fragment[..header.header_len].to_vec());
        }
        add_range(&mut partial.have, (start, end));

        let total = partial.total?;
        if partial.header.is_none() || partial.have.first() != Some(&(0, total)) {
            return None;
        }
        let partial = self.pending.remove(index);
        let mut whole = partial.header.expect("checked above");
        let header_len = whole.len();
        let length = (header_len + total) as u16;
        whole[2..4].copy_from_slice(&length.to_be_bytes());
        whole[6..8].copy_from_slice(&[0, 0]);
        whole[10..12].copy_from_slice(&[0, 0]);
        let sum = checksum(&[&whole]);
        whole[10..12].copy_from_slice(&sum.to_be_bytes());
        whole.extend_from_slice(&partial.data[..total]);
        Some(whole)
    }
}

fn add_range(ranges: &mut Vec<(usize, usize)>, (mut start, mut end): (usize, usize)) {
    let mut merged = Vec::with_capacity(ranges.len() + 1);
    for &(s, e) in ranges.iter() {
        if e < start || s > end {
            merged.push((s, e));
        } else {
            start = start.min(s);
            end = end.max(e);
        }
    }
    merged.push((start, end));
    merged.sort_unstable();
    *ranges = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(ip: [u8; 4], port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::from(ip), port)
    }

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn a_small_datagram_is_one_packet_with_valid_checksums() {
        let src = addr([10, 213, 37, 2], 40000);
        let dst = addr([10, 213, 37, 1], 47998);
        let data = payload(101);
        let packets = udp_packets(src, dst, &data, 7, 1400).unwrap();
        assert_eq!(packets.len(), 1);
        let p = &packets[0];
        let header = Header::parse(p).unwrap();
        assert_eq!(header.total_len, 20 + 8 + 101);
        assert_eq!((header.src, header.dst), (*src.ip(), *dst.ip()));
        assert!(!header.is_fragment());
        // A header with a correct checksum sums to zero.
        assert_eq!(checksum(&[&p[..20]]), 0);
        // So does a UDP datagram with its pseudo-header.
        let mut pseudo = [0u8; 12];
        pseudo[..4].copy_from_slice(&src.ip().octets());
        pseudo[4..8].copy_from_slice(&dst.ip().octets());
        pseudo[9] = PROTO_UDP;
        pseudo[10..].copy_from_slice(&(8u16 + 101).to_be_bytes());
        assert_eq!(checksum(&[&pseudo, &p[20..]]), 0);
        assert_eq!(udp_payload(p), Some((40000, 47998, &data[..])));
    }

    #[test]
    fn checksum_matches_a_known_header() {
        // The worked example from RFC 1071's usual illustration.
        let header = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(checksum(&[&header]), 0xb861);
    }

    #[test]
    fn a_video_packet_is_fragmented_to_fit_the_tunnel_and_joined_again() {
        let src = addr([10, 213, 37, 1], 47998);
        let dst = addr([10, 213, 37, 2], 40000);
        let data = payload(1392);
        let packets = udp_packets(src, dst, &data, 0x1234, 1400).unwrap();
        assert_eq!(packets.len(), 2);
        assert!(packets.iter().all(|p| p.len() <= 1400));
        let mut joiner = Reassembler::new();
        let now = Instant::now();
        // Out of order, as a network may deliver them.
        let second = Header::parse(&packets[1]).unwrap();
        assert!(second.is_fragment() && !second.more_fragments);
        assert_eq!(joiner.push(&second, &packets[1], now), None);
        let first = Header::parse(&packets[0]).unwrap();
        assert!(first.more_fragments && first.fragment_offset == 0);
        let whole = joiner.push(&first, &packets[0], now).unwrap();
        assert_eq!(checksum(&[&whole[..20]]), 0);
        assert_eq!(udp_payload(&whole), Some((47998, 40000, &data[..])));
        assert!(joiner.pending.is_empty());
    }

    #[test]
    fn many_fragments_duplicates_and_interleaved_packets() {
        let src = addr([10, 213, 37, 1], 48000);
        let dst = addr([10, 213, 37, 2], 40002);
        let a = payload(5000);
        let b: Vec<u8> = payload(3000).into_iter().rev().collect();
        let pa = udp_packets(src, dst, &a, 1, 576).unwrap();
        let pb = udp_packets(src, dst, &b, 2, 576).unwrap();
        assert!(pa.len() > 5 && pb.len() > 3);
        let mut joiner = Reassembler::new();
        let now = Instant::now();
        let mut done = Vec::new();
        let mut order: Vec<&Vec<u8>> = Vec::new();
        for i in (0..pa.len().max(pb.len())).rev() {
            order.extend(pa.get(i));
            order.extend(pb.get(i));
            // A duplicate of each fragment changes nothing.
            order.extend(pa.get(i));
        }
        for p in order {
            if let Some(whole) = joiner.push(&Header::parse(p).unwrap(), p, now) {
                done.push(whole);
            }
        }
        assert_eq!(done.len(), 2);
        let payloads: Vec<_> = done.iter().map(|p| udp_payload(p).unwrap().2).collect();
        assert!(payloads.contains(&&a[..]) && payloads.contains(&&b[..]));
    }

    #[test]
    fn stale_and_inconsistent_fragments_are_dropped() {
        let src = addr([10, 213, 37, 1], 47998);
        let dst = addr([10, 213, 37, 2], 40000);
        let packets = udp_packets(src, dst, &payload(1392), 9, 1400).unwrap();
        let mut joiner = Reassembler::new();
        let start = Instant::now();
        let first = Header::parse(&packets[0]).unwrap();
        assert_eq!(joiner.push(&first, &packets[0], start), None);
        // The rest arrives too late: the first half is gone by then.
        let second = Header::parse(&packets[1]).unwrap();
        let late = start + Duration::from_secs(3);
        assert_eq!(joiner.push(&second, &packets[1], late), None);
        assert_eq!(joiner.pending.len(), 1);

        // A fragment that isn't a multiple of 8 bytes, but says more follow.
        let mut odd = Header::parse(&packets[0]).unwrap();
        odd.total_len -= 3;
        assert_eq!(joiner.push(&odd, &packets[0], late), None);
    }

    #[test]
    fn it_holds_only_so_many_packets() {
        let mut joiner = Reassembler::new();
        let now = Instant::now();
        for id in 0..100u16 {
            let src = addr([10, 213, 37, 1], 47998);
            let dst = addr([10, 213, 37, 2], 40000);
            let packets = udp_packets(src, dst, &payload(2000), id, 1400).unwrap();
            joiner.push(&Header::parse(&packets[0]).unwrap(), &packets[0], now);
        }
        assert_eq!(joiner.pending.len(), MAX_PENDING);
    }

    #[test]
    fn garbage_is_not_a_packet() {
        assert_eq!(Header::parse(&[]), None);
        assert_eq!(Header::parse(&[0x60; 40]), None);
        let mut short = vec![0x45, 0, 0, 200];
        short.resize(40, 0);
        assert_eq!(Header::parse(&short), None);
        assert_eq!(
            udp_packets(
                addr([1, 1, 1, 1], 1),
                addr([2, 2, 2, 2], 2),
                &[0; 65535],
                0,
                1400
            ),
            None
        );
    }
}
