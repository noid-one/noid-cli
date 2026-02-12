//! TAP device lifecycle via raw ioctl.
//!
//! create_tap: opens /dev/net/tun, sets IFF_TAP|IFF_NO_PI, TUNSETPERSIST(1)
//! destroy_tap: reopens, TUNSETPERSIST(0)
//! link_up: ioctl SIOCSIFFLAGS with IFF_UP

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::os::unix::io::RawFd;

// ioctl constants
const TUNSETIFF: libc::c_ulong = 0x400454ca;
const TUNSETPERSIST: libc::c_ulong = 0x400454cb;
const IFF_TAP: libc::c_short = 0x0002;
const IFF_NO_PI: libc::c_short = 0x1000;
const SIOCSIFFLAGS: libc::c_ulong = 0x8914;
const SIOCGIFFLAGS: libc::c_ulong = 0x8913;

#[repr(C)]
struct IfReq {
    ifr_name: [u8; libc::IFNAMSIZ],
    ifr_data: [u8; 24], // union, we only use first 2 bytes for flags or first 4 for ifr_flags
}

impl IfReq {
    fn new(name: &str) -> Result<Self> {
        if name.len() >= libc::IFNAMSIZ {
            bail!("interface name too long: {}", name);
        }
        let mut req = Self {
            ifr_name: [0u8; libc::IFNAMSIZ],
            ifr_data: [0u8; 24],
        };
        req.ifr_name[..name.len()].copy_from_slice(name.as_bytes());
        Ok(req)
    }
}

fn open_tun() -> Result<RawFd> {
    let path = CString::new("/dev/net/tun").unwrap();
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        bail!(
            "failed to open /dev/net/tun: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(fd)
}

/// Create a persistent TAP device with the given name.
pub fn create_tap(name: &str) -> Result<()> {
    let fd = open_tun().context("create_tap: open /dev/net/tun")?;

    let mut req = IfReq::new(name)?;
    // Set IFF_TAP | IFF_NO_PI in the ifr_flags field
    let flags = (IFF_TAP | IFF_NO_PI) as u16;
    req.ifr_data[..2].copy_from_slice(&flags.to_ne_bytes());

    let ret = unsafe { libc::ioctl(fd, TUNSETIFF, &req as *const IfReq) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        bail!(
            "TUNSETIFF failed for {}: {}",
            name,
            std::io::Error::last_os_error()
        );
    }

    // Make persistent
    let ret = unsafe { libc::ioctl(fd, TUNSETPERSIST, 1 as libc::c_int) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        bail!(
            "TUNSETPERSIST(1) failed for {}: {}",
            name,
            std::io::Error::last_os_error()
        );
    }

    unsafe { libc::close(fd) };
    Ok(())
}

/// Destroy a persistent TAP device.
pub fn destroy_tap(name: &str) -> Result<()> {
    let fd = open_tun().context("destroy_tap: open /dev/net/tun")?;

    let mut req = IfReq::new(name)?;
    let flags = (IFF_TAP | IFF_NO_PI) as u16;
    req.ifr_data[..2].copy_from_slice(&flags.to_ne_bytes());

    let ret = unsafe { libc::ioctl(fd, TUNSETIFF, &req as *const IfReq) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        // Interface may already be gone — not an error
        eprintln!("TUNSETIFF for destroy of {} failed (may be gone already)", name);
        return Ok(());
    }

    let ret = unsafe { libc::ioctl(fd, TUNSETPERSIST, 0 as libc::c_int) };
    if ret < 0 {
        unsafe { libc::close(fd) };
        bail!(
            "TUNSETPERSIST(0) failed for {}: {}",
            name,
            std::io::Error::last_os_error()
        );
    }

    unsafe { libc::close(fd) };
    Ok(())
}

/// Bring interface up (IFF_UP).
pub fn link_up(name: &str) -> Result<()> {
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        bail!("failed to create socket: {}", std::io::Error::last_os_error());
    }

    let mut req = IfReq::new(name)?;

    // Get current flags
    let ret = unsafe { libc::ioctl(sock, SIOCGIFFLAGS, &mut req as *mut IfReq) };
    if ret < 0 {
        unsafe { libc::close(sock) };
        bail!(
            "SIOCGIFFLAGS failed for {}: {}",
            name,
            std::io::Error::last_os_error()
        );
    }

    // Set IFF_UP
    let mut flags = i16::from_ne_bytes([req.ifr_data[0], req.ifr_data[1]]);
    flags |= libc::IFF_UP as i16;
    req.ifr_data[..2].copy_from_slice(&flags.to_ne_bytes());

    let ret = unsafe { libc::ioctl(sock, SIOCSIFFLAGS, &req as *const IfReq) };
    if ret < 0 {
        unsafe { libc::close(sock) };
        bail!(
            "SIOCSIFFLAGS (UP) failed for {}: {}",
            name,
            std::io::Error::last_os_error()
        );
    }

    unsafe { libc::close(sock) };
    Ok(())
}
