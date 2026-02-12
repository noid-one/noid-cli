//! IP address assignment via ioctl SIOCSIFADDR + SIOCSIFNETMASK.
//!
//! We use the ioctl approach rather than raw netlink because it's simpler
//! and more portable. The netlink RTM_NEWADDR path is complex and these
//! ioctls work fine for point-to-point /30 subnets.

use anyhow::{bail, Result};
use std::net::Ipv4Addr;

const SIOCSIFADDR: libc::c_ulong = 0x8916;
const SIOCSIFNETMASK: libc::c_ulong = 0x891c;

#[repr(C)]
struct SockAddrIn {
    sin_family: u16,
    sin_port: u16,
    sin_addr: u32, // network byte order
    sin_zero: [u8; 8],
}

#[repr(C)]
struct IfReqAddr {
    ifr_name: [u8; libc::IFNAMSIZ],
    ifr_addr: SockAddrIn,
}

/// Assign an IPv4 address and netmask to an interface.
pub fn assign_ip(ifname: &str, ip: &str, prefix_len: u8) -> Result<()> {
    let addr: Ipv4Addr = ip.parse().map_err(|e| anyhow::anyhow!("invalid IP {}: {}", ip, e))?;
    let mask = prefix_to_mask(prefix_len);

    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        bail!("failed to create socket: {}", std::io::Error::last_os_error());
    }

    // Set address
    let mut req = make_ifreq_addr(ifname, addr)?;
    let ret = unsafe { libc::ioctl(sock, SIOCSIFADDR, &req as *const IfReqAddr) };
    if ret < 0 {
        unsafe { libc::close(sock) };
        bail!(
            "SIOCSIFADDR failed for {} ({}): {}",
            ifname,
            ip,
            std::io::Error::last_os_error()
        );
    }

    // Set netmask
    req = make_ifreq_addr(ifname, mask)?;
    let ret = unsafe { libc::ioctl(sock, SIOCSIFNETMASK, &req as *const IfReqAddr) };
    if ret < 0 {
        unsafe { libc::close(sock) };
        bail!(
            "SIOCSIFNETMASK failed for {} (/{prefix_len}): {}",
            ifname,
            std::io::Error::last_os_error()
        );
    }

    unsafe { libc::close(sock) };
    Ok(())
}

fn make_ifreq_addr(ifname: &str, addr: Ipv4Addr) -> Result<IfReqAddr> {
    if ifname.len() >= libc::IFNAMSIZ {
        bail!("interface name too long: {}", ifname);
    }
    let mut req = IfReqAddr {
        ifr_name: [0u8; libc::IFNAMSIZ],
        ifr_addr: SockAddrIn {
            sin_family: libc::AF_INET as u16,
            sin_port: 0,
            sin_addr: u32::from_ne_bytes(addr.octets()),
            sin_zero: [0; 8],
        },
    };
    req.ifr_name[..ifname.len()].copy_from_slice(ifname.as_bytes());
    Ok(req)
}

fn prefix_to_mask(prefix_len: u8) -> Ipv4Addr {
    if prefix_len == 0 {
        return Ipv4Addr::new(0, 0, 0, 0);
    }
    if prefix_len >= 32 {
        return Ipv4Addr::new(255, 255, 255, 255);
    }
    let mask: u32 = !0u32 << (32 - prefix_len);
    Ipv4Addr::from(mask.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_to_mask() {
        assert_eq!(prefix_to_mask(30), Ipv4Addr::new(255, 255, 255, 252));
        assert_eq!(prefix_to_mask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_mask(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(prefix_to_mask(32), Ipv4Addr::new(255, 255, 255, 255));
        assert_eq!(prefix_to_mask(0), Ipv4Addr::new(0, 0, 0, 0));
    }
}
