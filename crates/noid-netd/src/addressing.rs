/// Per-VM /30 subnet allocation from 172.16.0.0/16.
///
/// Each VM gets a /30 (4 IPs): network, host, guest, broadcast.
/// index 0 → 172.16.0.0/30 (host .1, guest .2)
/// index 1 → 172.16.0.4/30 (host .5, guest .6)
/// ...


#[derive(Debug, Clone)]
pub struct NetConfig {
    pub tap_name: String,
    pub host_ip: String,
    pub guest_ip: String,
    pub guest_mac: String,
    pub index: u32,
}

pub fn derive_config(index: u32) -> NetConfig {
    let offset = index * 4;
    let hi = (offset >> 8) as u8;
    let lo = (offset & 0xFF) as u8;

    let host_ip = format!("172.16.{}.{}", hi, lo.wrapping_add(1));
    let guest_ip = format!("172.16.{}.{}", hi, lo.wrapping_add(2));
    let guest_mac = format!("AA:FC:00:00:{:02X}:{:02X}", (index >> 8) as u8, (index & 0xFF) as u8);
    let tap_name = format!("noid{}", index);

    NetConfig {
        tap_name,
        host_ip,
        guest_ip,
        guest_mac,
        index,
    }
}

/// Find the lowest unused index.
pub fn allocate_index(used: &[u32]) -> u32 {
    let mut i = 0u32;
    loop {
        if !used.contains(&i) {
            return i;
        }
        i += 1;
    }
}

/// Build kernel `ip=` boot parameter for the guest.
pub fn kernel_ip_param(config: &NetConfig) -> String {
    // ip=<client-ip>:<server-ip>:<gw-ip>:<netmask>:<hostname>:<device>:<autoconf>
    // guest uses host as gateway
    format!(
        "ip={}::{}:255.255.255.252::eth0:off",
        config.guest_ip, config.host_ip
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_config_index_0() {
        let c = derive_config(0);
        assert_eq!(c.tap_name, "noid0");
        assert_eq!(c.host_ip, "172.16.0.1");
        assert_eq!(c.guest_ip, "172.16.0.2");
        assert_eq!(c.guest_mac, "AA:FC:00:00:00:00");
    }

    #[test]
    fn test_derive_config_index_1() {
        let c = derive_config(1);
        assert_eq!(c.tap_name, "noid1");
        assert_eq!(c.host_ip, "172.16.0.5");
        assert_eq!(c.guest_ip, "172.16.0.6");
        assert_eq!(c.guest_mac, "AA:FC:00:00:00:01");
    }

    #[test]
    fn test_derive_config_index_64() {
        let c = derive_config(64);
        assert_eq!(c.tap_name, "noid64");
        assert_eq!(c.host_ip, "172.16.1.1");
        assert_eq!(c.guest_ip, "172.16.1.2");
        assert_eq!(c.guest_mac, "AA:FC:00:00:00:40");
    }

    #[test]
    fn test_allocate_index() {
        assert_eq!(allocate_index(&[]), 0);
        assert_eq!(allocate_index(&[0]), 1);
        assert_eq!(allocate_index(&[0, 1, 3]), 2);
        assert_eq!(allocate_index(&[1, 2]), 0);
    }

    #[test]
    fn test_kernel_ip_param() {
        let c = derive_config(0);
        assert_eq!(
            kernel_ip_param(&c),
            "ip=172.16.0.2::172.16.0.1:255.255.255.252::eth0:off"
        );
    }
}
