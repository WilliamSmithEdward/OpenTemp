//! Network interfaces via the IP Helper API (64-bit octet counters).

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Instant;

use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    GetIfTable2, IP_ADAPTER_ADDRESSES_LH, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

use crate::collect::RateTracker;
use crate::model::NetSample;

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const AF_UNSPEC: u32 = 0;
const AF_INET: u16 = 2;
const AF_INET6: u16 = 23;

#[derive(Default)]
pub struct Net {
    rates: RateTracker<(u64, bool)>,
    addresses: HashMap<u64, Vec<String>>,
    addresses_at: Option<Instant>,
}

fn wide(s: &[u16]) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}

impl Net {
    pub fn sample(&mut self, now: Instant) -> Vec<NetSample> {
        if self.addresses_at.is_none_or(|t| now.duration_since(t).as_secs() >= 10) {
            self.addresses = adapter_addresses();
            self.addresses_at = Some(now);
        }
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: GetIfTable2 allocates the table; we free it below.
        if unsafe { GetIfTable2(&mut table) }.is_err() || table.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        // SAFETY: the table holds NumEntries rows.
        let rows = unsafe { std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize) };
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            let hardware = flags & 0x01 != 0;
            let filter = flags & 0x02 != 0;
            let connector = flags & 0x04 != 0;
            if !hardware || filter || !connector || row.Type == IF_TYPE_SOFTWARE_LOOPBACK {
                continue;
            }
            // SAFETY: NET_LUID_LH is a plain u64 union.
            let luid = unsafe { row.InterfaceLuid.Value };
            let rx = self.rates.rate(&(luid, false), row.InOctets, now).unwrap_or(0.0);
            let tx = self.rates.rate(&(luid, true), row.OutOctets, now).unwrap_or(0.0);
            let up = row.OperStatus == IfOperStatusUp;
            let mac_len = (row.PhysicalAddressLength as usize).min(row.PhysicalAddress.len());
            let mac = (mac_len > 0).then(|| {
                row.PhysicalAddress[..mac_len].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
            });
            let link = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
            out.push(NetSample {
                key: format!("{luid:016x}"),
                name: wide(&row.Alias),
                description: wide(&row.Description),
                up,
                rx_bps: rx,
                tx_bps: tx,
                rx_total: row.InOctets,
                tx_total: row.OutOctets,
                link_bps: (up && link > 0 && link != u64::MAX).then_some(link),
                addresses: self.addresses.get(&luid).cloned().unwrap_or_default(),
                mac,
            });
        }
        // SAFETY: freeing the table GetIfTable2 allocated.
        unsafe { FreeMibTable(table as *const _) };
        out
    }
}

fn adapter_addresses() -> HashMap<u64, Vec<String>> {
    let mut map = HashMap::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size = 16 * 1024u32;
    for _ in 0..3 {
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let head = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        // SAFETY: buffer of `size` bytes, 8-byte aligned.
        let r = unsafe { GetAdaptersAddresses(AF_UNSPEC, flags, None, Some(head), &mut size) };
        if r == 111 {
            // ERROR_BUFFER_OVERFLOW: `size` now holds the required length.
            continue;
        }
        if r != 0 {
            return map;
        }
        let mut cur = head as *const IP_ADAPTER_ADDRESSES_LH;
        while !cur.is_null() {
            // SAFETY: walking the linked list inside `buf`.
            let a = unsafe { &*cur };
            // SAFETY: NET_LUID_LH is a plain u64 union.
            let luid = unsafe { a.Luid.Value };
            let mut v4 = Vec::new();
            let mut v6 = Vec::new();
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                // SAFETY: linked list inside `buf`.
                let ua = unsafe { &*u };
                let sa = ua.Address.lpSockaddr as *const u8;
                if !sa.is_null() {
                    // SAFETY: the sockaddr is at least as long as its family requires.
                    let family = unsafe { u16::from_ne_bytes([*sa, *sa.add(1)]) };
                    if family == AF_INET {
                        let b = unsafe { std::slice::from_raw_parts(sa.add(4), 4) };
                        v4.push(Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string());
                    } else if family == AF_INET6 {
                        let b = unsafe { std::slice::from_raw_parts(sa.add(8), 16) };
                        let mut o = [0u8; 16];
                        o.copy_from_slice(b);
                        let ip = Ipv6Addr::from(o);
                        // Link-local addresses are noise for most users.
                        if (ip.segments()[0] & 0xffc0) != 0xfe80 {
                            v6.push(ip.to_string());
                        }
                    }
                }
                u = ua.Next;
            }
            v4.extend(v6);
            map.insert(luid, v4);
            cur = a.Next;
        }
        break;
    }
    map
}
