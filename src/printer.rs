//! Talking to the printer: jobs go raw to TCP 9100, status comes from SNMP.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

pub fn send_job(host: &str, job: &[u8]) -> io::Result<()> {
    let addr = (host, 9100)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address for host"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    s.set_write_timeout(Some(Duration::from_secs(120)))?;
    s.write_all(job)?;
    s.flush()?;
    s.shutdown(Shutdown::Write)?;
    // Let the printer finish reading before we drop the connection.
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0u8; 1024];
    while matches!(s.read(&mut buf), Ok(n) if n > 0) {}
    Ok(())
}

#[derive(Debug, Clone)]
pub enum SnmpValue {
    Int(i64),
    Str(String),
    Missing,
}

impl SnmpValue {
    pub fn int(&self) -> Option<i64> {
        match self {
            SnmpValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn text(&self) -> String {
        match self {
            SnmpValue::Int(i) => i.to_string(),
            SnmpValue::Str(s) => s.trim().to_string(),
            SnmpValue::Missing => "?".into(),
        }
    }
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let len = n.to_be_bytes();
        let skip = len.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (len.len() - skip) as u8);
        out.extend_from_slice(&len[skip..]);
    }
    out.extend_from_slice(content);
    out
}

fn ber_int(v: i64) -> Vec<u8> {
    let b = v.to_be_bytes();
    let mut i = 0;
    while i < 7 && ((b[i] == 0 && b[i + 1] < 0x80) || (b[i] == 0xff && b[i + 1] >= 0x80)) {
        i += 1;
    }
    tlv(0x02, &b[i..])
}

fn ber_oid(oid: &str) -> Vec<u8> {
    let arcs: Vec<u64> = oid.trim_start_matches('.').split('.').map(|a| a.parse().unwrap()).collect();
    let mut out = vec![(arcs[0] * 40 + arcs[1]) as u8];
    for &a in &arcs[2..] {
        let mut chunk = vec![(a & 0x7f) as u8];
        let mut rest = a >> 7;
        while rest > 0 {
            chunk.push(0x80 | (rest & 0x7f) as u8);
            rest >>= 7;
        }
        out.extend(chunk.iter().rev());
    }
    tlv(0x06, &out)
}

/// Split one TLV off the front: (tag, content, rest).
fn read_tlv(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, buf) = buf.split_first()?;
    let (&first, mut buf) = buf.split_first()?;
    let len = if first < 0x80 {
        first as usize
    } else {
        let nb = (first & 0x7f) as usize;
        let len = buf.get(..nb)?.iter().fold(0, |v, &b| v << 8 | b as usize);
        buf = &buf[nb..];
        len
    };
    Some((tag, buf.get(..len)?, &buf[len..]))
}

/// SNMPv1 GET of several OIDs in one request.
pub fn snmp_get(host: &str, community: &str, oids: &[String]) -> io::Result<Vec<SnmpValue>> {
    let binds: Vec<u8> = oids.iter().flat_map(|o| tlv(0x30, &[ber_oid(o), vec![0x05, 0x00]].concat())).collect();
    let pdu = [ber_int(0x1760), ber_int(0), ber_int(0), tlv(0x30, &binds)].concat();
    let msg = tlv(0x30, &[ber_int(0), tlv(0x04, community.as_bytes()), tlv(0xa0, &pdu)].concat());

    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    sock.connect((host, 161))?;
    let mut buf = [0u8; 65535];
    let mut n = None;
    for _ in 0..3 {
        sock.send(&msg)?;
        if let Ok(got) = sock.recv(&mut buf) {
            n = Some(got);
            break;
        }
    }
    let n = n.ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no SNMP response"))?;

    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "malformed SNMP response");
    let (_, msg, _) = read_tlv(&buf[..n]).ok_or_else(malformed)?;
    let (_, _version, rest) = read_tlv(msg).ok_or_else(malformed)?;
    let (_, _community, rest) = read_tlv(rest).ok_or_else(malformed)?;
    let (_, pdu, _) = read_tlv(rest).ok_or_else(malformed)?;
    let (_, _id, rest) = read_tlv(pdu).ok_or_else(malformed)?;
    let (_, _err, rest) = read_tlv(rest).ok_or_else(malformed)?;
    let (_, _idx, rest) = read_tlv(rest).ok_or_else(malformed)?;
    let (_, mut binds, _) = read_tlv(rest).ok_or_else(malformed)?;

    let mut out = Vec::new();
    while let Some((_, bind, rest)) = read_tlv(binds) {
        binds = rest;
        let (_, _oid, val) = read_tlv(bind).ok_or_else(malformed)?;
        let (tag, val, _) = read_tlv(val).ok_or_else(malformed)?;
        out.push(match tag {
            // INTEGER, Counter32, Gauge32, TimeTicks
            0x02 | 0x41 | 0x42 | 0x43 => {
                let init = if val.first().is_some_and(|&b| b >= 0x80) { -1 } else { 0 };
                SnmpValue::Int(val.iter().fold(init, |v, &b| v << 8 | b as i64))
            }
            0x04 => SnmpValue::Str(String::from_utf8_lossy(val).into_owned()),
            _ => SnmpValue::Missing,
        });
    }
    // On error SNMPv1 echoes NULLs (Missing); also guard short replies.
    out.resize(oids.len(), SnmpValue::Missing);
    Ok(out)
}

pub struct Supply {
    pub name: String,
    /// Percent remaining, if the printer reports it.
    pub percent: Option<i64>,
}

pub struct Status {
    pub model: String,
    pub state: String,
    pub display: String,
    pub supplies: Vec<Supply>,
}

pub fn status(host: &str) -> io::Result<Status> {
    const SUPPLIES: usize = 4;
    let mut oids: Vec<String> = vec![
        "1.3.6.1.2.1.25.3.2.1.3.1".into(),        // hrDeviceDescr
        "1.3.6.1.2.1.25.3.5.1.1.1".into(),        // hrPrinterStatus
        "1.3.6.1.2.1.43.16.5.1.2.1.1".into(),     // prtConsoleDisplayBufferText
    ];
    for i in 1..=SUPPLIES {
        oids.push(format!("1.3.6.1.2.1.43.11.1.1.6.1.{i}")); // description
        oids.push(format!("1.3.6.1.2.1.43.11.1.1.8.1.{i}")); // max capacity
        oids.push(format!("1.3.6.1.2.1.43.11.1.1.9.1.{i}")); // level
    }
    let v = snmp_get(host, "public", &oids)?;
    let state = match v[1].int() {
        Some(1) => "other",
        Some(2) => "unknown",
        Some(3) => "idle",
        Some(4) => "printing",
        Some(5) => "warming up",
        _ => "?",
    };
    let supplies = (0..SUPPLIES)
        .map(|i| {
            let (max, level) = (v[4 + i * 3].int(), v[5 + i * 3].int());
            Supply {
                name: v[3 + i * 3].text(),
                percent: match (max, level) {
                    (Some(m), Some(l)) if m > 0 && l >= 0 => Some(l * 100 / m),
                    _ => None,
                },
            }
        })
        .collect();
    Ok(Status { model: v[0].text(), state: state.into(), display: v[2].text(), supplies })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ber_encoding() {
        assert_eq!(ber_int(0), vec![2, 1, 0]);
        assert_eq!(ber_int(128), vec![2, 2, 0, 128]);
        assert_eq!(ber_int(-1), vec![2, 1, 0xff]);
        assert_eq!(ber_oid("1.3.6.1.2.1.43"), vec![6, 6, 0x2b, 6, 1, 2, 1, 43]);
        assert_eq!(ber_oid("1.3.6.1.4.1.311"), vec![6, 7, 0x2b, 6, 1, 4, 1, 0x82, 0x37]);
    }
}
